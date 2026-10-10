// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real [`crate::release::Backend`]: what `targo --unverified ship release` asks of
//! git, the publication engine, the receipt stores, the release cache, the children it
//! runs (the trust lane, the one build, the MEASURE run, `pub stage`/`pub publish`, the
//! cut, the site hook), the channel and the owner's aterm.
//!
//! Every child is built with the release's ONE build environment: each variable the
//! gate records on a receipt's `build-env` line (`aterm_verify::differential::
//! build_env_records`) and `CARGO_TARGET_DIR`/`CARGO_BUILD_TARGET_DIR` are removed —
//! no caller's `RUSTFLAGS`, wrapper or profile reaches the trust lane, MEASURE's
//! compiles or the public gate `pub stage` runs — `TRUST_STAGE2_BIN` names the release's prover
//! and `PUBLICATION_ENGINE` the one engine the release judges (`--engine`), so the cut
//! and `pub publish`'s pre-promote read the ledger the release read. Every child the
//! release waits on that can take long — a step's, the preflight's, git, gh or curl
//! talking to a remote — starts through ONE spawn path ([`Real::spawn_forwarded`]): in
//! a session and process group of its own with no controlling terminal ([`detach`]
//! says why), so it can never stop on the terminal, and an interrupt is handed to its
//! group (never to a trustd in it) while the release waits for it. A step's output
//! goes to the step's log (`~/.local/state/aterm-release/logs/<stamp>-<pid>/
//! <nn>-<step>.log`), and to the terminal too, as it arrives, under `--verbose`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::ledger::{GitCli, GitRunner};
use crate::provision::{AskWords, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Candidate, CutState, EngineRows, Facts, Prover,
    RELEASE_COMMAND, RUNNING_CHILD_FILE, Ran, Refusal, Reservation, Step,
};

/// Where the preflight keeps what it leaves behind (the engine dry run's scratch clone
/// and log, the wasm32 check's source copy, target directory and log): `preflight/` of
/// the clone's ONE release cache, `<clone>-release.noindex` ([`crate::snapshot::
/// CacheRoot`]) — outside the clone, so nothing the preflight does makes the tree
/// dirty; persistent, so a re-run is warm; private and `.noindex`, so Spotlight never
/// indexes the source copies.
fn preflight_cache(repo: &Path) -> Result<PathBuf, String> {
    let dir = crate::snapshot::CacheRoot::for_repo(repo)?.preflight();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Every input `tools/trust-gate-all.sh` reads from its environment, pinned empty (the
/// script's default) for the release's lane: an exported one would narrow the run,
/// move its memory record, aim its trustd probe at another socket, shorten its wait,
/// turn off its refused-path check, or move how often it samples the load.
const TRUST_LANE_INPUTS: [&str; 13] = [
    "ATERM_GATE_ONLY",
    "ATERM_GATE_RATCHET",
    "ATERM_GATE_FORKS",
    "ATERM_GATE_SCOPE",
    "ATERM_GATE_MEMORY",
    "ATERM_GATE_HOST_MEMORY",
    "ATERM_GATE_LOGDIR",
    "ATERM_GATE_COSTS",
    "ATERM_GATE_LOAD_EVERY",
    "ATERM_GATE_MACHINE",
    "ATERM_GATE_TRUSTD",
    "ATERM_GATE_TRUSTD_WAIT",
    "ATERM_GATE_NO_PATH_CHECK",
];

/// THE INTERRUPT: SIGINT (Ctrl-C), SIGTERM (`kill <pid>`) and SIGHUP (a closed
/// terminal) are caught, never fatal — each one this process did not inherit IGNORED:
/// under `nohup` (or a background `&` of a shell without job control) the release keeps
/// SIG_IGN, as every program that respects it does (`if (signal(sig, SIG_IGN) !=
/// SIG_IGN) signal(sig, handler)`), and its children inherit it.
///
/// The handler only records (atomics: async-signal-safe). While a step's child runs, a
/// forwarder thread ([`Running`]) hands the first signal to every process of the
/// child's group and a second one SIGKILL to them — every process but a Trust `trustd`
/// and what it runs ([`group_members`]). A trustd shares no group with its launcher
/// from trust `41f912aa5f4` (2026-09-22) on, but the store seal of 2026-10 (321aaeda7)
/// predates it, so a trustd a step's `targo trust check` starts (`pub stage`'s public
/// clone gate runs one) lives in that step's group — and a signalled trustd records
/// DIRTY, after which targo starts none until an operator attests no solver survives
/// (AGENTS.md: never signal a trustd). So no signal goes to a group as a whole. The
/// release then sees the step end, lets its lock go and says its INTERRUPTED line.
#[cfg(unix)]
pub(crate) mod interrupt {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

    pub const SIGHUP: i32 = 1;
    pub const SIGINT: i32 = 2;
    pub const SIGKILL: i32 = 9;
    pub const SIGTERM: i32 = 15;
    /// `SIG_IGN` as `signal` takes and answers it.
    const SIG_IGN: usize = 1;

    /// The first signal caught (0: none).
    static SIGNAL: AtomicI32 = AtomicI32::new(0);
    /// How many were caught.
    static COUNT: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" {
        fn signal(sig: i32, handler: usize) -> usize;
        fn kill(pid: i32, sig: i32) -> i32;
    }

    extern "C" fn on_signal(sig: i32) {
        let _ = SIGNAL.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
        COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// Catch SIGINT, SIGTERM and SIGHUP for the release's life — except a signal this
    /// process was started ignoring, which stays ignored.
    pub fn install() {
        let handler = on_signal as extern "C" fn(i32) as usize;
        for sig in [SIGHUP, SIGINT, SIGTERM] {
            // SAFETY: `signal` takes an integer and a handler value: SIG_IGN, or
            // `on_signal`, an `extern "C" fn(i32)` that lives for the whole program and
            // does only async-signal-safe work (two atomics).
            unsafe {
                if signal(sig, SIG_IGN) != SIG_IGN {
                    signal(sig, handler);
                }
            }
        }
    }

    /// The signal that interrupted the release, if one did.
    pub fn caught() -> Option<i32> {
        let sig = SIGNAL.load(Ordering::SeqCst);
        (sig != 0).then_some(sig)
    }

    /// `sig` to `pid` (a member of a step's group, never a group as a whole).
    pub fn send(pid: i32, sig: i32) {
        // SAFETY: `kill` takes two integers and touches no memory of ours.
        unsafe {
            kill(pid, sig);
        }
    }

    /// The running step's child, for as long as this value lives: a forwarder thread
    /// polls the signals caught (every 50 ms) and hands them on — so a signal that
    /// arrived between the spawn and this is handed on at its first look.
    pub struct Running {
        done: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Running {
        /// `leader` is the child's pid, which leads its own session and group.
        pub fn enter(leader: u32) -> Self {
            let done = Arc::new(AtomicBool::new(false));
            let thread = i32::try_from(leader).ok().map(|group| {
                let done = Arc::clone(&done);
                std::thread::spawn(move || forward(group, &done))
            });
            Self { done, thread }
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            self.done.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn forward(group: i32, done: &AtomicBool) {
        let mut handed = 0u32;
        while !done.load(Ordering::SeqCst) {
            let caught = COUNT.load(Ordering::SeqCst);
            if caught > handed {
                if handed == 0 {
                    signal_group(group, SIGNAL.load(Ordering::SeqCst));
                    handed = 1;
                } else {
                    signal_group(group, SIGKILL);
                    handed = caught;
                }
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// `sig` to every member of `group` that is not a trustd's ([`super::group_members`]);
    /// SIGKILL is sent twice, the group listed again between, for what forked meanwhile.
    /// A listing that cannot be read signals the group's leader alone: never a trustd.
    fn signal_group(group: i32, sig: i32) {
        let passes = if sig == SIGKILL { 2 } else { 1 };
        for _ in 0..passes {
            let listing = std::process::Command::new("/bin/ps")
                .args(["-A", "-o", "pid=,ppid=,pgid=,comm="])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let members = listing.map_or_else(|| vec![group], |l| super::group_members(&l, group));
            for pid in members {
                send(pid, sig);
            }
        }
    }
}

#[cfg(not(unix))]
pub(crate) mod interrupt {
    pub fn install() {}
    pub fn caught() -> Option<i32> {
        None
    }
    pub struct Running;
    impl Running {
        pub fn enter(_leader: u32) -> Self {
            Self
        }
    }
}

/// The members of process group `group` in a `ps -A -o pid=,ppid=,pgid=,comm=` listing
/// that a release's interrupt may signal: every one but a Trust `trustd` (its command's
/// last path component) and every descendant of one — the solvers it runs. A trustd is
/// never signalled (the module above says why); it outlives the step by design.
#[must_use]
pub fn group_members(listing: &str, group: i32) -> Vec<i32> {
    // (pid, ppid, pgid, comm), the command being the rest of the line (paths hold spaces)
    let rows: Vec<(i32, i32, i32, &str)> = listing
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut num = || -> Option<i32> {
                let end = rest.find(char::is_whitespace)?;
                let n = rest[..end].parse().ok()?;
                rest = rest[end..].trim_start();
                Some(n)
            };
            let (pid, ppid, pgid) = (num()?, num()?, num()?);
            Some((pid, ppid, pgid, rest.trim_end()))
        })
        .collect();
    let mut spared: std::collections::BTreeSet<i32> = rows
        .iter()
        .filter(|(_, _, _, comm)| comm.rsplit('/').next() == Some("trustd"))
        .map(|(pid, ..)| *pid)
        .collect();
    loop {
        let before = spared.len();
        for (pid, ppid, ..) in &rows {
            if spared.contains(ppid) {
                spared.insert(*pid);
            }
        }
        if spared.len() == before {
            break;
        }
    }
    rows.iter()
        .filter(|(pid, _, pgid, _)| *pgid == group && !spared.contains(pid))
        .map(|(pid, ..)| *pid)
        .collect()
}

/// A child the release waits on, made unable to block on a terminal: git's own prompt
/// off, and — on Unix — the child the leader of a NEW SESSION (`setsid`), which also
/// makes it the leader of its own process group, the group the interrupt forwards to.
///
/// WHY A SESSION, NOT THE TERMINAL'S FOREGROUND. A child in a background process group
/// of the release's session that reads its terminal (ssh asking for a key passphrase
/// during a `git push`) is stopped by SIGTTIN, and the release's `wait` hangs with
/// nothing said. A child with no controlling terminal cannot be: `/dev/tty` does not
/// open (ENXIO), so ssh fails at once, and its words land in the step's log and the
/// refusal. Handing the child the terminal's foreground instead (`tcsetpgrp`, as a
/// shell does) would let it prompt — but the release runs unattended for hours with
/// the step's output in its log, so a prompt is a silent wait all the same; it would
/// take the operator's Ctrl-C and Ctrl-Z away from the release (which would then have
/// to infer an interrupt from the child's status, and stop and continue the child as
/// a job-control shell does); and it does nothing for a release that is a background
/// job or under `nohup`, whose terminal is not its own to hand over. A session works
/// the same in every one of those. stdin is the caller's to set (`/dev/null` for
/// every step), and a hangup reaches the child only through the release.
fn detach(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        unsafe extern "C" {
            fn setsid() -> i32;
        }
        // SAFETY: the closure runs in the forked child before its exec and calls only
        // `setsid`, which is async-signal-safe and touches no memory of ours.
        unsafe {
            cmd.pre_exec(|| {
                if setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// A pid's start time (`ps -o lstart=`), `None` when no such process lives.
fn started_of(pid: u32) -> Option<String> {
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// The real backend.
pub struct Real {
    repo: PathBuf,
    home: PathBuf,
    engine: PathBuf,
    state: PathBuf,
    logs: PathBuf,
    /// `<git common dir>/aterm-release`: the journal, the run claim, the child's note.
    release: PathBuf,
    /// A dry run's logs are scratch, removed when it ends.
    scratch_logs: bool,
    verbose: bool,
    origin_tags: String,
    /// The local UTC offset, in seconds, read once (`date +%z`).
    offset: i64,
    lock: Option<aterm_verify::snapshot::MachineHold>,
    /// The step the run is at ([`Backend::entered`]): what a child is noted under.
    step: Step,
    /// This process's arguments (after the program) with every path flag's value
    /// anchored ([`crate::release::ReleaseOptions::anchored_args`]): what the re-exec
    /// as R's own cutter passes on, from the clone's root.
    argv: Vec<std::ffi::OsString>,
}

/// A forwarded child's guard ([`Real::spawn_forwarded`]): while it lives, the interrupt
/// is handed to the child's group and [`RUNNING_CHILD_FILE`] names the child.
struct Forwarded {
    _running: interrupt::Running,
    note: Option<PathBuf>,
}

impl Drop for Forwarded {
    fn drop(&mut self) {
        if let Some(note) = &self.note {
            let _ = std::fs::remove_file(note);
        }
    }
}

impl Drop for Real {
    fn drop(&mut self) {
        if self.scratch_logs {
            let _ = std::fs::remove_dir_all(&self.logs);
        }
    }
}

impl Real {
    /// The backend for the clone at `repo`.
    ///
    /// # Errors
    /// No HOME, or a log directory that cannot be made.
    pub fn new(
        repo: &Path,
        engine: Option<&Path>,
        dry: bool,
        verbose: bool,
    ) -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let engine = engine.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("PUBLICATION_ENGINE")
                .filter(|v| !v.is_empty())
                .map_or_else(|| atpkg::devroot::publication_engine(&home), PathBuf::from)
        });
        // a relative engine names one place, the one it named where the release started
        let engine = std::env::current_dir().map_or_else(
            |_| engine.clone(),
            |cwd| crate::release::anchor(&engine, &cwd),
        );
        let state = home.join(".local/state/aterm-release");
        let stamp = Command::new("date")
            .arg("+%Y%m%dT%H%M%S")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| now().to_string());
        let logs = if dry {
            // one scratch directory per backend, removed with it
            static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::env::temp_dir().join(format!("aterm-release-dry-{}-{nth}", std::process::id()))
        } else {
            state
                .join("logs")
                .join(format!("{stamp}-{}", std::process::id()))
        };
        std::fs::create_dir_all(&logs).map_err(|e| format!("create {}: {e}", logs.display()))?;
        let offset = Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| parse_offset(String::from_utf8_lossy(&o.stdout).trim()))
            .unwrap_or(0);
        let release = GitCli::new(repo)
            .git(&["rev-parse", "--path-format=absolute", "--git-common-dir"])
            .ok()
            .filter(crate::ledger::RunOut::success)
            .map_or_else(
                || repo.join(".git"),
                |out| PathBuf::from(out.stdout_utf8().trim()),
            )
            .join("aterm-release");
        Ok(Self {
            repo: repo.to_path_buf(),
            home,
            engine,
            state,
            logs,
            release,
            scratch_logs: dry,
            verbose,
            origin_tags: String::new(),
            offset,
            lock: None,
            step: Step::Checks,
            argv: std::env::args_os().skip(1).collect(),
        })
    }

    fn git(&self) -> GitCli {
        GitCli::new(&self.repo)
    }

    fn git_out(&self, args: &[&str]) -> Option<String> {
        let out = self.git().git(args).ok()?;
        out.success().then(|| out.stdout_utf8().trim().to_string())
    }

    /// `git -C <clone>` that talks to origin (a fetch, `ls-remote`): forwarded.
    fn remote_git(&self, args: &[&str]) -> std::io::Result<std::process::Output> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo).args(args);
        self.output_forwarded(&mut cmd)
    }

    /// `git -C <engine>` (its fetch and push among them): forwarded.
    fn engine_git(&self, args: &[&str]) -> Option<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.engine).args(args);
        let out = self.output_forwarded(&mut cmd).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// THE ONE SPAWN PATH for a child the release waits on: an interrupted release
    /// starts none (`ErrorKind::Interrupted`); the child is [`detach`]ed (its own
    /// session and group, no terminal), named in [`RUNNING_CHILD_FILE`] while it runs
    /// (a real run), and the interrupt is handed to its group
    /// ([`interrupt::Running`]) until the guard returned with it drops.
    fn spawn_forwarded(
        &self,
        cmd: &mut Command,
    ) -> std::io::Result<(std::process::Child, Forwarded)> {
        if interrupt::caught().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: the release was interrupted",
            ));
        }
        detach(cmd);
        let child = cmd.spawn()?;
        let running = interrupt::Running::enter(child.id());
        let note = self.note_child(child.id());
        Ok((
            child,
            Forwarded {
                _running: running,
                note,
            },
        ))
    }

    /// [`Real::spawn_forwarded`] as `Command::output` has it: stdin empty, both
    /// streams captured.
    fn output_forwarded(&self, cmd: &mut Command) -> std::io::Result<std::process::Output> {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, forwarded) = self.spawn_forwarded(cmd)?;
        let out = child.wait_with_output();
        drop(forwarded);
        out
    }

    fn slug(&self) -> String {
        crate::publish::workspace_channel_slug(&self.repo)
            .unwrap_or_else(|_| "alabsystems/aterm".to_string())
    }

    /// `program` with the release's environment: the build-steering variables and the
    /// launcher's own removed ([`scrub`]), the one engine named.
    fn scrubbed(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        scrub(&mut cmd);
        cmd.env("PUBLICATION_ENGINE", &self.engine);
        cmd
    }

    /// A child with the release's build environment, from the clone.
    fn child(&self, program: impl AsRef<std::ffi::OsStr>, prover: Option<&Prover>) -> Command {
        let mut cmd = self.scrubbed(program);
        cmd.current_dir(&self.repo).stdin(Stdio::null());
        if let Some(p) = prover {
            cmd.env("TRUST_STAGE2_BIN", &p.bin);
        }
        cmd
    }

    /// The cut's command: R's own cutter (this binary — it re-ran itself as R's), the
    /// `cut` verb, from this clone, with the release's prover and engine.
    fn cut_command(&self, args: &[String], prover: &Prover) -> Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aterm-release"));
        let mut cmd = self.child(exe, Some(prover));
        cmd.args(args);
        cmd
    }

    /// Run `cmd` as a step's child, through [`Real::spawn_forwarded`] — its output
    /// into `step`'s log (and, as it arrives, to the terminal under `--verbose`). An
    /// interrupted release starts no child.
    pub(crate) fn logged(&mut self, step: Step, cmd: &mut Command) -> Ran {
        let log = self.log_path(step);
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            Ok(f) => f,
            Err(e) => {
                return Ran {
                    code: -1,
                    detail: vec![format!("cannot open {}: {e}", log.display())],
                };
            }
        };
        let _ = writeln!(file, "$ {cmd:?}");
        let not_started = || Ran {
            code: 130,
            detail: vec!["not started: the release was interrupted".into()],
        };
        if interrupt::caught().is_some() {
            return not_started();
        }
        cmd.stdin(Stdio::null());
        // `--verbose`: ONE pipe for both streams, as `2>&1 | tee` had it — every line
        // reaches the log and the terminal as it is written, in the order written.
        let pipe = if self.verbose {
            match std::io::pipe().and_then(|(r, w)| Ok((r, w.try_clone()?, w))) {
                Ok((reader, out, err)) => {
                    cmd.stdout(out).stderr(err);
                    Some(reader)
                }
                Err(e) => {
                    let _ = writeln!(file, "pipe: {e}");
                    return Ran {
                        code: -1,
                        detail: vec![format!("cannot make a pipe: {e}")],
                    };
                }
            }
        } else {
            let (Ok(out), Ok(err)) = (file.try_clone(), file.try_clone()) else {
                return Ran {
                    code: -1,
                    detail: vec![format!("cannot write {}", log.display())],
                };
            };
            cmd.stdout(out).stderr(err);
            None
        };
        let spawned = self.spawn_forwarded(cmd);
        // the command's own copies of the pipe's write end go, so the reader sees the
        // end of the output when the child (and whatever it left holding it) is done
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let (mut child, forwarded) = match spawned {
            Ok(spawned) => spawned,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return not_started(),
            Err(e) => {
                let _ = writeln!(file, "spawn: {e}");
                return Ran {
                    code: -1,
                    detail: vec![format!("spawn: {e}")],
                };
            }
        };
        let pumped = pipe.and_then(|reader| {
            let mut log = file.try_clone().ok()?;
            let (done, finished) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                let mut reader = reader;
                let mut buf = [0u8; 8192];
                while let Ok(n) = std::io::Read::read(&mut reader, &mut buf) {
                    if n == 0 {
                        break;
                    }
                    let _ = log.write_all(&buf[..n]);
                    let _ = std::io::stdout().write_all(&buf[..n]);
                }
                let _ = done.send(());
            });
            Some(finished)
        });
        let code = child.wait().map_or(-1, |s| s.code().unwrap_or(-1));
        // the last of its output, bounded: a daemon the child left may hold the pipe
        if let Some(finished) = pumped {
            let _ = finished.recv_timeout(std::time::Duration::from_secs(5));
        }
        drop(forwarded);
        Ran {
            code,
            detail: tail(&log, 8),
        }
    }

    /// [`RUNNING_CHILD_FILE`]: the step's child, while it runs (a real run only); the
    /// note's path, for the guard that removes it.
    fn note_child(&self, pid: u32) -> Option<PathBuf> {
        if self.scratch_logs {
            return None;
        }
        let started = started_of(pid).unwrap_or_default();
        let note = self.release.join(RUNNING_CHILD_FILE);
        let _ = std::fs::create_dir_all(&self.release);
        let _ = std::fs::write(
            &note,
            format!("pid {pid}\nstarted {started}\nstep {}\n", self.step.name()),
        );
        Some(note)
    }

    fn step_log_text(&mut self, step: Step) -> String {
        std::fs::read_to_string(self.log_path(step)).unwrap_or_default()
    }

    /// THE ENGINE AT ITS OWN origin/main, its appended audit rows kept (the pub-sync
    /// pattern): `pub stage` refuses an engine that is not exactly its origin/main, and
    /// every dry run and stage appends a row to the tracked `verification-log.jsonl`.
    fn engine_sync(&mut self) -> Result<(), Refusal> {
        let e = self.engine.display().to_string();
        if self.engine_git(&["fetch", "-q", "origin"]).is_none() {
            return Err(Refusal::new(
                format!("cannot fetch the engine's origin ({e})"),
                format!("check the network and git -C {e} remote -v"),
            ));
        }
        if self.engine_dirty() {
            return Err(Refusal::new(
                format!("the engine {e} has local changes besides its audit log"),
                format!("commit or discard them there: git -C {e} status --short"),
            ));
        }
        let rows: Vec<String> = self
            .engine_git(&["diff", "verification-log.jsonl"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.strip_prefix('+'))
            .filter(|l| l.starts_with('{'))
            .map(str::to_string)
            .collect();
        // APPENDED, never rewritten: a later sync's empty diff must not overwrite the
        // rows an earlier one kept and could not record
        let kept = self.logs.join("audit-rows.jsonl");
        if !rows.is_empty()
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&kept)
        {
            let _ = writeln!(f, "{}", rows.join("\n"));
        }
        let _ = self.engine_git(&["checkout", "-q", "--", "verification-log.jsonl"]);
        if self
            .engine_git(&["merge", "--ff-only", "-q", "origin/main"])
            .is_none()
        {
            return Err(Refusal::new(
                format!(
                    "the engine {e} is not a fast-forward of its origin/main (its audit rows are \
                     kept in {})",
                    kept.display()
                ),
                format!("reconcile it by hand: git -C {e} log --oneline origin/main..HEAD"),
            ));
        }
        if rows.is_empty() {
            return Ok(());
        }
        let path = self.engine.join("verification-log.jsonl");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let merged = audit_rows_merged(&text, &rows);
        let recorded = std::fs::write(&path, merged).is_ok()
            && self
                .engine_git(&[
                    "commit",
                    "-q",
                    "-m",
                    "chore(audit): record stage verification rows",
                    "--",
                    "verification-log.jsonl",
                ])
                .is_some()
            && self
                .engine_git(&["push", "-q", "origin", "HEAD:main"])
                .is_some();
        if recorded {
            Ok(())
        } else {
            Err(Refusal::new(
                "could not record the engine's audit rows (commit and push of \
                 verification-log.jsonl)",
                format!(
                    "{RELEASE_COMMAND} again (the rows are kept in {})",
                    kept.display()
                ),
            ))
        }
    }

    fn engine_dirty(&self) -> bool {
        self.engine_git(&["status", "--porcelain"])
            .unwrap_or_default()
            .lines()
            .any(|l| !l.ends_with(" verification-log.jsonl"))
    }

    fn submodule_sync(&mut self, step: Step) -> Result<(), Refusal> {
        let mut cmd = Command::new("git");
        cmd.args(["submodule", "update", "--init", "vendor/astream"])
            .current_dir(&self.repo);
        let ran = self.logged(step, &mut cmd);
        if ran.code == 0 {
            Ok(())
        } else {
            Err(Refusal::new(
                "git submodule update --init vendor/astream failed — the workspace does not \
                 resolve without it",
                "check access to the astream repository, then re-run",
            )
            .detail(ran.detail))
        }
    }

    /// The ONE clean-tree judge: `aterm_verify::identity::dirty_against`, the Trust
    /// lane's own rule (the tracked `.gitignore` files, never `.git/info/exclude` or
    /// `core.excludesFile`) — so the release and the lane cannot disagree on whether
    /// this clone is clean. `Err` when git could not answer.
    fn dirty(&self) -> Result<Vec<String>, String> {
        crate::gates::dirty_paths(&self.repo).map_err(|e| e.to_string())
    }

    /// This clone's main to `rev`: a fast-forward, or — only when every commit it would
    /// leave is on origin/main already — back to it. Nothing is lost either way.
    fn main_to(&mut self, rev: &str) -> Result<(), Refusal> {
        let head = self.head();
        let ancestor = |s: &mut Self, a: &str, b: &str| s.is_ancestor(a, b);
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.repo);
        if ancestor(self, &head, rev) {
            cmd.args(["merge", "-q", "--ff-only", rev]);
        } else if ancestor(self, &head, "origin/main") {
            cmd.args(["reset", "-q", "--keep", rev]);
        } else {
            return Err(Refusal::new(
                "this clone's main has commits origin/main does not",
                "push them, or release from a fresh clone",
            ));
        }
        let ran = self.logged(Step::Commit, &mut cmd);
        if ran.code == 0 {
            Ok(())
        } else {
            Err(Refusal::new(
                format!("cannot move this clone's main to {}", short(rev)),
                format!("git -C {} status; then re-run", self.repo.display()),
            )
            .detail(ran.detail))
        }
    }

    // -- the preflight ----------------------------------------------------------

    /// STEP 0 — THE ENGINE'S OWN DRY RUN: `pub stage aterm --dry-run --no-check --commit
    /// R`, every export guard the stage runs, in one pass, over a SCRATCH CLONE of this
    /// checkout (never this checkout's `publish/.out`, where a real stage's attestation
    /// waits for `pub publish`). It is never skipped: a step 0 that cannot run is a block.
    /// The export it leaves, for the wasm32 check.
    fn engine_dry_run(&mut self, r: &str, blocks: &mut Vec<(bool, Refusal)>) -> Option<PathBuf> {
        let cache = match preflight_cache(&self.repo) {
            Ok(cache) => cache,
            Err(why) => {
                blocks.push((
                    false,
                    Refusal::new(
                        format!("the engine's dry run has no place to run: {why}"),
                        "fix what that names, then re-run",
                    ),
                ));
                return None;
            }
        };
        let name = self
            .repo
            .file_name()
            .map_or_else(|| "aterm".into(), |n| n.to_os_string());
        let root = cache.join("dry-run").join(name);
        let log = cache.join("dry-run.log");
        let _ = std::fs::create_dir_all(&cache);
        if let Err(why) = self.dry_clone(&cache, &root, r) {
            blocks.push((
                false,
                Refusal::new(
                    format!(
                        "the engine's dry run could not lay {} out, so no export guard has run: \
                         {why}",
                        short(r)
                    ),
                    format!(
                        "fix what that names, then re-run (git's own words: {})",
                        cache.join("dry-run-clone.log").display()
                    ),
                ),
            ));
            return None;
        }
        let pubbin = self.engine.join("bin/pub");
        let mut stage = self.scrubbed(&pubbin);
        stage
            .args([
                "stage",
                "aterm",
                "--dry-run",
                "--no-check",
                "--commit",
                r,
                "--repo-path",
            ])
            .arg(&root);
        let out = self.output_forwarded(&mut stage);
        let (code, text) = match out {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ),
            ),
            Err(e) => (-1, format!("cannot run {}: {e}", pubbin.display())),
        };
        let _ = std::fs::write(&log, &text);
        let export = root.join("publish/.out/export");
        let (verdict, export) = engine_dry_run_verdict(code, &text, short(r), export.is_dir());
        for refusal in verdict {
            blocks.push((
                false,
                refusal.detail([format!("the whole run: {}", log.display())]),
            ));
        }
        export.then_some(root.join("publish/.out/export"))
    }

    /// R laid out at `root` the way `pub stage --repo-path` needs a checkout: `git clone
    /// --shared` of this checkout (objects borrowed), R checked out, origin set to this
    /// checkout's origin, each submodule cloned the same way at its gitlink.
    fn dry_clone(&self, cache: &Path, root: &Path, r: &str) -> Result<(), String> {
        let log = cache.join("dry-run-clone.log");
        let _ = std::fs::write(&log, "");
        let _ = std::fs::remove_dir_all(cache.join("dry-run"));
        if cache.join("dry-run").exists() {
            return Err(format!(
                "the last run's clone at {} cannot be removed",
                cache.join("dry-run").display()
            ));
        }
        let origin = self
            .git_out(&["remote", "get-url", "origin"])
            .ok_or("this checkout names no origin")?;
        let git = |dir: &Path, args: &[&str]| -> bool {
            let out = self.output_forwarded(Command::new("git").arg("-C").arg(dir).args(args));
            if let Ok(o) = &out
                && let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log)
            {
                let _ = f.write_all(&o.stderr);
            }
            out.is_ok_and(|o| o.status.success())
        };
        let (repo, root_s) = (self.repo.display().to_string(), root.display().to_string());
        if !git(
            cache,
            &[
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                &repo,
                &root_s,
            ],
        ) || !git(root, &["remote", "set-url", "origin", &origin])
            || !git(
                root,
                &[
                    "-c",
                    "submodule.recurse=false",
                    "checkout",
                    "--quiet",
                    "--detach",
                    r,
                ],
            )
        {
            return Err(format!(
                "git could not clone this checkout at {} into {root_s}",
                short(r)
            ));
        }
        for (rev, path) in gitlinks(&self.git_out(&["ls-tree", "-r", r]).unwrap_or_default()) {
            let sub = self.repo.join(&path);
            let own = Command::new("git")
                .arg("-C")
                .arg(&sub)
                .args(["rev-parse", "--show-toplevel"])
                .output()
                .ok()
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
            let is_own = own.and_then(|p| p.canonicalize().ok()) == sub.canonicalize().ok();
            if !is_own || !git(&sub, &["cat-file", "-e", &format!("{rev}^{{commit}}")]) {
                return Err(format!(
                    "the submodule {path} is not initialized at its gitlink {} in this checkout, \
                     and the stage writes it from there (git submodule update --init {path})",
                    short(&rev)
                ));
            }
            let dest = root.join(&path);
            let _ = std::fs::remove_dir_all(&dest);
            let (sub_s, dest_s) = (sub.display().to_string(), dest.display().to_string());
            if !git(
                cache,
                &[
                    "clone",
                    "--quiet",
                    "--shared",
                    "--no-checkout",
                    &sub_s,
                    &dest_s,
                ],
            ) || !git(&dest, &["checkout", "--quiet", "--detach", &rev])
            {
                return Err(format!(
                    "git could not clone the submodule {path} at {} into the scratch clone",
                    short(&rev)
                ));
            }
        }
        Ok(())
    }

    /// THE WASM32 ENGINE: the site's /terminal engine type-checked on the public
    /// toolchain pin, with `publish/config.sh`'s own wasm32 clause (read out of
    /// `CHECK_CMD_DEFAULT`, never restated), over the export step 0 left — else a `git
    /// archive` of R without `.cargo/` — copied outside every checkout, in a scrubbed
    /// environment. v0.91.0 was refused there by the site build AFTER its promote.
    fn wasm32(&mut self, r: &str, export: Option<&Path>, blocks: &mut Vec<(bool, Refusal)>) {
        let config =
            std::fs::read_to_string(self.repo.join("publish/config.sh")).unwrap_or_default();
        let shape = match wasm_clause(&config) {
            Ok(shape) => shape,
            Err(r) => {
                blocks.push((false, r));
                return;
            }
        };
        let cache = match preflight_cache(&self.repo) {
            Ok(cache) => cache,
            Err(why) => {
                blocks.push((
                    false,
                    Refusal::new(
                        format!("the wasm32 check has no place to run: {why}"),
                        "fix what that names, then re-run",
                    ),
                ));
                return;
            }
        };
        let src = cache.join("wasm-src");
        let log = cache.join("wasm-check.log");
        let _ = std::fs::remove_dir_all(&src);
        let laid = std::fs::create_dir_all(&src).is_ok()
            && match export {
                Some(export) => self
                    .output_forwarded(
                        Command::new("/bin/cp")
                            .arg("-R")
                            .arg(export.join("."))
                            .arg(&src),
                    )
                    .is_ok_and(|o| o.status.success()),
                None => self.archive_into(r, &src),
            };
        if !laid {
            self.say(&format!(
                "        note: could not lay out {} in {} — the wasm32 check did not run \
                 (`pub stage`'s public-clone gate runs the same clause)",
                short(r),
                src.display()
            ));
            return;
        }
        let mut env: Vec<(String, String)> = vec![
            (
                "CARGO_TARGET_DIR".into(),
                cache.join("wasm-target").display().to_string(),
            ),
            (
                "TERM".into(),
                std::env::var("TERM").unwrap_or_else(|_| "dumb".into()),
            ),
        ];
        for keep in [
            "PATH",
            "HOME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TZ",
            "TMPDIR",
            "RUSTUP_HOME",
            "CARGO_HOME",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Ok(v) = std::env::var(keep) {
                env.push((keep.into(), v));
            }
        }
        let scrubbed = |program: &str| {
            let mut cmd = Command::new(program);
            cmd.env_clear().envs(env.iter().cloned()).current_dir(&src);
            cmd
        };
        let add = self.output_forwarded(scrubbed("rustup").args([
            "target",
            "add",
            "--toolchain",
            &shape.pin,
            "wasm32-unknown-unknown",
        ]));
        let mut text = String::new();
        let added = match &add {
            Ok(o) => {
                text.push_str(&String::from_utf8_lossy(&o.stdout));
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                o.status.success()
            }
            Err(e) => {
                text.push_str(&format!("cannot run rustup: {e}\n"));
                false
            }
        };
        if !added {
            let _ = std::fs::write(&log, &text);
            if add.is_err() {
                self.say(
                    "        note: no rustup on PATH — the wasm32 check did not run (`pub stage`'s \
                     public-clone gate runs the same clause through rustup)",
                );
                return;
            }
            blocks.push((
                false,
                Refusal::new(
                    format!(
                        "rustup could not add wasm32-unknown-unknown to the wasm32 pin {}, so the \
                         wasm32 clause did not run — `pub stage` would fail the same way",
                        shape.pin
                    ),
                    format!(
                        "rustup toolchain install {} --target wasm32-unknown-unknown (full log: \
                         {})",
                        shape.pin,
                        log.display()
                    ),
                ),
            ));
            return;
        }
        let checked = self.output_forwarded(scrubbed("/bin/sh").args(["-c", &shape.clause]));
        let ok = match &checked {
            Ok(o) => {
                text.push_str(&String::from_utf8_lossy(&o.stdout));
                text.push_str(&String::from_utf8_lossy(&o.stderr));
                o.status.success()
            }
            Err(e) => {
                text.push_str(&format!("cannot run sh: {e}\n"));
                false
            }
        };
        let _ = std::fs::write(&log, &text);
        if ok {
            return;
        }
        let errors: Vec<String> = text
            .lines()
            .filter(|l| l.starts_with("error"))
            .take(6)
            .map(str::to_string)
            .collect();
        let missing_target = text.contains("can't find crate for `core`")
            || text.contains("target may not be installed");
        blocks.push((
            false,
            Refusal::new(
                format!(
                    "aterm-wasm does NOT compile for wasm32-unknown-unknown on the wasm32 pin {} \
                     — `pub stage` refuses this tree, and the site could not build its /terminal \
                     engine from it",
                    shape.pin
                ),
                if missing_target {
                    format!(
                        "rustup target add --toolchain {} wasm32-unknown-unknown (rustup did not \
                         add the pin's listed target by itself here)",
                        shape.pin
                    )
                } else {
                    format!(
                        "fix the source — a cfg(not(target_os = \"macos\")) arm no host build \
                         compiles is the usual culprit; full log: {}",
                        log.display()
                    )
                },
            )
            .detail(errors),
        ));
    }

    /// `git archive` of `r` into `dest`, without `.cargo/`, each submodule written from
    /// its own object store at the gitlink `r` records.
    fn archive_into(&self, r: &str, dest: &Path) -> bool {
        // one forwarded child for the pipe: the archive and the tar share its group
        let tar = |repo: &Path, rev: &str, into: &Path| -> bool {
            let mut pipe = Command::new("/bin/sh");
            pipe.args([
                "-c",
                "set -o pipefail 2>/dev/null; git -C \"$1\" archive \"$2\" | tar -x -C \"$3\"",
                "archive",
            ])
            .arg(repo)
            .arg(rev)
            .arg(into);
            self.output_forwarded(&mut pipe)
                .is_ok_and(|o| o.status.success())
        };
        if !tar(&self.repo, r, dest) {
            return false;
        }
        let _ = std::fs::remove_dir_all(dest.join(".cargo"));
        gitlinks(&self.git_out(&["ls-tree", "-r", r]).unwrap_or_default())
            .into_iter()
            .all(|(rev, path)| {
                let into = dest.join(&path);
                std::fs::create_dir_all(&into).is_ok() && tar(&self.repo.join(&path), &rev, &into)
            })
    }

    // -- after the cut --------------------------------------------------------

    /// The published release `tag` as GitHub serves it, through the authenticated `gh`
    /// (a release tool never spends the anonymous API budget every install shares).
    fn release_json(&self, slug: &str, tag: &str) -> Result<aterm_json::Value, String> {
        let out = self
            .output_forwarded(
                Command::new("gh").args(["api", &format!("repos/{slug}/releases/tags/{tag}")]),
            )
            .map_err(|e| format!("cannot run gh: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "could not read {slug} {tag} through gh ({}; gh auth status)",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        aterm_json::from_str(&String::from_utf8_lossy(&out.stdout))
            .map_err(|e| format!("the release JSON of {tag} is not JSON: {e}"))
    }

    /// THE EVERGREEN POINTER, read from what github.com serves a stranger: the first
    /// hop of `releases/latest/download/aterm-appcast.toml` is a 302 to exactly this
    /// tag's appcast, and the appcast served there names this version and binds its
    /// `url` to this tag and its DMG. `Err` when GitHub could not be READ (the HEAD
    /// failed, an answer that is neither 302 nor 404, the appcast's fetch failed):
    /// nothing is known about the release's shape then, so the caller says COULD NOT
    /// RUN and a re-run asks again — never the shape refusal, whose remedy ends a
    /// published release.
    fn pointer_problems(
        &self,
        slug: &str,
        tag: &str,
        version: &str,
    ) -> Result<Vec<String>, String> {
        let ptr = format!("https://github.com/{slug}/releases/latest/download/aterm-appcast.toml");
        let want = format!("https://github.com/{slug}/releases/download/{tag}/aterm-appcast.toml");
        let hop = aterm_update_core::head_no_redirect(&ptr)
            .map_err(|e| format!("{ptr} could not be read: {e:?}"))?;
        if let Some(problem) =
            pointer_hop_problem(&ptr, &want, tag, hop.code, hop.location.as_deref())?
        {
            return Ok(vec![problem]);
        }
        let served = self.output_forwarded(Command::new("curl").args([
            "-fsSL",
            "--proto-redir",
            "=https",
            "--max-time",
            "60",
            "--",
            &want,
        ]));
        let text = match served {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            Ok(o) => {
                return Err(format!(
                    "the appcast at {want} could not be fetched anonymously ({})",
                    String::from_utf8_lossy(&o.stderr).trim()
                ));
            }
            Err(e) => return Err(format!("cannot run curl: {e}")),
        };
        Ok(appcast_problems(&text, slug, tag, version))
    }

    /// The release alab.systems would link: `publish/site-sync.py choose-download`
    /// over the channel's release list (through the authenticated `gh`).
    fn site_tag(&self, slug: &str) -> Result<String, String> {
        let list = self
            .output_forwarded(
                Command::new("gh").args(["api", &format!("repos/{slug}/releases?per_page=30")]),
            )
            .map_err(|e| format!("cannot run gh: {e}"))?;
        if !list.status.success() {
            return Err(format!(
                "the release list for {slug} could not be read through gh ({})",
                String::from_utf8_lossy(&list.stderr).trim()
            ));
        }
        let (mut chooser, forwarded) = self
            .spawn_forwarded(
                Command::new("python3")
                    .arg(self.repo.join("publish/site-sync.py"))
                    .args(["choose-download", "--slug", slug])
                    .current_dir(&self.repo)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .map_err(|e| format!("cannot run publish/site-sync.py: {e}"))?;
        if let Some(mut stdin) = chooser.stdin.take() {
            let _ = stdin.write_all(&list.stdout);
        }
        let out = chooser
            .wait_with_output()
            .map_err(|e| format!("publish/site-sync.py: {e}"))?;
        drop(forwarded);
        let facts =
            aterm_json::from_str::<aterm_json::Value>(&String::from_utf8_lossy(&out.stdout))
                .ok()
                .filter(|_| out.status.success());
        facts
            .as_ref()
            .and_then(|f| f.get("tag"))
            .and_then(aterm_json::Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "publish/site-sync.py choose-download could not tell which release the site \
                     would link on {slug} ({})",
                    String::from_utf8_lossy(&out.stderr).trim()
                )
            })
    }

    fn site(&mut self, slug: &str) -> Result<AfterCut, Refusal> {
        let hook = self.repo.join("publish/post-promote");
        let mut cmd = self.child(&hook, None);
        cmd.arg("--latest").env("PUB_REPO_SLUG", slug);
        let ran = self.logged(Step::AfterCut, &mut cmd);
        site_outcome(ran)
    }
}

/// The after-cut step's refusal of a published release whose shape is wrong. Its cut
/// FINISHED, and a published release is never re-cut, so no re-run of this release can
/// end it: the remedy is its successor, and it names the way there — `--forget` this
/// clone's journal of it, then `--bump`, or a bare run
/// where main already declares a version no release has used (the `--bump` refusal of
/// that case says "drop --bump", which only a forgotten journal makes a way out).
#[must_use]
pub fn shape_refusal(slug: &str, tag: &str, problems: Vec<String>) -> Refusal {
    Refusal::new(
        format!(
            "{slug} {tag} does NOT carry the one-lean-download shape ({} problem(s)) — the site \
             was NOT touched, so its download stays on the last release that passed",
            problems.len()
        ),
        format!(
            "a published release is never re-cut, so this one ends here: ship its successor \
             from current main with {RELEASE_COMMAND} --forget (this clone's journal of \
             {tag}; nothing public changes), then {RELEASE_COMMAND} --bump, or a bare \
             {RELEASE_COMMAND} where origin/main already declares a version no release has \
             used (docs/RELEASING.md, Resume, recut, abandon)"
        ),
    )
    .detail(problems)
}

/// What `publish/post-promote --latest` ending with `ran` means for the after-cut step.
/// Exit 3 is a machine with no usable site checkout (none found, or one without its
/// build and deploy scripts), said in the hook's line, kept here, before the hook
/// touched anything: a hand step on a machine that has one, recorded, never a stop — a
/// re-run here could only meet it again ([`AfterCut::site_deferred`]).
///
/// # Errors
/// Every other failure: 4 (deployed, the live site not agreeing yet) and the rest.
pub fn site_outcome(ran: Ran) -> Result<AfterCut, Refusal> {
    match ran.code {
        0 => Ok(AfterCut {
            note: "alab.systems brought to the release (publish/post-promote --latest)".into(),
            site_deferred: false,
        }),
        3 => {
            // the hook's own line: where it looked, or what the checkout there lacks
            let why = ran
                .detail
                .iter()
                .rev()
                .find_map(|l| l.trim().strip_prefix("post-promote: "))
                .unwrap_or("its log says why");
            Ok(AfterCut {
                note: format!("publish/post-promote said: {why}"),
                site_deferred: true,
            })
        }
        4 => Err(Refusal::new(
            "the site is deployed, but the live site does not agree yet (publish/post-promote \
             exited 4)",
            format!("{RELEASE_COMMAND} again in a few minutes"),
        )
        .detail(ran.detail)),
        code => Err(Refusal::new(
            format!("the release is well shaped, but the site hook exited {code}"),
            format!(
                "read its log (a failed push leaves its site commit on this machine), then \
                 {RELEASE_COMMAND}"
            ),
        )
        .detail(ran.detail)),
    }
}

/// Whether the journal's bundle is BUILT for this release: its store entry COMPLETE in
/// the release cache with its bytes re-proved, and built by the release's prover. Not
/// built — none named, none COMPLETE, bytes that changed, another prover's — is step
/// 7's to build (before the claim nothing is burned by building again).
fn built_answer(repo: &Path, artifact: Option<&Artifact>, prover: &Prover) -> Answer {
    let Some(a) = artifact else {
        return Answer::no("no bundle is built for it yet");
    };
    let read = crate::artifact::store(repo).and_then(|store| crate::artifact::read(&store, &a.key));
    match read {
        Ok(Some(complete)) if complete.macho_sha256 != a.macho_sha256 => Answer::no(format!(
            "the release cache's bundle {} is not the journal's (executable {}, journaled {})",
            short(&a.key),
            short(&complete.macho_sha256),
            short(&a.macho_sha256)
        )),
        Ok(Some(complete)) if complete.trustc != prover.commit => Answer::no(format!(
            "its bundle was built by trustc {}, and this release's prover is trustc {}",
            short(&complete.trustc),
            short(&prover.commit)
        )),
        Ok(Some(_)) => Answer::yes(format!(
            "built once: {} (executable sha256 {})",
            a.app.display(),
            short(&a.macho_sha256)
        )),
        Ok(None) => Answer::no(format!(
            "the release cache holds no COMPLETE bundle {}",
            short(&a.key)
        )),
        Err(e) => Answer::no(format!("the release cache's bundle is not usable: {e}")),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn short(sha: &str) -> &str {
    sha.get(..9).unwrap_or(sha)
}

/// `+0200` → 7200; `-0700` → -25200.
fn parse_offset(z: &str) -> Option<i64> {
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    if digits.len() != 4 {
        return None;
    }
    let h: i64 = digits[..2].parse().ok()?;
    let m: i64 = digits[2..].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// The variables cleared from every child: what the gate records on a receipt's
/// `build-env` line, and the target directory overrides — said on the checks line —
/// and, silently, what `targo run` handed this
/// process as its launcher (`CARGO`, `CARGO_MANIFEST_*`, `CARGO_PKG_*`, their `TARGO_*`
/// twins, `TRUST_TARGO_FRONTEND`, `DYLD_FALLBACK_LIBRARY_PATH`): every step runs as it
/// would from the operator's shell, never as a child of the alias.
fn clears(name: &str) -> bool {
    matches!(name, "CARGO_TARGET_DIR" | "CARGO_BUILD_TARGET_DIR")
        || aterm_verify::differential::build_env_records(name)
}

fn launcher_env(name: &str) -> bool {
    matches!(
        name,
        "CARGO" | "CARGO_PRIMARY_PACKAGE" | "TRUST_TARGO_FRONTEND" | "DYLD_FALLBACK_LIBRARY_PATH"
    ) || ["CARGO_MANIFEST_", "CARGO_PKG_", "TARGO_"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Remove from `cmd` every variable [`clears`] or [`launcher_env`] names.
fn scrub(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str()
            && (clears(name) || launcher_env(name))
        {
            cmd.env_remove(name);
        }
    }
}

fn tail(path: &Path, n: usize) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| (*l).to_string())
        .collect()
}

/// Every gitlink (`160000`) of an `ls-tree -r` listing: (rev, path).
fn gitlinks(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            let mut f = meta.split_whitespace();
            (f.next()? == "160000").then(|| {
                let _kind = f.next();
                Some((f.next()?.to_string(), path.to_string()))
            })?
        })
        .collect()
}

/// Each kept audit row put back in time order (the pub-sync pattern).
#[must_use]
pub fn audit_rows_merged(text: &str, kept: &[String]) -> String {
    let at = |row: &str| -> String {
        aterm_json::from_str::<aterm_json::Value>(row)
            .ok()
            .and_then(|v| {
                v.get("at")
                    .and_then(aterm_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default()
    };
    let mut rows: Vec<String> = text.lines().map(str::to_string).collect();
    for row in kept {
        if rows.contains(row) {
            continue;
        }
        let when = at(row);
        let i = rows.iter().position(|r| at(r) > when).unwrap_or(rows.len());
        rows.insert(i, row.clone());
    }
    let mut out = rows.join("\n");
    out.push('\n');
    out
}

/// What the engine's dry run said, as blocks: `(blocks, the export exists)`.
/// `exported`: the export directory exists.
#[must_use]
pub fn engine_dry_run_verdict(
    code: i32,
    text: &str,
    r: &str,
    exported: bool,
) -> (Vec<Refusal>, bool) {
    let n_ok = text
        .lines()
        .filter(|l| l.starts_with("== guard ok"))
        .count();
    let fails: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("== GUARD FAIL"))
        .collect();
    let export = n_ok + fails.len() > 0 && exported;
    if code == 0 && n_ok > 0 {
        return (Vec::new(), export);
    }
    let fix_source = "fix the SOURCE, never the export — patterns: the engine's \
                      baseline/forbidden-content.txt + publish/forbidden-extra.txt; the engine's \
                      reports are in the scratch clone's publish/.out";
    if code == 0 {
        return (
            vec![
                Refusal::new(
                    format!(
                        "the engine's dry run exited 0 without reporting a single guard over {r} — \
                     no export guard is known to have run"
                    ),
                    "read the whole run and teach this step the engine's words",
                )
                .detail(last_lines(text, 3)),
            ],
            export,
        );
    }
    if !fails.is_empty() {
        let mut out = Vec::new();
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if let Some(what) = l.strip_prefix("== GUARD FAIL: ") {
                let detail: Vec<String> = lines[i + 1..]
                    .iter()
                    .take_while(|d| d.starts_with("  "))
                    .take(6)
                    .map(|d| d.trim().to_string())
                    .collect();
                out.push(Refusal::new(what.to_string(), fix_source).detail(detail));
            }
        }
        return (out, export);
    }
    let refusals: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("FAIL: "))
        .filter(|l| *l != "transforms failed")
        .collect();
    let refusal = match refusals.first() {
        Some(first) => Refusal::new(
            format!("the engine refuses to stage {r} before any guard ran (exit {code}): {first}"),
            "fix the source the refusal names (never the export)",
        )
        .detail(refusals[1..].iter().take(2).map(|l| (*l).to_string())),
        None => Refusal::new(
            format!(
                "the engine's dry run stopped without a guard verdict (exit {code}) and printed no \
                 refusal"
            ),
            "read the whole run",
        )
        .detail(last_lines(text, 3)),
    };
    (vec![refusal], export)
}

fn last_lines(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| (*l).to_string())
        .collect()
}

/// The wasm32 clause of `publish/config.sh`'s `CHECK_CMD_DEFAULT` and the stock
/// channel it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmClause {
    pub clause: String,
    pub pin: String,
}

/// Read the wasm32 clause out of `config` (the engine's own line shape,
/// `CHECK_CMD_DEFAULT="…"`), with every refusal the stage would meet for it.
///
/// # Errors
/// No clause; no stock channel; no target-add before it; no `RUSTC` naming the pin's
/// own rustc.
pub fn wasm_clause(config: &str) -> Result<WasmClause, Refusal> {
    let line = config
        .lines()
        .find_map(|l| l.strip_prefix("CHECK_CMD_DEFAULT=\""))
        .and_then(|l| l.trim_end().strip_suffix('"'))
        .unwrap_or_default();
    let Some(clause) = line
        .split(" && ")
        .find(|c| c.contains("--target wasm32-unknown-unknown"))
        .map(str::trim)
    else {
        return Err(Refusal::new(
            "publish/config.sh's CHECK_CMD_DEFAULT has no wasm32 clause — nothing before the \
             promote compiles the site's /terminal engine",
            "restore its `cargo check --locked --target wasm32-unknown-unknown -p aterm-wasm` \
             clause (publish/config.sh, THE WASM32 CLAUSE)",
        ));
    };
    let pin = clause
        .split_once("rustup run ")
        .and_then(|(_, rest)| rest.split_once(" cargo "))
        .map(|(pin, _)| pin.to_string())
        .filter(|p| !p.is_empty() && !p.contains(' '));
    let Some(pin) = pin else {
        return Err(Refusal::new(
            format!(
                "publish/config.sh's wasm32 clause names no stock channel — the wasm32 pin is the \
                 compiler this check and the stage's gate both run: {clause}"
            ),
            "spell the clause `rustup run <channel> cargo check …`",
        ));
    };
    let add = format!("rustup target add --toolchain {pin} wasm32-unknown-unknown");
    if !line.contains(&format!("{add} && ")) {
        return Err(Refusal::new(
            format!(
                "publish/config.sh's CHECK_CMD_DEFAULT does not add the wasm32 target to {pin} \
                 before its wasm32 clause — `pub stage` on a host without that target fails after \
                 the verified build"
            ),
            format!("put `{add} && ` before the wasm32 clause"),
        ));
    }
    let rustc = format!("RUSTC=$(rustup which --toolchain {pin} rustc) ");
    if !clause.contains(&rustc) {
        return Err(Refusal::new(
            format!(
                "publish/config.sh's wasm32 clause does not name {pin}'s own rustc — cargo would \
                 take `rustc` from the host PATH (the gate's fresh CARGO_HOME holds no rustup \
                 proxy): {clause}"
            ),
            format!("put `{rustc}` before `rustup run {pin} cargo` in the wasm32 clause"),
        ));
    }
    Ok(WasmClause {
        clause: clause.to_string(),
        pin,
    })
}

/// The lean-download shape of a published release, from what GitHub serves (its
/// release JSON): the problems, empty when it carries the shape. It reads the
/// published objects, never the files the cut wrote, by the release commit's own
/// rules (the release re-ran as that commit's cutter; v0.63.0 shipped a 1.07 GB
/// seeded DMG from a cutter built from an older tree) — and only a release that
/// passes is put on the site's download button.
///
/// * `aterm-<v>.dmg` exists and is under the lean ceiling (200 MB);
/// * `aterm-<v>-mac.zip` exists; the evergreen twins `aterm.dmg` / `aterm-mac.zip`
///   exist and are byte copies (the same size as what they alias);
/// * a `.sha256` sidecar for each of those four; `aterm-appcast.toml` exists;
/// * no retired macOS name (`-x86_64.dmg`, `-lite.dmg`, `aterm-offline.dmg`);
/// * the body does not sell the download as seeded;
/// * the Linux lane: canonical raw ELF names for x86_64/aarch64 with bounded sizes and
///   all four appcast/roster proof objects beside them; historical `.tar.gz` pairs
///   complete; no other architecture or version. An inventory, never an authenticity
///   claim — the cut's own proofs verify the bytes.
#[must_use]
pub fn release_shape(release: &aterm_json::Value, version: &str) -> Vec<String> {
    const MAX_DMG_BYTES: u64 = 200 * 1000 * 1000;
    let mut assets: std::collections::BTreeMap<String, Option<u64>> = Default::default();
    for a in release
        .get("assets")
        .and_then(aterm_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = a
            .get("name")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("");
        assets.insert(
            name.to_string(),
            a.get("size").and_then(aterm_json::Value::as_u64),
        );
    }
    let mut bad = Vec::new();
    let dmg = format!("aterm-{version}.dmg");
    let zip = format!("aterm-{version}-mac.zip");
    match assets.get(&dmg) {
        None => bad.push(format!(
            "{dmg} is missing — the download the site links and the updater binds"
        )),
        Some(None) => bad.push(format!(
            "{dmg} carries no size in the release JSON — cannot prove it is lean"
        )),
        Some(Some(n)) if *n > MAX_DMG_BYTES => bad.push(format!(
            "{dmg} is {n} bytes — over the 200 MB lean ceiling: a seeded image"
        )),
        Some(Some(_)) => {}
    }
    if !assets.contains_key(&zip) {
        bad.push(format!(
            "{zip} is missing — the updater, the Homebrew cask and install.sh all consume it"
        ));
    }
    for (twin, source) in [("aterm.dmg", &dmg), ("aterm-mac.zip", &zip)] {
        match (assets.get(twin), assets.get(source.as_str())) {
            (None, _) => bad.push(format!(
                "{twin} is missing — the permanent releases/latest/download name"
            )),
            (Some(Some(a)), Some(Some(b))) if a != b => bad.push(format!(
                "{twin} is {a} bytes but {source} is {b} — a twin must be a byte copy of the \
                 container it aliases"
            )),
            _ => {}
        }
    }
    for name in [dmg.as_str(), zip.as_str(), "aterm.dmg", "aterm-mac.zip"] {
        let side = format!("{name}.sha256");
        if !assets.contains_key(&side) {
            bad.push(format!(
                "{side} is missing — `shasum -a 256 -c` matches on the embedded name, so each \
                 container and twin needs its own sidecar"
            ));
        }
    }
    if !assets.contains_key("aterm-appcast.toml") {
        bad.push(
            "aterm-appcast.toml is missing — installed copies read it to find this release".into(),
        );
    }
    let retired: Vec<&String> = assets
        .keys()
        .filter(|n| {
            n.ends_with("-x86_64.dmg") || n.ends_with("-lite.dmg") || *n == "aterm-offline.dmg"
        })
        .collect();
    if !retired.is_empty() {
        bad.push(format!(
            "retired macOS asset(s) present: {} — there is exactly ONE macOS DMG",
            retired
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let body = release
        .get("body")
        .and_then(aterm_json::Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let said: Vec<&str> = [
        "batteries-included",
        "batteries included",
        "offline alab toolchain seed",
        "toolchain seed",
        "air-gapped",
    ]
    .into_iter()
    .filter(|w| body.contains(w))
    .collect();
    if !said.is_empty() {
        bad.push(format!(
            "the release body still describes a seeded download ({}) — the lean template says \
             the toolchain installs itself on first launch",
            said.join(", ")
        ));
    }
    bad.extend(linux_inventory(&assets, version));
    bad
}

fn linux_inventory(
    assets: &std::collections::BTreeMap<String, Option<u64>>,
    version: &str,
) -> Vec<String> {
    const ARCHES: [&str; 2] = ["x86_64", "aarch64"];
    let mut bad = Vec::new();
    let mut native = Vec::new();
    let (mut tars, mut sides) = (Vec::new(), Vec::new());
    for (name, size) in assets {
        let Some(rest) = name.strip_prefix("aterm-") else {
            continue;
        };
        let Some((named, tail)) = rest.split_once("-linux-") else {
            continue;
        };
        let (arch, kind) = if let Some(a) = tail.strip_suffix(".tar.gz.sha256") {
            (a, "side")
        } else if let Some(a) = tail.strip_suffix(".tar.gz") {
            (a, "tar")
        } else {
            (tail, "native")
        };
        if named != version {
            bad.push(format!(
                "Linux asset {name} names another version — this release is {version}"
            ));
            continue;
        }
        if !ARCHES.contains(&arch) {
            bad.push(format!(
                "Linux asset {name} is named for an unsupported or noncanonical architecture — \
                 closed table (x86_64, aarch64)"
            ));
            continue;
        }
        match kind {
            "side" => sides.push(arch.to_string()),
            "tar" => tars.push(arch.to_string()),
            _ => {
                native.push(arch.to_string());
                if !size.is_some_and(|n| (64..=512 * 1024 * 1024).contains(&n)) {
                    bad.push(format!(
                        "{name} has invalid native ELF size {size:?} (expected 64..536870912 bytes)"
                    ));
                }
            }
        }
    }
    for arch in tars.iter().filter(|a| !sides.contains(a)) {
        bad.push(format!(
            "aterm-{version}-linux-{arch}.tar.gz.sha256 is missing — the historical manual \
             download requires its transport checksum"
        ));
    }
    for arch in sides.iter().filter(|a| !tars.contains(a)) {
        bad.push(format!(
            "aterm-{version}-linux-{arch}.tar.gz.sha256 is present but its tarball is not — the \
             historical manual-download pair is incomplete"
        ));
    }
    if !native.is_empty() {
        for proof in [
            "aterm-appcast.toml",
            "aterm-appcast.toml.sig",
            "aterm-machines.toml",
            "aterm-machines.toml.sig",
        ] {
            if !assets.get(proof).is_some_and(|s| s.is_some_and(|n| n > 0)) {
                bad.push(format!(
                    "Linux native proof object {proof} is missing or empty — all four \
                     authenticated appcast/roster objects are required"
                ));
            }
        }
    }
    bad
}

/// What the after-cut step makes of the pointer's answer: its shape problems, or
/// — when GitHub could not be READ — a COULD NOT RUN refusal whose remedy is to
/// run it again, never a shape problem, whose refusal ends a published release.
fn pointer_verdict(read: Result<Vec<String>, String>) -> Result<Vec<String>, Refusal> {
    read.map_err(|why| {
        Refusal::new(
            format!("{why}, so the release pointer's shape is unknown"),
            format!("{RELEASE_COMMAND} again when GitHub answers"),
        )
        .could_not_run()
    })
}

/// The pointer's first hop judged: `Ok(None)` when it is a 302 to exactly `want`;
/// `Ok(Some(problem))` for a SHAPE fact — 404 (no published release holds the
/// pointer) or a 302 elsewhere (another release holds GitHub's latest); `Err` for
/// any other answer, which says nothing about the release (a 5xx, a 403 rate
/// limit): the caller's COULD NOT RUN.
fn pointer_hop_problem(
    ptr: &str,
    want: &str,
    tag: &str,
    code: u16,
    location: Option<&str>,
) -> Result<Option<String>, String> {
    let why = match code {
        302 if location == Some(want) => return Ok(None),
        404 => "404 — no published non-prerelease release holds the pointer".to_string(),
        302 => format!(
            "302 to {}, not {want} — another release holds GitHub's latest; the cut owns \
             it: while {tag}'s cut is open, `{RELEASE_COMMAND}` resumes it and re-sends \
             its head PATCH (never re-point it by hand)",
            location.unwrap_or("<no Location>")
        ),
        code => {
            return Err(format!(
                "{ptr} answers HTTP {code}, not a release pointer's 302 or 404"
            ));
        }
    };
    Ok(Some(format!(
        "{ptr} answers {why}; every credential-less install discovers the channel head \
         from this pointer and would never see {tag}"
    )))
}

/// The appcast served at the pointer's target: it names `version`, and its `url` is
/// exactly the tag-specific download URL of its own `dmg` (the credential-less updater
/// refuses any other).
#[must_use]
pub fn appcast_problems(text: &str, slug: &str, tag: &str, version: &str) -> Vec<String> {
    let field = |key: &str| {
        text.lines().find_map(|l| {
            l.strip_prefix(&format!("{key} = \""))
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_string)
        })
    };
    if field("version").as_deref() != Some(version) {
        return vec![format!(
            "the appcast served for {tag} does not name version {version}"
        )];
    }
    let dmg = field("dmg").unwrap_or_default();
    let want = format!("https://github.com/{slug}/releases/download/{tag}/{dmg}");
    match field("url") {
        Some(url) if !dmg.is_empty() && url == want => Vec::new(),
        url => vec![format!(
            "the appcast's url is '{}', not {want} — the credential-less updater refuses a \
             manifest whose url does not bind it to its tag and container",
            url.unwrap_or_else(|| "<absent>".into())
        )],
    }
}

impl Backend for Real {
    fn now(&mut self) -> u64 {
        now()
    }

    fn hms(&mut self, epoch: u64) -> String {
        let local = (epoch as i64 + self.offset).rem_euclid(86_400);
        format!(
            "{:02}:{:02}:{:02}",
            local / 3600,
            local % 3600 / 60,
            local % 60
        )
    }

    /// Woken by an interrupt: the owner's wait ends the moment one arrives.
    fn sleep(&mut self, secs: u64) {
        for _ in 0..secs {
            if interrupt::caught().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    /// To the terminal, and to the run's transcript beside its step logs — a closed
    /// terminal (SIGHUP) loses no line, and a write to it never ends the run.
    fn say(&mut self, line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.logs.join("transcript.log"))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn interrupted(&mut self) -> Option<i32> {
        interrupt::caught()
    }

    fn log_path(&mut self, step: Step) -> PathBuf {
        self.logs
            .join(format!("{:02}-{}.log", step.number(), step.name()))
    }

    fn me(&mut self) -> (u32, String) {
        let pid = std::process::id();
        (pid, self.started_of(pid).unwrap_or_default())
    }

    fn started_of(&mut self, pid: u32) -> Option<String> {
        started_of(pid)
    }

    fn entered(&mut self, step: Step) {
        self.step = step;
    }

    fn release_dir(&mut self) -> PathBuf {
        self.release.clone()
    }

    fn times(&mut self) -> String {
        std::fs::read_to_string(self.state.join("steps.tsv")).unwrap_or_default()
    }

    fn record_time(&mut self, version: &str, step: Step, secs: u64) {
        let _ = std::fs::create_dir_all(&self.state);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state.join("steps.tsv"))
        {
            let _ = writeln!(f, "{}\t{version}\t{}\t{secs}", now(), step.name());
        }
    }

    fn prover(&mut self, journaled: Option<&str>) -> Result<Prover, String> {
        let explicit = std::env::var_os("TRUST_STAGE2_BIN").filter(|v| !v.is_empty());
        let (cand, source) = if let Some(dir) = explicit {
            (PathBuf::from(dir), "TRUST_STAGE2_BIN".to_string())
        } else if let Some(j) = journaled
            && Path::new(j).join("trustc").is_file()
            && Path::new(j).join("targo").is_file()
        {
            (
                PathBuf::from(j),
                "the prover this release started with".to_string(),
            )
        } else {
            let found = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
            let mut source = "the cut's own toolchain resolution".to_string();
            if let Some(j) = journaled {
                source.push_str(&format!(
                    " — the prover this release started with ({j}) is gone, and its receipts \
                     with it"
                ));
            }
            (found, source)
        };
        let bin = cand
            .canonicalize()
            .map_err(|e| format!("{source} ({}) is not a directory: {e}", cand.display()))?;
        if !bin.join("trustc").is_file() || !bin.join("targo").is_file() {
            return Err(format!(
                "{source} ({}) holds no trustc and targo",
                bin.display()
            ));
        }
        let identity = crate::gates::toolchain_at(&bin).map_err(|e| e.to_string())?;
        let commit = identity.rsplit(' ').next().unwrap_or("unknown").to_string();
        if commit == "unknown" || commit.is_empty() {
            return Err(format!(
                "{}/trustc names no commit-hash in `trustc -vV`, so no receipt can tell it from \
                 another prover",
                bin.display()
            ));
        }
        // THE ONE PROVER, pinned for this process before anything resolves a toolchain:
        // the provisioning audit's toolchain, verifier and front-door lines judge it,
        // never the store's current build (which a resume's journaled prover may not be)
        crate::gates::pin_trust_stage2_bin(&bin).map_err(|e| e.to_string())?;
        Ok(Prover {
            bin,
            commit,
            source,
        })
    }

    fn lease(&mut self, prover: &Prover) {
        crate::gates::lease_toolchain(&prover.bin);
    }

    fn cleared_env(&mut self) -> Vec<String> {
        let mut names: Vec<String> = std::env::vars_os()
            .filter_map(|(k, _)| k.into_string().ok())
            .filter(|k| clears(k))
            .collect();
        names.sort();
        names
    }

    fn machine(&mut self, creds: &Path, explicit: bool) -> (String, Vec<Refusal>) {
        let mut out = Vec::new();
        if explicit && !creds.is_file() {
            out.push(Refusal::new(
                format!("--release-credentials {} is not a file", creds.display()),
                "name the signing profile ship provision wrote (docs/RELEASE-KEYS.md), or \
                 drop the flag for ~/.aterm/release-credentials.toml",
            ));
        }
        // this machine's roster id: its public identity file, else the profile's line;
        // none is the provisioning audit's owner gap (the roster), never a refusal here
        let field = |path: &Path, key: &str| {
            std::fs::read_to_string(path).ok().and_then(|t| {
                t.lines().find_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
                })
            })
        };
        let id = field(&self.home.join(".aterm/machine.toml"), "id")
            .or_else(|| field(creds, "machine_id"))
            .unwrap_or_default();
        if let (Some(free), _) = self.disk()
            && free < crate::gates::MIN_FREE_DISK_GIB
        {
            out.push(Refusal::new(
                format!(
                    "only {free} GiB free on {} — the cut needs {}",
                    self.repo.display(),
                    crate::gates::MIN_FREE_DISK_GIB
                ),
                "free space, then re-run",
            ));
        }
        let e = self.engine.display().to_string();
        if !self.engine.join(".git").exists() || !self.engine.join("bin/pub").is_file() {
            out.push(Refusal::new(
                format!("no publication engine at {e} (bin/pub)"),
                "clone it there, or pass --engine <path>",
            ));
        } else if self
            .engine_git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .is_none_or(|b| b.trim() != "main")
        {
            out.push(Refusal::new(
                format!("the engine {e} is not on main — stage and publish run from its main"),
                format!("git -C {e} switch main"),
            ));
        } else if self.engine_dirty() {
            out.push(Refusal::new(
                format!("the engine {e} has local changes besides its audit log"),
                format!("commit or discard them there: git -C {e} status --short"),
            ));
        }
        match self.dirty() {
            Ok(dirty) if dirty.is_empty() => {}
            Ok(dirty) => out.push(
                Refusal::new(
                    format!(
                        "this clone ({}) is not clean — the trust lane files facts only for a \
                         clean tree",
                        self.repo.display()
                    ),
                    "commit or remove what it lists: a release runs in a clone of its own",
                )
                .detail(dirty.into_iter().take(5)),
            ),
            Err(why) => out.push(Refusal::new(
                format!("this clone's state cannot be read: {why}"),
                "run git status there, then re-run",
            )),
        }
        (id, out)
    }

    fn fetch(&mut self) -> Result<String, Refusal> {
        let fetched = self.remote_git(&["fetch", "-q", "origin", "main"]);
        if !fetched.as_ref().is_ok_and(|o| o.status.success()) {
            return Err(Refusal::new(
                "cannot fetch origin — there are no offline releases (the bump, the stage and \
                 the claim are pushes)",
                "check the network and your git credentials",
            ));
        }
        let tags = self.remote_git(&["ls-remote", "--tags", "origin"]);
        match tags {
            Ok(out) if out.status.success() => {
                self.origin_tags = String::from_utf8_lossy(&out.stdout).into_owned();
            }
            _ => {
                return Err(Refusal::new(
                    "cannot list origin's tags",
                    "check the network and your git credentials",
                ));
            }
        }
        self.git_out(&["rev-parse", "origin/main"]).ok_or_else(|| {
            Refusal::new(
                "origin/main does not resolve",
                "git fetch origin main by hand",
            )
        })
    }

    fn head(&mut self) -> String {
        self.git_out(&["rev-parse", "HEAD"]).unwrap_or_default()
    }

    fn branch_is_main(&mut self) -> bool {
        self.git_out(&["rev-parse", "--abbrev-ref", "HEAD"])
            .as_deref()
            == Some("main")
    }

    fn rev(&mut self, rev: &str) -> Option<String> {
        self.git_out(&["rev-parse", "--verify", "-q", rev])
    }

    fn version_at(&mut self, rev: &str) -> Option<String> {
        let manifest = self.git_out(&["show", &format!("{rev}:Cargo.toml")])?;
        crate::publish::workspace_version(&manifest).ok()
    }

    fn is_ancestor(&mut self, a: &str, b: &str) -> bool {
        !a.is_empty()
            && self
                .git()
                .git(&["merge-base", "--is-ancestor", a, b])
                .is_ok_and(|o| o.success())
    }

    fn origin_tag(&mut self, tag: &str) -> Option<String> {
        let want = format!("refs/tags/{tag}");
        let peeled = format!("{want}^{{}}");
        let mut plain = None;
        for line in self.origin_tags.lines() {
            let Some((sha, name)) = line.split_once('\t') else {
                continue;
            };
            if name == peeled {
                return Some(sha.to_string());
            }
            if name == want {
                plain = Some(sha.to_string());
            }
        }
        plain
    }

    fn origin_tag_now(&mut self, tag: &str) -> Result<Option<String>, String> {
        let want = format!("refs/tags/{tag}");
        let out = self
            .remote_git(&["ls-remote", "--tags", "origin", &want])
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let peeled = format!("{want}^{{}}");
        let mut plain = None;
        for line in text.lines() {
            let Some((sha, name)) = line.split_once('\t') else {
                continue;
            };
            if name == peeled {
                return Ok(Some(sha.to_string()));
            }
            if name == want {
                plain = Some(sha.to_string());
            }
        }
        Ok(plain)
    }

    fn local_tag(&mut self, tag: &str) -> bool {
        self.rev(&format!("refs/tags/{tag}")).is_some()
    }

    fn ledger_versions(&mut self, rev: &str) -> Vec<String> {
        self.git_out(&["show", &format!("{rev}:RELEASES.ledger")])
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect()
    }

    fn engine_rows(&mut self, commit: Option<&str>) -> EngineRows {
        let text = std::fs::read_to_string(self.engine.join("mappings.json")).unwrap_or_default();
        engine_rows_in(&text, commit)
    }

    fn cut_journal(&mut self) -> Option<CutState> {
        let header =
            crate::publish::JournalHeader::read(&self.repo.join("dist/cut-state.toml")).ok()??;
        Some(CutState {
            finished: header.finished(),
            version: header.version,
            build: header.build_number,
        })
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        path.is_dir()
    }

    fn sync(&mut self) -> Result<String, Refusal> {
        self.engine_sync()?;
        self.submodule_sync(Step::Sync)?;
        let head = self
            .engine_git(&["rev-parse", "--short", "HEAD"])
            .unwrap_or_default();
        Ok(format!(
            "engine at {}, its origin/main; vendor/astream at its gitlink",
            head.trim()
        ))
    }

    /// THE BUMP, committed on origin/main's tip and pushed there before anything is
    /// staged (`pub stage --commit` stages only an ancestor of origin/main, and merging
    /// an old-base bump into a newer main leaves the lockfile behind): the engine's own
    /// edit (`bin/pub-bump-edit`: the version and the intra-workspace requirements) on
    /// THIS clone, then the lockfile with the prover's `targo`, a check that nothing but
    /// version lines moved, one commit, one push. A push that loses a race rebuilds the
    /// bump on the new tip, three times at most.
    fn bump(&mut self, from: &str, to: &str, prover: &Prover) -> Result<String, Refusal> {
        let edit = self.engine.join("bin/pub-bump-edit");
        if !edit.is_file() {
            return Err(Refusal::new(
                "the engine has no bin/pub-bump-edit",
                format!(
                    "update the engine: git -C {} pull --ff-only",
                    self.engine.display()
                ),
            ));
        }
        let undo = |s: &mut Self| {
            let _ = s.git().git(&["checkout", "-q", "--", "."]);
        };
        for attempt in 1..=3 {
            let tip = self
                .git_out(&["rev-parse", "origin/main"])
                .unwrap_or_default();
            self.main_to(&tip)?;
            self.submodule_sync(Step::Commit)?;
            let cur = self.version_at("HEAD").unwrap_or_default();
            if cur != from {
                return Err(Refusal::new(
                    format!(
                        "origin/main's version moved to {cur} while this release bumped {from}"
                    ),
                    format!("{RELEASE_COMMAND} again (without --bump when {cur} is unreleased)"),
                ));
            }
            let mut cmd = self.scrubbed(&edit);
            cmd.arg(&self.repo).args([to, &cur]).current_dir(&self.repo);
            if self.logged(Step::Commit, &mut cmd).code != 0 {
                return Err(Refusal::new(
                    "bin/pub-bump-edit could not find the version line",
                    "read Cargo.toml's [workspace.package]",
                ));
            }
            let targo = prover.bin.join("targo");
            let mut offline = self.child(&targo, Some(prover));
            offline.args(["update", "--workspace", "--offline", "-q"]);
            if self.logged(Step::Commit, &mut offline).code != 0 {
                let mut online = self.child(&targo, Some(prover));
                online.args(["update", "--workspace", "-q"]);
                if self.logged(Step::Commit, &mut online).code != 0 {
                    undo(self);
                    return Err(Refusal::new(
                        "the lockfile refresh failed (targo update --workspace); the bump is undone",
                        "read the commit step's log",
                    ));
                }
            }
            // Whole paths, one per line: tracked changes, then untracked files.
            // Never `status --porcelain` sliced at a column: `git_out` trims, and
            // the first row's leading status blank went with it (` M Cargo.lock`
            // read `argo.lock`), so every bump that moved the lockfile refused.
            let moved: Vec<String> = [
                &["diff", "--name-only", "HEAD"][..],
                &["ls-files", "--others", "--exclude-standard"][..],
            ]
            .iter()
            .flat_map(|args| {
                self.git_out(args)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|p| p != "Cargo.toml" && p != "Cargo.lock")
            .collect();
            let stray = self
                .git_out(&["diff", "-U0"])
                .unwrap_or_default()
                .lines()
                .any(|l| {
                    (l.starts_with('+') || l.starts_with('-'))
                        && !l.starts_with("+++")
                        && !l.starts_with("---")
                        && !l.contains("version")
                });
            if !moved.is_empty() || stray {
                undo(self);
                return Err(Refusal::new(
                    "the bump moved more than version lines; it is undone",
                    "bump by hand and read the diff",
                )
                .detail(moved));
            }
            let msg = format!("release: {to} — the version and the lockfile move together");
            let mut commit = Command::new("git");
            commit
                .args(["commit", "-q", "-m", &msg, "--", "Cargo.toml", "Cargo.lock"])
                .current_dir(&self.repo);
            if self.logged(Step::Commit, &mut commit).code != 0 {
                return Err(Refusal::new(
                    "could not commit the bump",
                    "read the commit step's log",
                ));
            }
            let mut push = Command::new("git");
            push.args(["push", "-q", "origin", "HEAD:main"])
                .current_dir(&self.repo);
            if self.logged(Step::Commit, &mut push).code == 0 {
                return Ok(self.head());
            }
            if attempt == 3 {
                return Err(Refusal::new(
                    "main moved under the bump three times (each push lost the race)",
                    format!("{RELEASE_COMMAND} --bump again"),
                ));
            }
            let _ = self.remote_git(&["fetch", "-q", "origin", "main"]);
            let _ = self.git().git(&["reset", "-q", "--keep", "origin/main"]);
        }
        Err(Refusal::new(
            "the bump did not land",
            "read the commit step's log",
        ))
    }

    fn move_to(&mut self, commit: &str) -> Result<(), Refusal> {
        self.main_to(commit)?;
        self.submodule_sync(Step::Commit)?;
        match self.dirty() {
            Ok(d) if d.is_empty() => Ok(()),
            Ok(d) => Err(Refusal::new(
                format!("this clone is not clean at {}", short(commit)),
                "clean it (a submodule off its gitlink?), then re-run",
            )
            .detail(d.into_iter().take(5))),
            Err(why) => Err(Refusal::new(why, "run git status there, then re-run")),
        }
    }

    fn own_cutter(&mut self, commit: &str) -> Result<String, String> {
        crate::gates::own_cutter(&self.git(), commit)
    }

    fn reexec(&mut self, prover: &Prover) -> Refusal {
        // as from the operator's shell: R's cutter is built under the release's own
        // scrubbed environment, never the alias's launcher variables or a RUSTFLAGS —
        // from the clone's root, so with this run's arguments as anchored at its start
        let mut cmd = self.scrubbed(prover.bin.join("targo"));
        cmd.args(["--unverified", "ship"])
            .args(&self.argv)
            .current_dir(&self.repo);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            let e = cmd.exec();
            Refusal::new(
                format!("cannot re-run as R's own cutter ({e})"),
                format!("run {RELEASE_COMMAND} again from the clone"),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = cmd;
            Refusal::new(
                "a release runs on macOS",
                format!("run {RELEASE_COMMAND} from a provisioned Mac"),
            )
        }
    }

    fn facts(&mut self, c: &Candidate, prover: &Prover, artifact: Option<&Artifact>) -> Facts {
        let git = self.git();
        let answer = |yes: bool, words: String| {
            if yes {
                Answer::yes(words)
            } else {
                Answer::no(words)
            }
        };
        let trusted = match crate::trust_facts::verdict_for(
            &git,
            &crate::trust_facts::Prover::at(&prover.bin),
            &c.commit,
        ) {
            Ok((yes, words)) => answer(yes, words),
            Err(e) => Answer::unanswered(format!("the trust judge did not answer: {e}")),
        };
        let built = built_answer(&self.repo, artifact, prover);
        let measured = match artifact.filter(|_| built.yes) {
            None => Answer::no("its bundle is not built yet (step 7)"),
            Some(a) => match crate::gates::receipt_store(&git)
                .and_then(|store| crate::gates::measured(&store, &a.macho_sha256, &c.commit))
            {
                Ok((yes, words)) => answer(yes, words),
                Err(e) => Answer::unanswered(format!("the MEASURE judge did not answer: {e}")),
            },
        };
        let attested = std::fs::read_to_string(self.repo.join("publish/.out/attestation.json"))
            .ok()
            .is_some_and(|t| attestation_promotes(&t, &c.commit));
        Facts {
            cut: self.cut_journal(),
            engine: self.engine_rows(Some(&c.commit)),
            attested,
            trusted,
            built,
            measured,
        }
    }

    fn disk(&mut self) -> (Option<u64>, bool) {
        let free = Command::new("df")
            .arg("-Pk")
            .arg(&self.repo)
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .nth(1)
                    .and_then(|l| l.split_whitespace().nth(3))
                    .and_then(|f| f.parse::<u64>().ok())
            })
            .map(|kib| kib / (1024 * 1024));
        let twin = PathBuf::from(format!("{}-verify.noindex", self.repo.display()));
        let warm = std::fs::read_dir(&twin).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (name == "target" || name.starts_with("target-"))
                    && e.file_type().is_ok_and(|t| t.is_dir())
            })
        });
        (free, warm)
    }

    fn preflight(&mut self, c: &Candidate, dry: bool, again: bool) -> Vec<(bool, Refusal)> {
        let mut blocks: Vec<(bool, Refusal)> = Vec::new();
        // R's own pre-claim gates that read only the commit — the changelog, the
        // predecessor's handoff fixtures, the handoff policy — judged for the
        // release's version: a bump not made yet is judged on its parent with the
        // version it will declare, which is all a bump changes. A real run is R's own
        // cutter (it re-execs as R's); a dry run says when it is not.
        let git = self.git();
        if dry && let Err(why) = crate::gates::own_cutter(&git, &c.commit) {
            self.say(&format!(
                "        note: R's pre-claim gates asked of this cutter, not yet R's own (a real \
                 run re-execs as R's): {why}"
            ));
        }
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        let gates: [(&str, crate::ledger::Result<()>); 3] = [
            (
                "changelog",
                crate::gates::changelog_gate_at(&git, commit, version).map(|_| ()),
            ),
            (
                "handoff fixtures",
                crate::gates::handoff_fixture_gate_at(&git, commit, version).map(|_| ()),
            ),
            (
                "handoff policy",
                crate::gates::handoff_policy_gate_at(&git, commit, version).map(|_| ()),
            ),
        ];
        for (name, outcome) in gates {
            if let Err(e) = outcome {
                let why = e.to_string();
                blocks.push((
                    false,
                    Refusal::new(
                        format!("{name} — {}", why.lines().next().unwrap_or("")),
                        why.lines()
                            .find_map(|l| l.trim().strip_prefix("fix:"))
                            .map_or_else(
                                || "the cut's own remedy, above".to_string(),
                                |f| f.trim().to_string(),
                            ),
                    )
                    .detail(
                        why.lines()
                            .skip(1)
                            .filter(|l| !l.trim().starts_with("fix:"))
                            .map(str::to_string),
                    ),
                ));
            }
        }
        // the engine
        let path = std::env::var_os("PATH").unwrap_or_default();
        let gitleaks = std::env::split_paths(&path).any(|d| d.join("gitleaks").is_file());
        if !gitleaks {
            blocks.push((
                false,
                Refusal::new(
                    "gitleaks is not installed — every stage dies at the guard",
                    "brew install gitleaks",
                ),
            ));
        }
        let seam = std::fs::read_to_string(self.engine.join("bin/pub"))
            .is_ok_and(|t| t.contains("run_pre_promote_hook"));
        if !seam {
            self.say(
                "        note: this engine has no pre-promote seam: `pub publish` will not run \
                 publish/pre-promote — the release asked R's own pre-claim gates above itself",
            );
        }
        // A dry run reads only. Asked AGAIN (step 9), R's gates and the engine's
        // readiness are what can have moved: the engine's dry run and the wasm32
        // compile judged THIS R's objects at step 4, and `pub stage`, the step after,
        // runs both again as its own gates before anything is public — a third run
        // between them decides nothing new.
        if dry || again {
            return blocks;
        }
        // step 0, the engine's own dry run (it appends an audit row), then the wasm32
        // engine over the export it left; the engine synced around it
        if let Err(r) = self.engine_sync() {
            blocks.push((false, r));
            return blocks;
        }
        if blocks.iter().any(|(_, r)| r.why.starts_with("gitleaks")) {
            return blocks;
        }
        let export = self.engine_dry_run(&c.commit, &mut blocks);
        if let Err(r) = self.engine_sync() {
            blocks.push((false, r));
        }
        self.wasm32(&c.commit, export.as_deref(), &mut blocks);
        blocks
    }

    fn cut_gates(&mut self, c: &Candidate, creds: &Path) -> Vec<Refusal> {
        crate::publish::pre_publish_gates(&self.repo, &c.version, creds)
            .into_iter()
            .map(|(gate, why)| {
                let first = why.lines().next().unwrap_or("").to_string();
                let fix = why
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("fix:"))
                    .map_or_else(
                        || "the cut's own remedy, above".to_string(),
                        |f| f.trim().to_string(),
                    );
                Refusal::new(format!("{gate} — {first}"), fix).detail(
                    why.lines()
                        .skip(1)
                        .filter(|l| !l.trim().starts_with("fix:"))
                        .map(str::to_string),
                )
            })
            .collect()
    }

    fn fixture_guard(&mut self, prover: &Prover) -> Result<String, Refusal> {
        let log = self.log_path(Step::PreflightEarly);
        let this = &*self;
        // a cold build of minutes: a forwarded child, as every step's is
        let mut run = |cmd: &mut Command| -> std::io::Result<std::process::Output> {
            scrub(cmd);
            cmd.env("TRUST_STAGE2_BIN", &prover.bin);
            let out = this.output_forwarded(cmd)?;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
            {
                let _ = f.write_all(&out.stdout);
                let _ = f.write_all(&out.stderr);
            }
            Ok(out)
        };
        let filter = crate::gates::HANDOFF_FIXTURE_GUARD_FILTER;
        crate::gates::handoff_fixture_guard_with(&prover.bin.join("targo"), &this.repo, &mut run)
            .map(|n| format!("the handoff fixture guard passes ({n} test(s), {filter})"))
            .map_err(|e| {
                let text = e.to_string();
                Refusal::new(
                    format!(
                        "the handoff fixture guard ({filter}) refuses: {} — the cut runs it \
                         before its claim, after the publish",
                        text.lines().next().unwrap_or("")
                    ),
                    format!(
                        "fix the CONSUMER on main, never a fixture (targo --unverified test -p \
                         aterm-gui --lib -- {filter}), then {RELEASE_COMMAND} --commit origin/main"
                    ),
                )
                .detail(text.lines().skip(1).take(8).map(str::to_string))
            })
    }

    fn audit(&mut self, machine_id: &str) -> Vec<Finding> {
        crate::provision::audit(&self.repo, machine_id)
    }

    fn ask(&mut self, words: &AskWords) -> Result<String, String> {
        let line = format!(
            "{} tag=release {} -- {} -- {}",
            words.key, words.title, words.reason, words.step
        );
        notice(&["notice", "ask"], &line)
    }

    fn resolve(&mut self, key: &str) -> Result<String, String> {
        notice(&["notice", "done"], &format!("{key} ok"))
    }

    fn withdraw(&mut self, key: &str) -> Result<String, String> {
        notice(&["notice", "done"], &format!("{key} withdraw"))
    }

    fn trust_pending(&mut self, c: &Candidate, prover: &Prover) -> Vec<String> {
        crate::trust_facts::pending_for(
            &self.git(),
            &crate::trust_facts::Prover::at(&prover.bin),
            &c.commit,
        )
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
    }

    fn trust_lane(&mut self, prover: &Prover, remeasure: bool) -> Ran {
        // The prover first on PATH for the lane alone (it runs the first `targo` on PATH;
        // a global change would hand the stock-pinned public-clone gate the Trust cargo).
        // --stop-if-narrowed: a host whose verifier ceiling cannot verify every library
        // is refused in minutes, before any check (exit 4, NARROWED BY MEMORY).
        let path = std::env::var_os("PATH").unwrap_or_default();
        let joined = std::env::join_paths(
            std::iter::once(prover.bin.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap_or(path);
        let mut cmd = self.child(self.repo.join("tools/trust-gate-all.sh"), Some(prover));
        cmd.env("PATH", joined);
        for k in TRUST_LANE_INPUTS {
            cmd.env(k, "");
        }
        if remeasure {
            cmd.arg("--remeasure");
        }
        cmd.arg("--stop-if-narrowed");
        let mut ran = self.logged(Step::Trust, &mut cmd);
        let text = self.step_log_text(Step::Trust);
        let wanted: Vec<String> = if ran.code == 4 {
            text.lines()
                .filter(|l| {
                    l.starts_with("trust-gate-all: ")
                        && (l.contains(" not verified here: ") || l.contains("NARROWED BY MEMORY"))
                })
                .map(str::to_string)
                .collect()
        } else {
            let summary = [
                "FACTS",
                "VERDICT",
                "TOTAL",
                "NO FLOOR",
                "BUDGET",
                "DRIVER",
                "MEMORY",
                "NOT VERIFIED",
            ];
            let lines: Vec<String> = text
                .lines()
                .filter(|l| summary.iter().any(|s| l.starts_with(s)))
                .map(str::to_string)
                .collect();
            lines[lines.len().saturating_sub(8)..].to_vec()
        };
        if !wanted.is_empty() {
            ran.detail = wanted;
        }
        ran
    }

    /// This machine's gate lock, in its line — a wait of up to three hours that an
    /// interrupt ends within a fraction of a second (`hold_machine_unless`), so the
    /// run's INTERRUPTED line follows at once.
    fn lock(&mut self, caller: &str) -> Result<String, Refusal> {
        let dir = aterm_verify::snapshot::machine_lock_dir(None);
        let who = PathBuf::from(format!("{} ({caller})", self.repo.display()));
        let mut interrupted = || interrupt::caught().is_some();
        match aterm_verify::snapshot::hold_machine_unless(&who, dir.as_deref(), &mut interrupted) {
            Ok(hold) => {
                self.lock = Some(hold);
                Ok(format!(
                    "held {} — peers' gates wait in line until this release ends",
                    dir.map_or_else(
                        || "(no lock dir)".into(),
                        |d| d.join("lock").display().to_string()
                    )
                ))
            }
            // nothing about R was decided: a resumable stop
            Err(why) => Err(Refusal::new(
                format!("could not take this machine's gate lock: {why}"),
                format!(
                    "wait for the gate named there (or stop it if it is wedged), then \
                     {RELEASE_COMMAND}"
                ),
            )
            .could_not_run()),
        }
    }

    fn unlock(&mut self) {
        self.lock = None;
    }

    fn origin_tail(&mut self) -> Option<u64> {
        let text = self.git_out(&["show", "origin/main:RELEASES.ledger"])?;
        crate::ledger::tail(&text).ok().map(|r| r.build)
    }

    fn reserve(&mut self, c: &Candidate) -> Result<Reservation, Refusal> {
        let date = crate::changelog::today_la().map_err(|e| {
            Refusal::new(
                format!("cannot date the changelog's roll: {e}"),
                "check this machine's clock",
            )
        })?;
        let plan = crate::ledger::ClaimPlan {
            version: &c.version,
            now: crate::publish::unix_now(),
            // only the claim's lost race reads it; the cut decides it at its claim
            allow_existing_section: false,
            max_attempts: crate::ledger::MAX_CLAIM_ATTEMPTS,
            source: &c.commit,
        };
        let version = c.version.clone();
        crate::ledger::reserve(&self.git(), &plan, &|source, main| {
            crate::changelog::claim_changelogs(source, main, &version, &date)
        })
        .map(|r| Reservation {
            build: r.build,
            commit: r.commit,
            line: r.line,
        })
        .map_err(|e| {
            Refusal::new(
                format!("could not reserve the release's identity: {e}"),
                format!("fix what it names, then {RELEASE_COMMAND} (nothing was claimed)"),
            )
        })
    }

    fn build(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<Artifact, Refusal> {
        // R's own cutter (this binary), as a step's child: `ship build` reads the
        // journal's reservation and builds its bundle once into the release cache.
        let mut cmd = self.cut_command(&["build".to_string()], prover);
        let ran = self.logged(Step::Build, &mut cmd);
        if ran.code != 0 {
            return Err(Refusal::new(
                format!("the build did not finish (ship build exited {})", ran.code),
                format!(
                    "read its log; a red build is fixed on main, then {RELEASE_COMMAND} \
                     --commit origin/main (nothing was claimed)"
                ),
            )
            .detail(ran.detail));
        }
        let identity = crate::artifact::Identity {
            version: c.version.clone(),
            build: reserved.build,
            commit: reserved.commit.clone(),
        };
        // The key the child built under: its trustc is the release's prover (the
        // child's TRUST_STAGE2_BIN), its rustc this machine's upstream stable.
        let found = crate::buildplan::stable_rustc_commit()
            .map(|rustc| {
                crate::artifact::key(
                    &identity,
                    &crate::artifact::Compilers {
                        trustc: prover.commit.clone(),
                        rustc,
                    },
                )
            })
            .and_then(|key| {
                let store = crate::artifact::store(&self.repo)?;
                crate::artifact::read(&store, &key).map(|c| c.map(|c| (store, c)))
            });
        match found {
            Ok(Some((store, complete))) if complete.trustc == prover.commit => Ok(Artifact {
                app: complete.app(&store),
                key: complete.key,
                macho_sha256: complete.macho_sha256,
                build: complete.build,
            }),
            Ok(Some((_, complete))) => Err(Refusal::new(
                format!(
                    "the build ran with trustc {}, not this release's prover {}",
                    complete.trustc, prover.commit
                ),
                format!("{RELEASE_COMMAND} (it builds with the release's prover)"),
            )),
            Ok(None) => Err(Refusal::new(
                "the build ended, and the release cache holds no COMPLETE bundle for it",
                format!("read its log, then {RELEASE_COMMAND}"),
            )
            .detail(ran.detail)),
            Err(e) => Err(Refusal::new(
                format!("the built bundle cannot be read back: {e}"),
                format!("read its log, then {RELEASE_COMMAND}"),
            )),
        }
    }

    fn measure(&mut self, prover: &Prover, app: &Path) -> (Ran, bool) {
        // Under the release's lock; the gate takes a lock of its own. It judges the
        // bundle step 7 built — the bytes the cut signs — and builds no aterm of its own.
        let mut cmd = self.child(self.repo.join("tools/verify.sh"), Some(prover));
        cmd.arg("--measure")
            .arg(app)
            .arg("--machine-lock-dir")
            .arg(self.state.join("gate-lock"));
        let mut ran = self.logged(Step::Measure, &mut cmd);
        let text = self.step_log_text(Step::Measure);
        let lines: Vec<String> = text
            .lines()
            .filter(|l| {
                l.contains("MEASURE")
                    || l.contains("VERIFY:")
                    || l.contains("verify: COULD NOT RUN")
            })
            .map(str::to_string)
            .collect();
        if !lines.is_empty() {
            ran.detail = lines[lines.len().saturating_sub(6)..].to_vec();
        }
        let stage_starved = text.lines().any(|l| {
            l.contains("MEASURE tier — NOT MEASURED: `")
                && l.split_once("NOT MEASURED: `")
                    .is_some_and(|(_, rest)| rest.contains("` could not run:"))
        });
        (ran, stage_starved)
    }

    fn stage(&mut self, c: &Candidate, prover: &Prover) -> Result<Ran, Refusal> {
        self.engine_sync()?;
        let mut cmd = self.child(self.engine.join("bin/pub"), Some(prover));
        cmd.args(["stage", "aterm", "--commit", &c.commit, "--repo-path"])
            .arg(&self.repo);
        let ran = self.logged(Step::Stage, &mut cmd);
        // the stage appended its audit row: recorded now, or the step refuses
        self.engine_sync()?;
        Ok(ran)
    }

    fn publish(&mut self, prover: &Prover) -> Result<Ran, Refusal> {
        self.engine_sync()?;
        let mut cmd = self.child(self.engine.join("bin/pub"), Some(prover));
        cmd.args(["publish", "aterm", "--repo-path"])
            .arg(&self.repo);
        Ok(self.logged(Step::Publish, &mut cmd))
    }

    fn cut(&mut self, args: &[String], prover: &Prover) -> Ran {
        let mut cmd = self.cut_command(args, prover);
        self.logged(Step::Cut, &mut cmd)
    }

    fn after_cut(&mut self, version: &str) -> Result<AfterCut, Refusal> {
        let slug = self.slug();
        let tag = format!("v{version}");
        let read = |s: &Self, t: &str| s.release_json(&slug, t);
        let mut problems = match read(self, &tag) {
            Ok(rel) => release_shape(&rel, version),
            Err(why) => {
                return Err(
                    Refusal::new(why, format!("{RELEASE_COMMAND} again when gh answers"))
                        .could_not_run(),
                );
            }
        };
        problems.extend(pointer_verdict(
            self.pointer_problems(&slug, &tag, version),
        )?);
        // The site links the newest stable release carrying its DMG, not a named tag —
        // as `publish/site-sync.py choose-download` elects it, asked here, never
        // restated: when that is another release, its shape is checked too.
        let site_tag = match self.site_tag(&slug) {
            Ok(t) => t,
            Err(why) => {
                return Err(Refusal::new(
                    format!(
                        "{why}, so the release the site would link is unknown and cannot be \
                         shape-checked — the site was NOT touched"
                    ),
                    format!("{RELEASE_COMMAND} again when gh answers"),
                )
                .could_not_run());
            }
        };
        if site_tag != tag {
            match read(self, &site_tag) {
                Ok(rel) => {
                    let v = site_tag.trim_start_matches('v');
                    problems.extend(
                        release_shape(&rel, v).into_iter().map(|p| {
                            format!("{site_tag} (the release alab.systems would link): {p}")
                        }),
                    );
                }
                Err(why) => {
                    return Err(Refusal::new(
                        why,
                        format!("{RELEASE_COMMAND} again when gh answers"),
                    )
                    .could_not_run());
                }
            }
        }
        if !problems.is_empty() {
            return Err(shape_refusal(&slug, &tag, problems));
        }
        let site = self.site(&slug)?;
        Ok(AfterCut {
            note: format!(
                "{tag} carries the lean shape as GitHub serves it; {}",
                site.note
            ),
            ..site
        })
    }

    fn latest(&mut self) -> Result<String, String> {
        let slug = self.slug();
        let out = self
            .output_forwarded(Command::new("gh").args([
                "api",
                &format!("repos/{slug}/releases/latest"),
                "--jq",
                ".tag_name",
            ]))
            .map_err(|e| format!("cannot run gh: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

/// `aterm ctl <verb> <line>` — the owner's aterm, from this shell: `Ok(its reply)` only
/// when it answered `OK` (the row is up, or resolved), else why not.
fn notice(verb: &[&str], line: &str) -> Result<String, String> {
    let out = Command::new("aterm")
        .arg("ctl")
        .args(verb)
        .args(line.split(' '))
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) => {
            let reply = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && reply.starts_with("OK") {
                Ok(reply)
            } else {
                Err(format!(
                    "{}{}",
                    reply,
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
        }
        Err(e) => Err(format!("no aterm to ask ({e})")),
    }
}

/// The engine's ledger for one commit: its newest aterm row, whether `commit` has one,
/// and that row's `release_rev`.
#[must_use]
pub fn engine_rows_in(mappings: &str, commit: Option<&str>) -> EngineRows {
    let Ok(value) = aterm_json::from_str::<aterm_json::Value>(mappings) else {
        return EngineRows::default();
    };
    let Some(rows) = value
        .get("mappings")
        .and_then(|m| m.get("aterm"))
        .and_then(aterm_json::Value::as_object)
    else {
        return EngineRows::default();
    };
    let at = |row: &aterm_json::Value| {
        row.get("verified_at")
            .and_then(aterm_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let newest = rows
        .iter()
        .max_by(|a, b| at(a.1).cmp(&at(b.1)))
        .map(|(sha, _)| sha.clone());
    let mine = commit.and_then(|c| rows.get(c));
    EngineRows {
        newest,
        has: mine.is_some(),
        rev: mine
            .and_then(|row| row.get("release_rev"))
            .and_then(aterm_json::Value::as_str)
            .map(str::to_string),
    }
}

/// A promotable attestation of `commit`: its source is `commit`, and `pub publish`
/// would promote it (`public_clone` passed, or skipped on a source-only repo).
#[must_use]
pub fn attestation_promotes(json: &str, commit: &str) -> bool {
    let Ok(a) = aterm_json::from_str::<aterm_json::Value>(json) else {
        return false;
    };
    let source = a
        .get("source")
        .and_then(|s| s.get("commit"))
        .and_then(aterm_json::Value::as_str);
    let gate = a
        .get("guards")
        .and_then(|g| g.get("public_clone"))
        .and_then(aterm_json::Value::as_str);
    let mode = a.get("release_mode").and_then(aterm_json::Value::as_str);
    source == Some(commit)
        && (gate == Some("pass") || (gate == Some("skipped") && mode == Some("source-only")))
}

/// Run a release in the clone at `repo`; the exit code.
#[must_use]
pub fn run(repo: &Path, opts: &crate::release::ReleaseOptions) -> i32 {
    interrupt::install();
    // every relative path flag, anchored ONCE, where the release was started: the steps
    // run from the clone's root, and so does the re-exec as R's own cutter
    let opts = match std::env::current_dir() {
        Ok(cwd) => opts.anchored(&cwd),
        Err(_) => opts.clone(),
    };
    let mut real = match Real::new(repo, opts.engine.as_deref(), opts.dry_run, opts.verbose) {
        Ok(real) => real,
        Err(why) => {
            eprintln!("release: {why}");
            return 2;
        }
    };
    real.argv = opts.anchored_args(std::env::args_os().skip(1).collect());
    let home = real.home.clone();
    crate::release::run_with(&mut real, &opts, &home).code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pointer's first hop: only a 404 or a 302 elsewhere is the release's
    /// SHAPE (the refusal whose remedy ends a published release); an answer that
    /// says nothing about the release — a 5xx, a rate-limit 403 — is a read that
    /// failed (COULD NOT RUN: run it again), never a reason to ship a successor.
    #[test]
    fn a_pointer_that_could_not_be_read_is_could_not_run() {
        let refused = pointer_verdict(Err("https://x/latest could not be read: timeout".into()))
            .expect_err("a failed read refuses");
        assert!(refused.could_not_run, "{refused:?}");
        assert_eq!(
            pointer_verdict(Ok(vec!["404".into()])).expect("shape problems pass through"),
            ["404"]
        );
    }

    #[test]
    fn only_a_404_or_a_302_elsewhere_is_the_pointers_shape() {
        let (ptr, want) = ("https://x/latest", "https://x/v0.6.0");
        assert_eq!(
            pointer_hop_problem(ptr, want, "v0.6.0", 302, Some(want)),
            Ok(None)
        );
        assert!(matches!(
            pointer_hop_problem(ptr, want, "v0.6.0", 404, None),
            Ok(Some(p)) if p.contains("404")
        ));
        assert!(matches!(
            pointer_hop_problem(ptr, want, "v0.6.0", 302, Some("https://x/v0.5.0")),
            Ok(Some(p)) if p.contains("another release holds")
        ));
        for code in [500, 502, 503, 403, 200] {
            assert!(
                pointer_hop_problem(ptr, want, "v0.6.0", code, None).is_err(),
                "HTTP {code} is a read that failed"
            );
        }
    }

    fn release(assets: &[(&str, u64)], body: &str) -> aterm_json::Value {
        let assets: Vec<String> = assets
            .iter()
            .map(|(n, s)| format!("{{\"name\":\"{n}\",\"size\":{s}}}"))
            .collect();
        aterm_json::from_str(&format!(
            "{{\"tag_name\":\"v0.6.0\",\"body\":{body:?},\"assets\":[{}]}}",
            assets.join(",")
        ))
        .unwrap()
    }

    fn lean() -> Vec<(&'static str, u64)> {
        vec![
            ("aterm-0.6.0.dmg", 40_600_000),
            ("aterm-0.6.0-mac.zip", 38_000_000),
            ("aterm.dmg", 40_600_000),
            ("aterm-mac.zip", 38_000_000),
            ("aterm-0.6.0.dmg.sha256", 90),
            ("aterm-0.6.0-mac.zip.sha256", 90),
            ("aterm.dmg.sha256", 90),
            ("aterm-mac.zip.sha256", 90),
            ("aterm-appcast.toml", 900),
            ("aterm-appcast.toml.sig", 100),
            ("aterm-machines.toml", 900),
            ("aterm-machines.toml.sig", 100),
        ]
    }

    /// The outside look's every check (check-release-shape's, ported): the lean shape
    /// passes; each way a release is out of shape is named.
    #[test]
    fn a_release_is_judged_by_the_lean_shape_github_serves() {
        assert_eq!(
            release_shape(&release(&lean(), "notes"), "0.6.0"),
            Vec::<String>::new()
        );
        let mut seeded = lean();
        seeded[0].1 = 1_070_000_000;
        seeded.push(("aterm-0.6.0-x86_64.dmg", 9));
        seeded.retain(|(n, _)| *n != "aterm-mac.zip.sha256");
        seeded[2].1 = 1;
        let bad = release_shape(&release(&seeded, "the batteries-included image"), "0.6.0");
        for needle in [
            "over the 200 MB lean ceiling",
            "a twin must be a byte copy",
            "aterm-mac.zip.sha256 is missing",
            "retired macOS asset(s) present: aterm-0.6.0-x86_64.dmg",
            "describes a seeded download (batteries-included)",
        ] {
            assert!(bad.iter().any(|p| p.contains(needle)), "{needle}: {bad:#?}");
        }
        let mut linux = lean();
        linux.push(("aterm-0.6.0-linux-x86_64", 40_000_000));
        linux.push(("aterm-0.6.0-linux-riscv64", 40_000_000));
        linux.push(("aterm-0.5.0-linux-aarch64", 40_000_000));
        linux.push(("aterm-0.6.0-linux-aarch64.tar.gz", 1_000));
        linux.retain(|(n, _)| *n != "aterm-machines.toml.sig");
        let bad = release_shape(&release(&linux, ""), "0.6.0");
        for needle in [
            "unsupported or noncanonical architecture",
            "names another version",
            "aterm-0.6.0-linux-aarch64.tar.gz.sha256 is missing",
            "Linux native proof object aterm-machines.toml.sig",
        ] {
            assert!(bad.iter().any(|p| p.contains(needle)), "{needle}: {bad:#?}");
        }
    }

    #[test]
    fn the_appcast_the_pointer_serves_names_this_version_and_binds_its_url() {
        let slug = "alabsystems/aterm";
        let good = "version = \"0.6.0\"\ndmg = \"aterm-0.6.0.dmg\"\n\
                    url = \"https://github.com/alabsystems/aterm/releases/download/v0.6.0/aterm-0.6.0.dmg\"\n";
        assert!(appcast_problems(good, slug, "v0.6.0", "0.6.0").is_empty());
        assert!(appcast_problems(good, slug, "v0.7.0", "0.7.0")[0].contains("does not name"));
        let unbound = good.replace("download/v0.6.0", "download/v0.5.0");
        assert!(
            appcast_problems(&unbound, slug, "v0.6.0", "0.6.0")[0].contains("refuses a manifest")
        );
    }

    /// Step 0's verdicts: guards ok; a run that heard no guard; one block per refusing
    /// guard with its location lines; a refusal before any guard.
    #[test]
    fn the_engines_dry_run_is_read_as_its_own_verdict() {
        let ok = "== guard ok: forbidden content\n== guard ok: gitleaks\n";
        assert_eq!(engine_dry_run_verdict(0, ok, "r", true), (vec![], true));
        let (silent, _) = engine_dry_run_verdict(0, "exported\n", "r", false);
        assert!(silent[0].why.contains("without reporting a single guard"));
        let fails = "== guard ok: x\n== GUARD FAIL: forbidden content (2 hits)\n  a.rs:3\n  b.rs:9\n\
                     == GUARD FAIL: private references\n  c.md:1\nFAIL: guards failed\n";
        let (blocks, export) = engine_dry_run_verdict(1, fails, "r", true);
        assert!(export);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].why, "forbidden content (2 hits)");
        assert_eq!(blocks[0].detail, ["a.rs:3", "b.rs:9"]);
        let early = "FAIL: the export does not pin the Trust toolchain the dev commit pins\n\
                     FAIL: transforms failed\n";
        let (blocks, export) = engine_dry_run_verdict(1, early, "r", false);
        assert!(!export);
        assert!(
            blocks[0]
                .why
                .contains("before any guard ran (exit 1): the export does not pin")
        );
    }

    #[test]
    fn the_wasm32_clause_is_read_out_of_the_engines_own_line() {
        // the committed line's shape (publish/config.sh, which the public snapshot
        // withholds, so it is quoted here rather than read)
        let good = "CHECK_CMD_DEFAULT=\"rustup run trust targo trust check && rustup target add \
                    --toolchain 1.95.0 wasm32-unknown-unknown && RUSTFLAGS='-C \
                    target-feature=+simd128' RUSTC=$(rustup which --toolchain 1.95.0 rustc) \
                    rustup run 1.95.0 cargo check --locked --target wasm32-unknown-unknown -p \
                    aterm-wasm\"\n";
        let clause = wasm_clause(good).unwrap();
        assert_eq!(clause.pin, "1.95.0");
        assert!(
            clause
                .clause
                .starts_with("RUSTFLAGS='-C target-feature=+simd128' RUSTC=")
        );
        for (config, needle) in [
            ("CHECK_CMD_DEFAULT=\"targo check\"\n", "no wasm32 clause"),
            (
                &good.replace(
                    "rustup target add --toolchain 1.95.0 wasm32-unknown-unknown && ",
                    "",
                ),
                "does not add the wasm32 target",
            ),
            (
                &good.replace("RUSTC=$(rustup which --toolchain 1.95.0 rustc) ", ""),
                "does not name 1.95.0's own rustc",
            ),
            (
                &good.replace("rustup run 1.95.0 cargo", "cargo"),
                "names no stock channel",
            ),
        ] {
            let why = wasm_clause(config).unwrap_err().why;
            assert!(why.contains(needle), "{needle}: {why}");
        }
    }

    #[test]
    fn the_engines_ledger_and_attestation_are_read_as_the_release_reads_them() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let json = format!(
            "{{\"mappings\":{{\"aterm\":{{\"{a}\":{{\"verified_at\":\"2026-10-01T00:00:00Z\",\
             \"release_rev\":\"e1\"}},\"{b}\":{{\"verified_at\":\"2026-10-02T00:00:00Z\"}}}}}}}}"
        );
        let rows = engine_rows_in(&json, Some(&a));
        assert_eq!(
            rows,
            EngineRows {
                newest: Some(b.clone()),
                has: true,
                rev: Some("e1".into())
            }
        );
        assert!(!engine_rows_in(&json, Some("c")).has);
        assert_eq!(engine_rows_in("not json", None), EngineRows::default());
        let attestation = |gate: &str, mode: &str| {
            format!(
                "{{\"source\":{{\"commit\":\"{a}\"}},\"guards\":{{\"public_clone\":\"{gate}\"}},\
                 \"release_mode\":\"{mode}\"}}"
            )
        };
        assert!(attestation_promotes(&attestation("pass", "full"), &a));
        assert!(attestation_promotes(
            &attestation("skipped", "source-only"),
            &a
        ));
        assert!(!attestation_promotes(&attestation("skipped", "full"), &a));
        assert!(!attestation_promotes(&attestation("pass", "full"), &b));
    }

    /// `--engine` reaches every child: the cut, the stage and `pub publish`'s
    /// pre-promote (`ship check`) read the one engine ledger the release judged, never
    /// `~/Developer/publication` by default.
    #[test]
    fn the_engine_the_release_judges_is_the_one_every_child_reads() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = Real::new(
            dir.path(),
            Some(Path::new("/other/publication")),
            true,
            false,
        )
        .expect("a backend");
        let prover = Prover {
            bin: PathBuf::from("/store/trust/9192/bin"),
            commit: "321aaeda7".into(),
            source: "the store".into(),
        };
        let engine = (
            std::ffi::OsStr::new("PUBLICATION_ENGINE"),
            Some(std::ffi::OsStr::new("/other/publication")),
        );
        let cut = real.cut_command(&["cut".into(), "--mac-only".into()], &prover);
        let envs: Vec<_> = cut.get_envs().collect();
        assert!(envs.contains(&engine), "{envs:?}");
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new("TRUST_STAGE2_BIN"),
                Some(std::ffi::OsStr::new("/store/trust/9192/bin"))
            )),
            "{envs:?}"
        );
        let stage = real.child("/other/publication/bin/pub", Some(&prover));
        assert!(stage.get_envs().any(|kv| kv == engine));
        let edit = real.scrubbed("/other/publication/bin/pub-bump-edit");
        assert!(edit.get_envs().any(|kv| kv == engine));
    }

    /// A step's child under `--verbose`: both its streams reach the step's log on one
    /// pipe, in the order written, and its exit code is the step's.
    #[test]
    fn a_verbose_step_logs_both_streams_in_the_order_written() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut real = Real::new(dir.path(), None, true, true).expect("a backend");
        let mut cmd = real.child("/bin/sh", None);
        cmd.args(["-c", "echo one; echo two >&2; echo three; exit 7"]);
        let ran = real.logged(Step::Sync, &mut cmd);
        assert_eq!(ran.code, 7, "{ran:?}");
        let log = std::fs::read_to_string(real.log_path(Step::Sync)).unwrap();
        let at = |needle: &str| {
            log.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {log}"))
        };
        assert!(
            at("one\n") < at("two\n") && at("two\n") < at("three\n"),
            "{log}"
        );
    }

    /// `nohup` PROTECTS A RUN: a signal the release was started ignoring stays ignored
    /// (and so for its children), while every other one it catches. The probe is this
    /// test binary re-run as the release would start: under `nohup`, and — the control —
    /// with SIGHUP at its default, whatever the gate running this test inherited.
    #[cfg(unix)]
    #[test]
    fn an_inherited_sig_ign_is_kept_and_the_others_are_caught() {
        const PROBE: &str = "ATERM_RELEASE_SIGNAL_PROBE";
        const SIG_DFL: usize = 0;
        const SIG_IGN: usize = 1;
        unsafe extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        if let Some(mode) = std::env::var_os(PROBE) {
            if mode == "control" {
                // SAFETY: SIG_DFL is a disposition, no handler of ours.
                unsafe {
                    signal(interrupt::SIGHUP, SIG_DFL);
                }
            }
            interrupt::install();
            // SAFETY: as above; the disposition read is put straight back.
            let hup = unsafe {
                let now = signal(interrupt::SIGHUP, SIG_IGN);
                signal(interrupt::SIGHUP, now);
                now
            };
            let hup = match hup {
                SIG_IGN => "ignored",
                SIG_DFL => "default",
                _ => "caught",
            };
            println!("PROBE hup={hup}");
            interrupt::send(std::process::id().cast_signed(), interrupt::SIGTERM);
            let bound = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while interrupt::caught().is_none() {
                assert!(std::time::Instant::now() < bound, "SIGTERM never caught");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            println!("PROBE term={}", interrupt::caught().unwrap_or(0));
            std::process::exit(0);
        }
        let probe = |nohup: bool, mode: &str| {
            let exe = std::env::current_exe().unwrap();
            let mut cmd = if nohup {
                let mut cmd = Command::new("nohup");
                cmd.arg(exe);
                cmd
            } else {
                Command::new(exe)
            };
            let out = cmd
                .args([
                    "--exact",
                    "release_steps::tests::an_inherited_sig_ign_is_kept_and_the_others_are_caught",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(PROBE, mode)
                .stdin(Stdio::null())
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let kept = probe(true, "nohup");
        assert!(kept.contains("PROBE hup=ignored"), "{kept}");
        assert!(kept.contains("PROBE term=15"), "{kept}");
        let control = probe(false, "control");
        assert!(control.contains("PROBE hup=caught"), "{control}");
        assert!(control.contains("PROBE term=15"), "{control}");
    }

    /// The interrupt signals a step's group member by member, and never a Trust trustd
    /// or what it runs — even one living in the step's group (a seal before trust
    /// `41f912aa5f4` leaves it in its launcher's) — nor anything outside the group.
    #[test]
    fn the_interrupt_spares_a_trustd_and_its_solvers() {
        let listing = "\
            1     0     1 /sbin/launchd\n\
          500   400   500 /bin/sh\n\
          501   500   500 /Users//x/Library/Application Support/aterm/pkg/store/trust/9192/bin/targo\n\
          502   501   500 /Users//x/Library/Application Support/aterm/pkg/store/trust/9192/bin/trustd\n\
          503   502   500 z3\n\
          504   503   500 z3-worker\n\
          505   500   500 sleep\n\
          600   500   600 /usr/libexec/trustd\n\
          601     1   777 /bin/sh\n\
          garbage line\n";
        assert_eq!(group_members(listing, 500), [500, 501, 505]);
        // another group's members, and a group holding nothing, are no one's
        assert_eq!(group_members(listing, 777), [601]);
        assert!(group_members(listing, 999).is_empty());
    }

    #[test]
    fn kept_audit_rows_go_back_in_time_order_and_gitlinks_are_listed() {
        let text = "{\"at\":\"1\"}\n{\"at\":\"3\"}\n";
        let kept = vec!["{\"at\":\"2\"}".to_string(), "{\"at\":\"3\"}".to_string()];
        assert_eq!(
            audit_rows_merged(text, &kept),
            "{\"at\":\"1\"}\n{\"at\":\"2\"}\n{\"at\":\"3\"}\n"
        );
        let listing = "100644 blob 1111\tCargo.toml\n160000 commit abcd\tvendor/astream\n";
        assert_eq!(
            gitlinks(listing),
            [("abcd".to_string(), "vendor/astream".to_string())]
        );
        assert_eq!(parse_offset("+0200"), Some(7200));
        assert_eq!(parse_offset("-0730"), Some(-27000));
        assert!(clears("RUSTFLAGS") && clears("CARGO_TARGET_DIR") && !clears("PATH"));
        // what `targo run` hands the alias's process never reaches a step
        for name in [
            "CARGO",
            "CARGO_PKG_NAME",
            "TARGO_MANIFEST_DIR",
            "TRUST_TARGO_FRONTEND",
        ] {
            assert!(launcher_env(name), "{name}");
        }
        assert!(!launcher_env("CARGO_HOME") && !launcher_env("HOME"));
    }
}
