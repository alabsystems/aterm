// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKER STARTED THROUGH THE ONE aterm BINARY against a real aterm --headless.

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use crate::{child_output, headless_boot, launch_isolation};

use headless_boot::{Instance, boot_session, client_command, ctl, ctl_ok, field};

/// The stand-in `claude` (and `codex`, `agy`, `muse`: the same script under
/// each name): `--version` answers at once (the helper runs every fake once
/// that way, `headless_boot::standin`), and so, unrecorded, do agy's quota and
/// model reads (`-p /usage`, `-p /model`), which an agy spawn makes before its
/// launch: recorded, one could stand in for the launch at the test's read. Any
/// other call writes the trusted-folders file its environment names (the
/// retired Gemini CLI's `GEMINI_CLI_TRUSTED_FOLDERS_PATH`, which no worker is
/// handed any more, so empty) to `$HOME/trust.txt`, then its argv NUL-separated to `$HOME/argv.bin`, whole
/// (written aside, then renamed), and exits 0.
const STANDIN: &str = "#!/bin/sh\n\
    case \"$1\" in --version) echo '2.1.0 (Claude Code)'; exit 0 ;; esac\n\
    case \" $* \" in *' -p /usage '*|*' -p /model '*) exit 0 ;; esac\n\
    printf '%s' \"${GEMINI_CLI_TRUSTED_FOLDERS_PATH-}\" > \"$HOME/trust.txt\"\n\
    for a in \"$@\"; do printf '%s\\000' \"$a\"; done > \"$HOME/argv.part\" && \
    mv \"$HOME/argv.part\" \"$HOME/argv.bin\"\n";

/// A wait for something that MUST happen (AGENTS.md: a hang detector).
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// One headless instance whose shell is `shell` and whose harness supervises
/// with `harness` as its `[harness]` table (`None`: the isolation config's,
/// which switches it off), the stand-in first on its PATH, and `rc` the
/// person's startup file (its name in the home, its text) when given.
#[track_caller]
fn boot(tag: &str, shell: &str, harness: Option<&str>, rc: Option<(&str, &str)>) -> Instance {
    boot_with_launch(tag, shell, harness, rc, &[])
}

/// A command-only or explicitly unintegrated boot cannot run the fixture's normal
/// name-resolution probe in session zero. Its stand-ins are laid after boot; a worker's
/// own rc must verify its resolution before drawing the prompt the launcher types at.
fn boot_with_launch(
    tag: &str,
    shell: &str,
    harness: Option<&str>,
    rc: Option<(&str, &str)>,
    extra: &[&str],
) -> Instance {
    boot_with_setup(tag, shell, harness, rc, extra, |_| None)
}

fn boot_with_setup(
    tag: &str,
    shell: &str,
    harness: Option<&str>,
    rc: Option<(&str, &str)>,
    extra: &[&str],
    setup: impl FnOnce(&Path) -> Option<PathBuf>,
) -> Instance {
    boot_configured(tag, shell, harness, rc, extra, |tmp, _| setup(tmp))
}

#[expect(
    clippy::disallowed_types,
    reason = "configures the isolated headless child"
)]
fn boot_configured(
    tag: &str,
    shell: &str,
    harness: Option<&str>,
    rc: Option<(&str, &str)>,
    extra: &[&str],
    setup: impl FnOnce(&Path, &mut std::process::Command) -> Option<PathBuf>,
) -> Instance {
    let deferred = extra
        .iter()
        .any(|arg| matches!(*arg, "-e" | "--no-shell-integration"));
    let inst = headless_boot::boot_with(
        &format!("atws{tag}"),
        |tmp, cmd| {
            if let Some((name, text)) = rc {
                std::fs::write(tmp.join("home").join(name), text).expect("write the rc");
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
            if !deferred {
                for word in ["claude", "codex", "agy", "muse"] {
                    headless_boot::standin(&bin.join(word), STANDIN);
                }
            } else {
                cmd.env("ATERM_SHELL_INTEGRATION_INSTALLED", "inherited")
                    .env("ATERM_EXPECTED_CLAUDE", bin.join("claude"));
            }
            std::fs::create_dir_all(tmp.join("work")).expect("the worker's folder");
            cmd.env("SHELL", shell)
                .env(
                    "PATH",
                    format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
                )
                .env("ATERM_STATE_HOME", tmp.join("state"))
                .env("ATERM_LOG", "info")
                .args(["--lines", "24", "--columns", "100"])
                .args(extra);
            if let Some(shell) = setup(tmp, cmd) {
                cmd.env("SHELL", shell);
            }
        },
        |_| true,
    );
    if deferred {
        for word in ["claude", "codex", "agy", "muse"] {
            headless_boot::standin(&inst.tmp.join("bin").join(word), STANDIN);
        }
    }
    inst
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
    let out = headless_boot::until_served(|| {
        let mut child = cmd.spawn().expect("spawn aterm ctl");
        let mut stdin = child.stdin.take().expect("its stdin");
        stdin.write_all(task).expect("write the task");
        drop(stdin);
        child_output::wait_and_read(child, Instant::now() + headless_boot::CLIENT_EXIT_DEADLINE)
            .expect("aterm ctl exits")
    });
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

/// Never let the worker launcher type a vendor command unless this worker's own shell
/// resolves it to the fixture. The marker proves that guarded shell reached its rc: for bash,
/// `.bashrc`, which integrated bash reads on macOS (after the login files) and on Linux (where
/// it starts as no login shell, as a terminal's bash does there).
const WORKER_RESOLUTION: &str = "if [ -n \"$ATERM_TASK_FILE\" ]; then\n\
    [ \"$(command -v claude)\" = \"$ATERM_EXPECTED_CLAUDE\" ] || exit 86\n\
    printf 'worker shell resolved fixture\\n' > \"$HOME/worker-resolved\"\n\
    fi\n";

/// The worker's sid out of a spawn's `OK <sid>`.
fn sid_of(stdout: &str) -> String {
    stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .unwrap_or_else(|| panic!("a sid: {stdout}"))
        .to_string()
}

fn run_worker(tag: &str, shell: &str, model: Option<&str>) {
    run_agent_worker(tag, shell, model, "claude");
}

/// [`run_worker`] for the worker agent `word` (`spawn agent=<word>`).
fn run_agent_worker(tag: &str, shell: &str, model: Option<&str>, word: &str) {
    let inst = boot(tag, shell, Some(SUPERVISED), None);
    let (_, manager) = boot_session(&inst);
    let work = inst.tmp.join("work");
    let task = hostile_task();
    let model_arg = model.map(|m| format!("model={m}"));
    let agent_arg = format!("agent={word}");
    let mut args = vec!["@self", "spawn", agent_arg.as_str(), "raise=false"];
    args.extend(model_arg.as_deref());
    args.push("-");
    let (ok, stdout, stderr) = spawn_as(&inst, &manager, &work, &args, task.as_bytes());
    assert!(ok, "spawn {agent_arg}: {stdout} {stderr}");
    let worker = sid_of(&stdout);

    let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
    assert!(
        record.starts_with(&format!("OK task rev=1 state=working from={manager} ")),
        "{record}"
    );
    // The window and active columns ride a main-thread hop that a loaded machine can miss (`-`):
    // read the roster until it placed both, then judge the placement.
    let roster = aterm_spec::wait::until_within(HANG, "the roster's placement columns", || {
        let roster = ctl_ok(&inst, &["sessions"]);
        let placed = |sid: &str| {
            roster
                .lines()
                .find(|l| l.split_whitespace().nth(1) == Some(sid))
                .and_then(|row| field(row, "active"))
                .is_some_and(|active| active != "-")
        };
        (placed(&manager) && placed(&worker)).then_some(roster)
    });
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
    let meta = ctl_ok(&inst, &[&format!("@{worker}"), "meta"]);
    assert_eq!(field(&meta, "worker"), Some(word), "{meta}");
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
    assert_eq!(field(&meta, "autopilot"), Some("land"), "{meta}");
    assert_ne!(
        field(
            &ctl_ok(&inst, &[&format!("@{manager}"), "meta"]),
            "autopilot"
        ),
        Some("land"),
        "the manager's word"
    );

    let tasks = inst.tmp.join("state/tasks");
    let file = tasks.join(format!("{worker}-1.txt"));
    let home = inst.tmp.join("home");
    let argv_file = home.join("argv.bin");
    until(&inst, &worker, "the stand-in ran", || argv_file.exists());
    // The world's config says no to installs, so the launcher of an agent atpkg ships installed
    // nothing (no download of the CLI, whether or not an atpkg stands beside this aterm) and its
    // launch line in the instance's log (`$ATERM_STATE_HOME/logs`) named that off beside the
    // program it kept off.
    let cli = aterm_agent::harness::worker::WORKERS
        .iter()
        .find(|cli| cli.word == word)
        .unwrap_or_else(|| panic!("{word}: a worker row"));
    if let Some(program) = cli.atpkg_program() {
        let log = inst.tmp.join("state/logs/aterm.log");
        until(
            &inst,
            &worker,
            "the launch line naming the no to installs",
            || {
                std::fs::read_to_string(&log).is_ok_and(|log| {
                    log.lines().any(|line| {
                        line.contains(atpkg::adoption::UPDATES_OFF) && line.contains(program)
                    })
                })
            },
        );
    }
    let text = std::fs::read(&file).expect("the task file");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&tasks), mode(&file)), (0o700, 0o600));
    }
    let gap = if word == "agy" { "" } else { "\n" };
    let head = format!("{task}\n{gap}");
    let fact = text
        .strip_prefix(head.as_bytes())
        .unwrap_or_else(|| panic!("{word}: the file opens with the task, then {gap:?}"));
    let fact = std::str::from_utf8(fact).expect("utf-8 fact");
    assert_eq!(fact.matches('\n').count(), 1, "one line of fact: {fact:?}");
    assert!(fact.ends_with('\n') && fact.contains(&manager), "{fact:?}");

    let argv = std::fs::read(&argv_file).expect("the argv");
    let argv: Vec<&[u8]> = argv.split(|b| *b == 0).collect();
    let whole = text.strip_suffix(b"\n").expect("the file ends its line");
    let mut want: Vec<&[u8]> = Vec::new();
    // With no starter of this CLI, its worker carries the permission floor before the
    // ordinary launch flags. Keep this oracle independent of the worker's flag builder.
    match word {
        "agy" => want.extend([b"--mode".as_slice(), b"auto".as_slice()]),
        "muse" => want.extend([b"--approval-mode".as_slice(), b"never".as_slice()]),
        _ => {}
    }
    if word == "muse" {
        want.push(b"--trust-workspace");
    }
    want.extend(cli.default_flags.iter().map(|f| f.as_bytes()));
    if let Some(m) = model.or(cli.default_model) {
        want.extend([b"--model".as_slice(), m.as_bytes()]);
    }
    let session = argv
        .iter()
        .position(|a| *a == b"--session-id")
        .and_then(|at| argv.get(at + 1))
        .map(|id| String::from_utf8_lossy(id).into_owned());
    assert_eq!(session, None, "{word}: no session id of the line's");
    let trust = std::fs::read_to_string(home.join("trust.txt")).expect("the stand-in's trust");
    assert_eq!(trust, "", "{word}: no trusted-folders file");
    assert!(
        !tasks.join(format!("{worker}-1.trust.json")).exists(),
        "{word}: no trusted-folders sidecar"
    );
    let attached = [b"--prompt-interactive=".as_slice(), whole].concat();
    match word {
        "agy" => want.extend([attached.as_slice(), b"".as_slice()]),
        _ => want.extend([b"--".as_slice(), whole, b"".as_slice()]),
    }
    assert!(
        argv == want,
        "argv: {} parts, the last {:?}",
        argv.len(),
        argv.last().map(|a| a.len())
    );
    assert!(!home.join("ran").exists(), "the shell ran the task's text");

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

/// THE LIVE WORLDS DECLINE INSTALLS, so a spawn here runs the test's stand-in and never starts a
/// download of the agent CLI: the config every boot writes ([`launch_isolation::prepare`]), and
/// the one this file's supervised boots make of it, say no to unattended installs by atpkg's own
/// reading ([`atpkg::adoption::unattended_install_off`]), which a spawn's install obeys.
#[test]
fn the_live_worlds_config_declines_installs() {
    let root = launch_isolation::scratch_root("atwsconsent");
    let written = std::fs::read_to_string(root.path().join("cfg/aterm/aterm.toml"))
        .expect("the isolation config");
    let supervised = format!(
        "{}[harness]\n{SUPERVISED}",
        written.replace(launch_isolation::HARNESS_OFF, "")
    );
    let store = atpkg::store::Layout {
        prefix: root.path().join("pkg"),
    };
    for (which, text) in [("written", &written), ("supervised", &supervised)] {
        let cfg = atpkg::config::parse_packages(text);
        assert_eq!(
            atpkg::adoption::unattended_install_off(&store, &cfg),
            Some(atpkg::adoption::UPDATES_OFF),
            "{which}: {text}"
        );
    }
}

#[test]
fn a_worker_runs_its_task_as_claudes_argv_in_a_background_tab_under_zsh() {
    run_worker("z", headless_boot::need_zsh(), Some("claude-test.1_a"));
}

#[test]
fn a_worker_runs_its_task_as_claudes_argv_in_a_background_tab_under_bash() {
    run_worker("b", "/bin/bash", None);
}

#[test]
fn an_agy_worker_runs_its_task_as_its_prompt_interactive_argv() {
    run_agent_worker("a", "/bin/bash", None, "agy");
}

#[test]
fn a_codex_worker_runs_its_task_after_the_end_of_options() {
    run_agent_worker("c", "/bin/bash", Some("gpt-x"), "codex");
}

#[test]
fn a_muse_worker_runs_its_task_after_the_end_of_options() {
    run_agent_worker("m", headless_boot::need_zsh(), Some("m-1"), "muse");
}

#[test]
fn a_command_only_instance_spawns_a_worker_without_typing_into_its_command() {
    let inst = boot_with_launch(
        "cmd",
        "/bin/bash",
        Some(SUPERVISED),
        Some((".bashrc", WORKER_RESOLUTION)),
        &[
            "-e",
            "/bin/sh",
            "-c",
            "printf 'parent-guard=%s\\n' \"$ATERM_SHELL_INTEGRATION_INSTALLED\"; exec /bin/cat",
        ],
    );
    let (_, original) = boot_session(&inst);
    let cat_pid = || {
        let tree = headless_boot::process_tree(&inst);
        tree.lines()
            .find(|line| line.split_whitespace().nth(5) == Some("/bin/cat"))
            .and_then(|line| line.split_whitespace().next())
            .map(str::to_owned)
    };
    let original_pid = aterm_spec::wait::until_within(HANG, "the direct command's cat", cat_pid);
    let before = aterm_spec::wait::until_within(HANG, "the direct command's environment", || {
        let text = ctl_ok(&inst, &[&format!("@{original}"), "text", "trim"]);
        text.contains("parent-guard=inherited").then_some(text)
    });
    let work = inst.tmp.join("work");
    let cwd = format!("cwd={}", work.display());
    let (ok, stdout, stderr) = spawn_as(
        &inst,
        &original,
        &work,
        &["spawn", "agent=claude", &cwd, "raise=false", "-"],
        b"a task for the new worker only",
    );
    assert!(ok, "spawn from a command-only instance: {stdout} {stderr}");
    let worker = sid_of(&stdout);
    until(
        &inst,
        &worker,
        "the fixture ran from the worker's shell",
        || inst.tmp.join("home/argv.bin").exists(),
    );
    assert!(inst.tmp.join("home/worker-resolved").exists());
    let worker_status = headless_boot::status(&inst, &worker);
    assert_eq!(
        field(&worker_status, "integration"),
        Some("on"),
        "{worker_status}"
    );
    let original_status = headless_boot::status(&inst, &original);
    assert_eq!(
        field(&original_status, "integration"),
        Some("off"),
        "{original_status}"
    );
    assert_eq!(
        cat_pid(),
        Some(original_pid),
        "the original command was not replaced"
    );
    assert_eq!(
        ctl_ok(&inst, &[&format!("@{original}"), "text", "trim"]),
        before,
        "no task or shell command was typed into cat"
    );
    let roster = aterm_spec::wait::until_within(HANG, "the original tab's placement", || {
        let roster = ctl_ok(&inst, &["sessions"]);
        let placed = roster
            .lines()
            .find(|line| line.split_whitespace().nth(1) == Some(original.as_str()))
            .and_then(|row| field(row, "active"))
            .is_some_and(|active| active != "-");
        placed.then_some(roster)
    });
    let original_row = roster
        .lines()
        .find(|line| line.split_whitespace().nth(1) == Some(original.as_str()))
        .expect("the original session remains");
    assert_eq!(field(original_row, "active"), Some("1"), "{roster}");
}

#[test]
fn an_explicit_integration_opt_out_refuses_workers_even_with_a_command() {
    for (tag, extra) in [
        ("off", &["--no-shell-integration"][..]),
        ("cmdoff", &["--no-shell-integration", "-e", "/bin/cat"][..]),
    ] {
        let inst = boot_with_launch(tag, "/bin/bash", Some(SUPERVISED), None, extra);
        let (_, original) = boot_session(&inst);
        let before = ctl_ok(&inst, &["sessions"]).lines().count();
        let (ok, stdout, stderr) = spawn_as(
            &inst,
            &original,
            &inst.tmp.join("work"),
            &["spawn", "agent=claude", "raise=false", "-"],
            b"must not start",
        );
        assert!(
            !ok && stderr.contains("shell integration is off"),
            "{stdout} {stderr}"
        );
        assert_eq!(ctl_ok(&inst, &["sessions"]).lines().count(), before);
        assert!(!inst.tmp.join("home/argv.bin").exists());
        assert!(
            std::fs::read_dir(inst.tmp.join("state/tasks"))
                .map_or(true, |mut entries| entries.next().is_none()),
            "no task was booked"
        );
    }
}

/// The retired Gemini CLI (`agent=gemini`, `agent=gemini-cli`) is refused by
/// name with agy named in its place, on a supervised instance where any
/// other agent would start, and nothing is spawned: never another agent.
#[test]
fn a_spawn_of_the_retired_gemini_cli_is_refused_and_spawns_nothing() {
    let inst = boot("r", "/bin/bash", Some(SUPERVISED), None);
    let (_, manager) = boot_session(&inst);
    let work = inst.tmp.join("work");
    let rows = || ctl_ok(&inst, &["sessions"]).lines().count();
    let before = rows();
    for word in ["gemini", "gemini-cli"] {
        let agent = format!("agent={word}");
        let (ok, stdout, stderr) = spawn_as(
            &inst,
            &manager,
            &work,
            &["@self", "spawn", &agent, "-"],
            b"a task",
        );
        assert!(
            !ok && stderr.contains("Gemini CLI is no longer supported: agy replaces it"),
            "{word}: {stdout} {stderr}"
        );
    }
    assert_eq!(rows(), before, "nothing was spawned");
    assert!(!inst.tmp.join("home/argv.bin").exists(), "no stand-in ran");
}

/// With the window harness off nothing would finish a worker's task: `ERR
/// unsupervised:` naming the switch, and nothing is spawned (the other
/// switches that refuse it: `harness_host::worker_unsupervised`'s test).
#[test]
fn a_worker_is_refused_unsupervised_and_spawns_nothing() {
    let inst = boot("u", headless_boot::login_shell().path, None, None);
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
        !ok && stderr.contains("ERR unsupervised: [harness] enabled = false"),
        "{stderr}"
    );
    let (ok, _, stderr) = spawn_as(&inst, &manager, &work, &["spawn", "agent=claude", "-"], b"");
    assert!(
        !ok && stderr.contains("the task on stdin is empty"),
        "{stderr}"
    );
    assert_eq!(rows(), before, "nothing was spawned");
    let raw = ctl(&inst, &["spawn", "agent=claude"]);
    assert!(
        String::from_utf8_lossy(&raw.stderr).contains("end the request with -"),
        "{raw:?}"
    );
}

/// A worker whose shell ends before its first prompt — the person's rc file
/// ends it (a worker's shell alone: the one `ATERM_TASK_FILE` names a task to)
/// — has its task `stalled why=shell`, and nothing was typed: the stand-in
/// never ran.
#[test]
fn a_worker_whose_shell_draws_no_prompt_is_stalled_and_nothing_is_typed() {
    let shell = headless_boot::login_shell();
    let inst = boot(
        "s",
        shell.path,
        Some(SUPERVISED),
        Some((shell.rc, "[ -n \"$ATERM_TASK_FILE\" ] && exit 3\n")),
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

/// A worker whose shell is slow to its first prompt, as a loaded machine makes one (its rc file
/// holds it here), has not failed: its task stays `working` and the prompt takes its line.
#[test]
fn a_worker_whose_first_prompt_comes_late_still_starts() {
    let shell = headless_boot::login_shell();
    let inst = boot_configured(
        "late",
        shell.path,
        Some(SUPERVISED),
        Some((
            shell.rc,
            "[ -n \"$ATERM_TASK_FILE\" ] && \
             while [ ! -e \"$HOME/prompt-cue\" ]; do sleep 0.1; done\n",
        )),
        &[],
        |tmp, cmd| {
            let clock = tmp.join("prompt-clock");
            std::fs::create_dir(&clock).expect("the prompt clock");
            cmd.env("ATERM_DEBUG_PROMPT_CLOCK", clock);
            None
        },
    );
    let (_, manager) = boot_session(&inst);
    let work = inst.tmp.join("work");
    let (ok, stdout, stderr) = spawn_as(
        &inst,
        &manager,
        &work,
        &["spawn", "agent=claude", "-"],
        b"a task its shell is slow to reach",
    );
    assert!(ok, "spawn agent=claude: {stdout} {stderr}");
    let worker = sid_of(&stdout);
    let clock = inst.tmp.join("prompt-clock");
    until(
        &inst,
        &worker,
        "the launcher is waiting for its prompt",
        || clock.join(&worker).with_extension("seen").exists(),
    );
    crate::turn_submit_live_headless::write_clock(&clock.join(&worker), "25000");
    until(&inst, &worker, "the launcher judged the late clock", || {
        std::fs::read_to_string(clock.join(&worker).with_extension("seen"))
            .is_ok_and(|seen| seen == "25000")
    });
    let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
    assert!(
        record.contains(" state=working "),
        "its launcher waits on: {record}"
    );
    std::fs::write(inst.tmp.join("home/prompt-cue"), b"").expect("write the cue");
    until(&inst, &worker, "the late prompt took the line", || {
        inst.tmp.join("home/argv.bin").exists()
    });
}

/// A PROGRAM WORKER CONCLUDES ON ITS EXIT STATUS, ITS OUTPUT TAIL ITS RESULT: `spawn
/// program=/bin/sh -` hands the task file to the shell as its script, verbatim (no fact line,
/// which would be input to it), in a background tab under its manager; exit 0 is `done
/// why=exit-status exit=0` and exit 3 `failed why=exit-status exit=3`, each on `task` and on an
/// `await task` row, and `result` answers `source=output` with what it printed. NEGATIVE
/// CONTROLS: a spawn naming no program, or one a harness here concludes, is refused and spawns
/// nothing.
#[test]
fn a_program_worker_concludes_on_its_exit_status_with_its_output_as_its_result() {
    // This is the worker-result protocol, using one integrated shell. Bash's own
    // setpgid race must not contaminate the program's exact output; its job-control
    // behavior is covered by foreground_handback_live_headless.
    let inst = boot("p", headless_boot::need_zsh(), Some(SUPERVISED), None);
    let (_, manager) = boot_session(&inst);
    let work = inst.tmp.join("work");
    for (task, state, conclusion, exit, said) in [
        ("echo built it\nexit 0\n", "done", "finished", 0, "built it"),
        ("echo it broke\nexit 3\n", "failed", "failed", 3, "it broke"),
    ] {
        let (ok, stdout, stderr) = spawn_as(
            &inst,
            &manager,
            &work,
            &["@self", "spawn", "program=/bin/sh", "raise=false", "-"],
            task.as_bytes(),
        );
        assert!(ok, "spawn program=/bin/sh: {stdout} {stderr}");
        let worker = sid_of(&stdout);
        let file = inst.tmp.join(format!("state/tasks/{worker}-1.txt"));
        assert_eq!(
            std::fs::read_to_string(&file).expect("the task file"),
            task,
            "the task as given"
        );
        let meta = ctl_ok(&inst, &[&format!("@{worker}"), "meta"]);
        assert_eq!(field(&meta, "worker"), Some("generic"), "{meta}");
        until(&inst, &worker, "its program's end", || {
            ctl_ok(&inst, &[&format!("@{worker}"), "task"]).contains(&format!(" state={state} "))
        });
        let record = ctl_ok(&inst, &[&format!("@{worker}"), "task"]);
        assert!(
            record.contains(&format!(" conclusion={conclusion} why=exit-status "))
                && record.contains(&format!(" exit={exit} ")),
            "{record}"
        );
        let row = ctl_ok(
            &inst,
            &[
                "await",
                "task",
                &format!("@{worker}"),
                "since=0",
                "timeout=1000",
            ],
        );
        assert!(
            row.contains(&format!(
                " conclusion={conclusion} why=exit-status exit={exit}"
            )),
            "{row}"
        );
        // `result` prints the reply's status line on stderr and its body on stdout.
        let result = ctl(&inst, &[&format!("@{worker}"), "result"]);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(String::from_utf8_lossy(&result.stdout), format!("{said}\n"));
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains(&format!("OK 1 source=output bytes={}", said.len())),
            "{result:?}"
        );
    }
    let rows = || ctl_ok(&inst, &["sessions"]).lines().count();
    let before = rows();
    for (program, refused) in [
        ("program=", "ERR usage: spawn"),
        (
            "program=claude",
            "has a harness here; spawn it as `agent=claude`",
        ),
    ] {
        let (ok, stdout, stderr) = spawn_as(
            &inst,
            &manager,
            &work,
            &["@self", "spawn", program, "-"],
            b"exit 0\n",
        );
        assert!(
            !ok && stderr.contains(refused),
            "{program}: {stdout} {stderr}"
        );
    }
    assert_eq!(rows(), before, "nothing was spawned");
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
