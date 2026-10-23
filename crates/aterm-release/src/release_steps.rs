// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The real [`crate::release::Backend`]: what `targo --unverified ship release` asks of git, the publication engine, verification records, the release cache, the children it runs (the build, `verify release`, `pub stage`/`pub publish`, the cut), the site sync, the channel and the owner's aterm.

use aterm_uds::spawnfd::{Command, Stdio, command};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::gates::{Outcome, Precheck, PrecheckGate};
use crate::interrupt;
use crate::ledger::{BYTE_EXACT, GitCli, GitRunner};
use crate::provision::{AskWords, Finding};
use crate::release::{
    AfterCut, Answer, Artifact, Backend, Built, Candidate, CutState, EngineNow, EngineRows, Facts,
    Prover, RELEASE_COMMAND, Ran, Refusal, Reservation, Step,
};

/// Where `ship check`'s export and wasm32 gates keep what they leave behind (each run's
fn preflight_cache(repo: &Path) -> Result<PathBuf, String> {
    let dir = crate::snapshot::CacheRoot::for_repo(repo)?.preflight();
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The variable `publish/pre-promote` reads for the running release's own cutter: set on `pub publish`, whose engine passes it through to the hook.
pub const RELEASE_CUTTER_ENV: &str = "ATERM_RELEASE_CUTTER";

/// A child the release waits on, made unable to block on a terminal: git asks no
fn detach(cmd: &mut Command) {
    crate::credential_wait::ask_no_one(cmd);
    #[cfg(unix)]
    cmd.setsid(true);
}

/// Write `step`'s child `pid`'s note in `release` ([`crate::release::child_note`]): the
fn note_child_in(release: &Path, step: Step, pid: u32) -> PathBuf {
    let started = started_of(pid).unwrap_or_default();
    let note = crate::release::child_note(release, pid);
    let _ = std::fs::create_dir_all(release);
    let _ = std::fs::write(
        &note,
        format!("pid {pid}\nstarted {started}\nstep {}\n", step.name()),
    );
    note
}

/// The release's child rules for a step that runs in this process — the after-cut step's site sync: every child it starts, git's included, is started as [`Real::spawn_forwarded`] starts a step's (none once interrupted, a session of its own with git's prompt off, the interrupt handed to it, noted while it runs).
pub(crate) struct StepChildren {
    /// Where the running child is noted, and under which step; `None` on a dry run.
    pub(crate) noted: Option<(PathBuf, Step)>,
}

impl crate::site::Launch for StepChildren {
    fn interrupted(&self) -> Option<i32> {
        interrupt::caught()
    }

    fn spawn(
        &self,
        cmd: &mut aterm_uds::spawnfd::Command,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, crate::site::Guard)> {
        if interrupt::caught().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "not started: the release was interrupted",
            ));
        }
        crate::credential_wait::ask_no_one(cmd).setsid(true);
        let child = cmd.spawn()?;
        let running = interrupt::Running::enter(child.id());
        let note = self
            .noted
            .as_ref()
            .map(|(release, step)| note_child_in(release, *step, child.id()));
        Ok((
            child,
            Box::new(Forwarded {
                _running: running,
                note,
            }),
        ))
    }
}

/// A pid's start time (`ps -o lstart=`), `None` when no such process lives.
fn started_of(pid: u32) -> Option<String> {
    let out = {
        command("ps")
            .args(["-p", &pid.to_string(), "-o", "lstart="])
            .output()
            .ok()?
    };
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
    /// The step the run is at ([`Backend::entered`]): what a child is noted under.
    step: Step,
    /// This process's arguments (after the program) with every path flag's value
    argv: Vec<std::ffi::OsString>,
}

/// A forwarded child's guard ([`Real::spawn_forwarded`]): while it lives, the interrupt
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
                .map_or_else(|| home.join("publication"), PathBuf::from)
        });
        let engine = std::env::current_dir().map_or_else(
            |_| engine.clone(),
            |cwd| crate::release::anchor(&engine, &cwd),
        );
        let state = home.join(".local/state/aterm-release");
        let stamp = command("date")
            .arg("+%Y%m%dT%H%M%S")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| now().to_string());
        let logs = if dry {
            static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            std::env::temp_dir().join(format!("aterm-release-dry-{}-{nth}", std::process::id()))
        } else {
            state
                .join("logs")
                .join(format!("{stamp}-{}", std::process::id()))
        };
        std::fs::create_dir_all(&logs).map_err(|e| format!("create {}: {e}", logs.display()))?;
        let offset = command("date")
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
        let mut cmd = command("git");
        cmd.arg("-C").arg(&self.repo).args(args);
        self.output_forwarded(&mut cmd)
    }

    /// `git -C <engine>` (its fetch and push among them): forwarded.
    fn engine_git(&self, args: &[&str]) -> Option<String> {
        let mut cmd = command("git");
        cmd.arg("-C").arg(&self.engine).args(args);
        let out = self.output_forwarded(&mut cmd).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `git -C <engine> <args>` whose exit code is its answer: the code and the output,
    fn engine_git_code(&self, args: &[&str]) -> Result<(i32, std::process::Output), String> {
        let e = self.engine.display();
        let mut cmd = command("git");
        cmd.arg("-C").arg(&self.engine).args(args);
        let out = self
            .output_forwarded(&mut cmd)
            .map_err(|err| format!("git -C {e} {}: {err}", args.join(" ")))?;
        let code = out
            .status
            .code()
            .ok_or_else(|| format!("git -C {e} {} was ended by a signal", args.join(" ")))?;
        Ok((code, out))
    }

    /// `rev` in the engine, as the full sha of a commit it holds: `Ok(None)` when it holds
    fn engine_commit(&self, rev: &str) -> Result<Option<String>, String> {
        let e = self.engine.display();
        let spec = format!("{rev}^{{commit}}");
        let (code, out) = self.engine_git_code(&["rev-parse", "--verify", "-q", &spec])?;
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match code {
            0 if !sha.is_empty() => Ok(Some(sha)),
            1 if sha.is_empty() => Ok(None),
            _ => Err(format!(
                "git -C {e} rev-parse {spec} exited {code}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// The one spawn path for a child the release waits on: an interrupted release starts
    fn spawn_forwarded(
        &self,
        cmd: &mut Command,
    ) -> std::io::Result<(aterm_uds::spawnfd::Child, Forwarded)> {
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
    fn scrubbed(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = command(program);
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
    fn cut_command(&self, args: &[String], prover: &Prover) -> Command {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aterm-release"));
        let mut cmd = self.child(exe, Some(prover));
        cmd.args(args);
        cmd
    }

    /// `pub publish aterm --repo-path <this clone>`, a child with the release's prover and
    fn publish_command(&self, prover: &Prover) -> Command {
        let mut cmd = self.child(self.engine.join("bin/pub"), Some(prover));
        cmd.args(["publish", "aterm", "--skip-site", "--repo-path"])
            .arg(&self.repo);
        if let Ok(exe) = std::env::current_exe() {
            cmd.env(RELEASE_CUTTER_ENV, exe);
        }
        cmd
    }

    /// Run `cmd` as a step's child, through [`Real::spawn_forwarded`] — its output into `step`'s log (and, as it arrives, to the terminal under `--verbose`).
    pub(crate) fn logged(&self, step: Step, cmd: &mut Command) -> Ran {
        let stream = self.verbose;
        self.logged_to(step, cmd, stream)
    }

    /// [`Real::logged`], its output on the terminal too when `stream`.
    fn logged_to(&self, step: Step, cmd: &mut Command, stream: bool) -> Ran {
        let log = self.step_log(step);
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
        let pipe = if stream {
            let made = std::io::pipe().and_then(|(r, w)| Ok((r, w.try_clone()?, w)));
            match made {
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
        if let Some(finished) = pumped {
            let _ = finished.recv_timeout(std::time::Duration::from_secs(5));
        }
        drop(forwarded);
        Ran {
            code,
            detail: tail(&log, 8),
        }
    }

    /// Note the step's child `pid` while it runs ([`crate::release::child_note`], a real run
    fn note_child(&self, pid: u32) -> Option<PathBuf> {
        (!self.scratch_logs).then(|| note_child_in(&self.release, self.step, pid))
    }

    /// `step`'s log: `<logs>/<nn>-<step>.log`.
    fn step_log(&self, step: Step) -> PathBuf {
        self.logs
            .join(format!("{:02}-{}.log", step.number(), step.name()))
    }

    /// The log of the child named `what` that `step` runs beside its main one, beside
    fn side_log(&self, step: Step, what: &str) -> PathBuf {
        self.logs
            .join(format!("{:02}-{}-{what}.log", step.number(), step.name()))
    }

    /// The engine to its own origin/main, its appended audit rows kept (the pub-sync
    fn engine_sync(&self) -> Result<(), Refusal> {
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

    /// The submodule at its gitlink, and that gitlink on the branch `.gitmodules`
    fn submodule_sync(&self, step: Step) -> Result<(), Refusal> {
        let mut cmd = command("git");
        cmd.args(["submodule", "update", "--init", "vendor/astream"])
            .current_dir(&self.repo);
        let ran = self.logged(step, &mut cmd);
        if ran.code != 0 {
            return Err(Refusal::new(
                "git submodule update --init vendor/astream failed — the workspace does not \
                 resolve without it",
                "check access to the astream repository, then re-run",
            )
            .detail(ran.detail));
        }
        crate::gates::submodules_on_declared_branch(&self.git()).map_err(|e| {
            Refusal::new(
                e.to_string(),
                "land the astream change on astream's main and bump vendor/astream to that \
                 commit (vendor/README-astream.md), then re-run",
            )
        })
    }

    /// The one clean-tree judge ([`crate::clean`]): the tree's own `.gitignore` rules,
    fn dirty(&self) -> Result<Vec<String>, String> {
        crate::gates::dirty_paths(&self.repo).map_err(|e| e.to_string())
    }

    /// Why the clone is not at `commit`'s tree and clean — what a verified record vouches
    fn off_tree(&self, commit: &str) -> Option<Vec<String>> {
        let want = self.git_out(&["rev-parse", &format!("{commit}^{{tree}}")]);
        let head = self.git_out(&["rev-parse", "HEAD^{tree}"]);
        if want.is_none() || want != head {
            return Some(vec![format!(
                "HEAD's tree is {}, {}'s is {}",
                head.as_deref().map_or("unresolved", short),
                short(commit),
                want.as_deref().map_or("unresolved", short)
            )]);
        }
        match self.dirty() {
            Ok(dirty) if dirty.is_empty() => None,
            Ok(dirty) => Some(dirty.into_iter().take(5).collect()),
            Err(why) => Some(vec![format!(
                "the clean-tree judge could not answer: {why}"
            )]),
        }
    }

    /// This clone's main to `rev`: a fast-forward, or — only when every commit it would
    fn main_to(&mut self, rev: &str) -> Result<(), Refusal> {
        let head = self.head();
        let ancestor = |s: &mut Self, a: &str, b: &str| s.is_ancestor(a, b);
        let mut cmd = command("git");
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

    /// No engine at its path (no checkout, no `bin/pub`), with the fix.
    fn no_engine(&self) -> Option<Refusal> {
        let e = self.engine.display().to_string();
        (!self.engine.join(".git").exists() || !self.engine.join("bin/pub").is_file()).then(|| {
            Refusal::new(
                format!("no publication engine at {e} (bin/pub)"),
                "clone it there, or name one: --engine <path> (or PUBLICATION_ENGINE)",
            )
        })
    }

    /// Why the engine cannot stage at all — none at its path, not on `main`, changes
    fn engine_unready(&self) -> Option<Refusal> {
        if let Some(why) = self.no_engine() {
            return Some(why);
        }
        let e = self.engine.display().to_string();
        if self
            .engine_git(&["rev-parse", "--abbrev-ref", "HEAD"])
            .is_none_or(|b| b.trim() != "main")
        {
            return Some(Refusal::new(
                format!("the engine {e} is not on main — stage and publish run from its main"),
                format!("git -C {e} switch main"),
            ));
        }
        self.engine_dirty().then(|| {
            Refusal::new(
                format!("the engine {e} has local changes besides its audit log"),
                format!("commit or discard them there: git -C {e} status --short"),
            )
        })
    }

    /// The engine a read-only run drives: a `git clone --shared` of the engine in the run's
    fn scratch_engine(&self, scratch: &Scratch) -> Result<PathBuf, Refusal> {
        if let Some(why) = self.no_engine() {
            return Err(why);
        }
        let e = self.engine.display().to_string();
        let origin = self
            .engine_git(&["remote", "get-url", "origin"])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Refusal::new(
                    format!("the engine {e} names no origin"),
                    format!("git -C {e} remote add origin <the engine's repository>"),
                )
            })?;
        let copy = scratch.dir.join("engine");
        let _ = std::fs::remove_dir_all(&copy);
        let copy_s = copy.display().to_string();
        let ran = |cmd: &mut Command| self.output_forwarded(cmd).is_ok_and(|o| o.status.success());
        let in_copy = |args: &[&str]| {
            let mut cmd = command("git");
            cmd.arg("-C").arg(&copy).args(args);
            cmd
        };
        let mut clone = command("git");
        clone.arg("clone").args(BYTE_EXACT).args([
            "--quiet",
            "--shared",
            "--no-checkout",
            &e,
            &copy_s,
        ]);
        if !ran(&mut clone) {
            return Err(Refusal::new(
                format!("git could not copy the engine {e} into {copy_s}"),
                "fix what keeps it from being written, then run it again",
            ));
        }
        if !ran(&mut in_copy(&["remote", "set-url", "origin", &origin]))
            || !ran(&mut in_copy(&[
                "fetch",
                "--quiet",
                "--no-tags",
                "origin",
                "refs/heads/main",
            ]))
        {
            return Err(Refusal::new(
                format!("cannot fetch the main of the engine's origin ({origin})"),
                format!("check the network: git -C {e} ls-remote origin"),
            ));
        }
        if !ran(&mut in_copy(&[
            "checkout",
            "--quiet",
            "-B",
            "main",
            "FETCH_HEAD",
        ])) {
            return Err(Refusal::new(
                format!("git could not check the engine's origin main out in {copy_s}"),
                "fix what keeps it from being written, then run it again",
            ));
        }
        Ok(copy)
    }

    /// Why the engine's cargo-metadata guard cannot run here — it runs `rustup run
    fn metadata_toolchain(&self) -> Option<Refusal> {
        let Some(dir) = rustup_dir() else {
            return Some(Refusal::new(
                "no rustup (on PATH, in $CARGO_HOME/bin or in ~/.cargo/bin) — the engine's \
                 cargo-metadata guard runs `rustup run trust targo metadata`",
                "install rustup, then run it again",
            ));
        };
        let found = self
            .output_forwarded(
                command(dir.join("rustup"))
                    .args(["which", "--toolchain", "trust", "targo"])
                    .env("PATH", path_with(&dir)),
            )
            .is_ok_and(|o| o.status.success());
        (!found).then(|| {
            Refusal::new(
                "rustup has no `trust` toolchain with a targo — the engine's cargo-metadata \
                 guard runs `rustup run trust targo metadata`",
                "aterm pkg doctor, then aterm pkg repair (atpkg keeps ~/.rustup/toolchains/trust)",
            )
        })
    }

    /// The export guards, `ship check --export`: the engine's own dry run, `pub stage aterm --dry-run --no-check --commit R --repo-path <scratch clone>`, every export guard `pub stage` runs (forbidden content, private references, gitleaks, the cargo metadata closure, README versions, dead links) in one pass, over a scratch clone of R (never this checkout's `publish/.out`, where a real stage's attestation waits for `pub publish`) and a [`Real::scratch_engine`] copy.
    pub(crate) fn export_gate(&mut self, scratch: &Scratch, r: &str) -> (Outcome, Option<PathBuf>) {
        let not_run = |why: Refusal| (Outcome::CouldNotRun(why), None);
        if !self.is_ancestor(r, "refs/remotes/origin/main") {
            return not_run(Refusal::new(
                format!(
                    "{} is not on origin/main (as this clone last fetched it) — the engine \
                     stages only mainline commits, so unpushed work is not judged",
                    short(r)
                ),
                "push it (or `git fetch origin`, when it is there already), then run it again",
            ));
        }
        let engine = match self.scratch_engine(scratch) {
            Ok(engine) => engine,
            Err(why) => return not_run(why),
        };
        let root = scratch.dir.join("dry-run").join("aterm");
        let log = scratch.dir.join("dry-run.log");
        if let Err(why) = self.dry_clone(&scratch.dir, &root, r) {
            return not_run(Refusal::new(
                format!(
                    "the engine's dry run could not lay {} out, so no export guard has run: \
                     {why}",
                    short(r)
                ),
                format!(
                    "fix what that names, then run it again (git's own words: {})",
                    scratch.dir.join("dry-run-clone.log").display()
                ),
            ));
        }
        let toolchain = self.metadata_toolchain();
        let pubbin = engine.join("bin/pub");
        let mut stage = self.scrubbed(&pubbin);
        stage
            .env("PUBLICATION_ENGINE", &engine)
            .env(
                "PATH",
                rustup_dir().map_or_else(
                    || std::env::var_os("PATH").unwrap_or_default(),
                    |dir| path_with(&dir),
                ),
            )
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
        let (code, text) = match self.output_forwarded(&mut stage) {
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
        let mut run = EngineRun::read(code, &text);
        let out = root.join("publish/.out");
        let export = out.join("export");
        for (name, at) in &mut run.failed {
            if let Some(report) = gitleaks_report(name) {
                let json = std::fs::read_to_string(report).unwrap_or_default();
                *at = gitleaks_locations(&json, &export);
            }
        }
        let reports = scratch.dir.join("reports");
        keep_reports(&out, &reports);
        let judged = !run.passed.is_empty() || !run.failed.is_empty();
        let left = (judged && export.is_dir()).then_some(export);
        (
            export_outcome(&run, r, toolchain.as_ref(), &log, &reports),
            left,
        )
    }

    /// R laid out at `root` (under `base`'s `dry-run/`) the way `pub stage --repo-path`
    fn dry_clone(&self, base: &Path, root: &Path, r: &str) -> Result<(), String> {
        let log = base.join("dry-run-clone.log");
        let _ = std::fs::write(&log, "");
        let _ = std::fs::remove_dir_all(base.join("dry-run"));
        if base.join("dry-run").exists() {
            return Err(format!(
                "the last run's clone at {} cannot be removed",
                base.join("dry-run").display()
            ));
        }
        let origin = self
            .git_out(&["remote", "get-url", "origin"])
            .ok_or("this checkout names no origin")?;
        let git = |dir: &Path, args: &[&str]| -> bool {
            let out = self.output_forwarded(command("git").arg("-C").arg(dir).args(args));
            if let Ok(o) = &out
                && let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log)
            {
                let _ = f.write_all(&o.stderr);
            }
            out.is_ok_and(|o| o.status.success())
        };
        let clone = |from: &str, into: &str| {
            let args = [
                &["clone"][..],
                &BYTE_EXACT,
                &["--quiet", "--shared", "--no-checkout", from, into],
            ];
            git(base, &args.concat())
        };
        let (repo, root_s) = (self.repo.display().to_string(), root.display().to_string());
        if !clone(&repo, &root_s)
            || !git(root, &["remote", "set-url", "origin", &origin])
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
        for (rev, path) in
            crate::gates::gitlinks(&self.git_out(&["ls-tree", "-r", r]).unwrap_or_default())
        {
            let sub = self.repo.join(&path);
            let own = command("git")
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
            if !clone(&sub_s, &dest_s) || !git(&dest, &["checkout", "--quiet", "--detach", &rev]) {
                return Err(format!(
                    "git could not clone the submodule {path} at {} into the scratch clone",
                    short(&rev)
                ));
            }
        }
        Ok(())
    }

    /// THE WASM32 ENGINE: the site's /terminal engine type-checked on the public toolchain pin, with R's own `publish/config.sh` wasm32 clause (read out of its `CHECK_CMD_DEFAULT`, never restated), over the export the export guards judged — else a `git archive` of R without `.cargo/` — copied outside every checkout, in a scrubbed environment. v0.91.0 was refused there by the site build AFTER its promote; `pub stage`'s public-clone gate runs the same clause.
    pub(crate) fn wasm32_gate(
        &mut self,
        scratch: &Scratch,
        r: &str,
        export: Option<&Path>,
        tools: Option<&Wasm32Tools>,
    ) -> Outcome {
        let config = self
            .git_out(&["show", &format!("{r}:publish/config.sh")])
            .unwrap_or_default();
        let shape = match wasm_clause(&config) {
            Ok(shape) => shape,
            Err(why) => return Outcome::Refuse(why),
        };
        let not_run = |why: String, fix: String| Outcome::CouldNotRun(Refusal::new(why, fix));
        let Some(Wasm32Tools { rustup, path }) = tools else {
            return not_run(
                "no rustup (on PATH, in $CARGO_HOME/bin or in ~/.cargo/bin) — the wasm32 pin \
                 is a stock channel rustup installs"
                    .into(),
                "install rustup, then run it again".into(),
            );
        };
        let src = scratch.dir.join("wasm-src");
        let log = scratch.dir.join("wasm-check.log");
        let _ = std::fs::remove_dir_all(&src);
        let laid = std::fs::create_dir_all(&src).is_ok()
            && match export {
                Some(export) => self
                    .output_forwarded(command("/bin/cp").arg("-R").arg(export.join(".")).arg(&src))
                    .is_ok_and(|o| o.status.success()),
                None => self.archive_into(r, &src),
            };
        if !laid {
            return not_run(
                format!("could not lay {} out in {}", short(r), src.display()),
                "fix what keeps it from being written, then run it again".into(),
            );
        }
        let mut env: Vec<(String, std::ffi::OsString)> = vec![
            (
                "CARGO_TARGET_DIR".into(),
                scratch.cache.join("wasm-target").into_os_string(),
            ),
            (
                "TERM".into(),
                std::env::var_os("TERM").unwrap_or_else(|| "dumb".into()),
            ),
            ("PATH".into(), path.clone()),
        ];
        for keep in [
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
            if let Some(v) = std::env::var_os(keep) {
                env.push((keep.into(), v));
            }
        }
        let scrubbed = |program: &Path| {
            let mut cmd = command(program);
            cmd.env_clear().envs(env.iter().cloned()).current_dir(&src);
            cmd
        };
        let add = self.output_forwarded(scrubbed(&rustup.join("rustup")).args([
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
            return not_run(
                format!(
                    "rustup could not add wasm32-unknown-unknown to the wasm32 pin {}",
                    shape.pin
                ),
                format!(
                    "rustup toolchain install {} --target wasm32-unknown-unknown (full log: {})",
                    shape.pin,
                    log.display()
                ),
            );
        }
        let checked =
            self.output_forwarded(scrubbed(Path::new("/bin/sh")).args(["-c", &shape.clause]));
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
            return Outcome::Pass(format!(
                "wasm32 engine: aterm-wasm compiles for wasm32-unknown-unknown on {}",
                shape.pin
            ));
        }
        if checked.is_err()
            || text.contains("can't find crate for `core`")
            || text.contains("target may not be installed")
        {
            return not_run(
                format!(
                    "the wasm32 clause did not run on {} (its target is missing)",
                    shape.pin
                ),
                format!(
                    "rustup target add --toolchain {} wasm32-unknown-unknown (full log: {})",
                    shape.pin,
                    log.display()
                ),
            );
        }
        Outcome::Refuse(
            Refusal::new(
                format!(
                    "aterm-wasm does NOT compile for wasm32-unknown-unknown on the wasm32 pin {} \
                     — `pub stage` refuses this tree, and the site could not build its /terminal \
                     engine from it",
                    shape.pin
                ),
                format!(
                    "fix the source — a cfg(not(target_os = \"macos\")) arm no host build \
                     compiles is the usual culprit; full log: {}",
                    log.display()
                ),
            )
            .detail(
                text.lines()
                    .filter(|l| l.starts_with("error"))
                    .take(6)
                    .map(str::to_string),
            ),
        )
    }

    /// `git archive` of `r` into `dest`, without `.cargo/`, each submodule written from
    fn archive_into(&self, r: &str, dest: &Path) -> bool {
        let script = format!(
            "command set -o pipefail 2>/dev/null; git {} -C \"$1\" archive \"$2\" | tar -x -C \"$3\"",
            BYTE_EXACT.join(" ")
        );
        let tar = |repo: &Path, rev: &str, into: &Path| -> bool {
            let mut pipe = command("/bin/sh");
            pipe.args(["-c", &script, "archive"])
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
        crate::gates::gitlinks(&self.git_out(&["ls-tree", "-r", r]).unwrap_or_default())
            .into_iter()
            .all(|(rev, path)| {
                let into = dest.join(&path);
                std::fs::create_dir_all(&into).is_ok() && tar(&self.repo.join(&path), &rev, &into)
            })
    }

    /// `pub stage aterm --commit R --repo-path <this clone>`, the engine synced before
    fn stage_run(&self, c: &Candidate, prover: &Prover) -> Result<Ran, Refusal> {
        self.engine_sync()?;
        let mut cmd = self.child(self.engine.join("bin/pub"), Some(prover));
        cmd.args(["stage", "aterm", "--commit", &c.commit, "--repo-path"])
            .arg(&self.repo);
        let ran = self.logged(self.step, &mut cmd);
        self.engine_sync()?;
        Ok(ran)
    }

    /// A forwarded child with the release's build environment and its prover, its two
    fn captured(
        &self,
        prover: &Prover,
        log: &Path,
        cmd: &mut Command,
    ) -> std::io::Result<std::process::Output> {
        scrub(cmd);
        cmd.env("TRUST_STAGE2_BIN", &prover.bin);
        let out = self.output_forwarded(cmd)?;
        append_output(log, &out);
        Ok(out)
    }

    /// The release's own handoff fixtures for its reserved identity, written by the
    fn stage_fixtures_run(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        log: &Path,
    ) -> Result<String, Refusal> {
        let pins = fixture_pins(c, reserved);
        let root = fixture_root(&self.repo)?;
        let mut run = |cmd: &mut Command| self.captured(prover, log, cmd);
        crate::fixtures::stage(
            &prover.bin.join("targo"),
            &self.repo,
            &root,
            &pins,
            &mut run,
        )
        .map(|staged| staged.words(&pins))
        .map_err(|e| {
            let text = e.to_string();
            Refusal::new(
                text.lines().next().unwrap_or("").to_string(),
                format!(
                    "fix it on main (targo --unverified test -p aterm-gui --lib -- {} with \
                         the pins {} names), then {RELEASE_COMMAND} --commit origin/main",
                    crate::fixtures::generator_args().join(" "),
                    crate::gates::HANDOFF_FIXTURE_README
                ),
            )
            .detail(text.lines().skip(1).take(8).map(str::to_string))
        })
    }

    /// `ship build` as the build step's child (R's own cutter, this binary: it reads the
    fn build_run(
        &self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<Artifact, Refusal> {
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

    /// The published release `tag` as GitHub serves it, through the authenticated `gh`
    fn release_json(&self, slug: &str, tag: &str) -> Result<aterm_json::Value, String> {
        let out = self
            .output_forwarded(
                command("gh").args(["api", &format!("repos/{slug}/releases/tags/{tag}")]),
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

    /// The evergreen pointer as github.com serves a stranger: the first hop of
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
        let served = self.output_forwarded(command("curl").args([
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

    /// The release alab.systems would link: the site's own election of the macOS
    fn site_tag(&self, slug: &str) -> Result<String, String> {
        let list = self
            .output_forwarded(
                command("gh").args(["api", &format!("repos/{slug}/releases?per_page=30")]),
            )
            .map_err(|e| format!("cannot run gh: {e}"))?;
        if !list.status.success() {
            return Err(format!(
                "the release list for {slug} could not be read through gh ({})",
                String::from_utf8_lossy(&list.stderr).trim()
            ));
        }
        let catalog = crate::site::releases::parse_releases(&String::from_utf8_lossy(&list.stdout))
            .map_err(|e| format!("the release list for {slug} is not release JSON ({e})"))?;
        crate::site::releases::elect_mac(&catalog, slug, &mut Vec::new())
            .map(|(mac, _)| mac.tag)
            .map_err(|e| format!("the site could not tell which release to link on {slug} ({e})"))
    }

    /// `ship site --latest`, in this process: its transcript into the step's log (and the
    fn site(&mut self, slug: &str) -> Result<AfterCut, Refusal> {
        if interrupt::caught().is_some() {
            return Err(Refusal::new(
                "not started: the release was interrupted",
                format!("{RELEASE_COMMAND} again"),
            ));
        }
        let log = self.log_path(Step::AfterCut);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| {
                Refusal::new(
                    format!("cannot open {}: {e}", log.display()),
                    format!("{RELEASE_COMMAND} again once it is writable"),
                )
                .could_not_run()
            })?;
        let mut out = StepLog {
            file,
            echo: self.verbose,
        };
        let _ = writeln!(out, "$ ship site --latest ({slug}, in process)");
        let env = crate::site::Env::from_process(&self.repo, Some(slug)).map_err(|e| {
            Refusal::new(
                format!("the site settings cannot be read: {e}"),
                format!("fix publish/site.toml, then {RELEASE_COMMAND}"),
            )
        })?;
        let mut world = crate::site::Real {
            unset: std::env::vars_os()
                .filter_map(|(k, _)| k.into_string().ok())
                .filter(|k| clears(k) || launcher_env(k))
                .collect(),
            launch: std::rc::Rc::new(StepChildren {
                noted: (!self.scratch_logs).then(|| (self.release.clone(), self.step)),
            }),
        };
        let opts = crate::site::Options {
            latest: true,
            ..crate::site::Options::default()
        };
        let ended = crate::site::sync(&opts, &env, &mut world, &mut out);
        match &ended {
            Ok(crate::site::Outcome::NoCheckout(line)) => {
                let _ = writeln!(out, "ship site: {line}");
            }
            Err(failure) => {
                let _ = writeln!(out, "ship site: FAIL: {failure}");
            }
            Ok(_) => {}
        }
        site_outcome(&ended, tail(&log, 8))
    }
}

/// A step's log, and the terminal too when `echo`.
struct StepLog {
    file: std::fs::File,
    echo: bool,
}

impl std::io::Write for StepLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.echo {
            let _ = std::io::stdout().write_all(buf);
        }
        self.file.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.echo {
            let _ = std::io::stdout().flush();
        }
        self.file.flush()
    }
}

/// The after-cut step's refusal of a published release whose shape is wrong.
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

/// What `ship site --latest` ending with `ended` means for the after-cut step; `detail` is the end of its transcript.
/// # Errors
/// A deployed site the live site does not show yet, and every failure of the sync.
pub fn site_outcome(
    ended: &Result<crate::site::Outcome, crate::site::Failure>,
    detail: Vec<String>,
) -> Result<AfterCut, Refusal> {
    use crate::site::Outcome;
    let note = |s: &str| {
        Ok(AfterCut {
            note: s.into(),
            site_deferred: false,
        })
    };
    match ended {
        Ok(Outcome::Synced { deployed: true }) => {
            note("alab.systems brought to the release and deployed (ship site --latest)")
        }
        Ok(Outcome::Synced { deployed: false }) => {
            note("alab.systems already shows the release (ship site --latest)")
        }
        Ok(Outcome::LiveUnchecked { deployed }) => note(if *deployed {
            "alab.systems brought to the release and deployed, but the release could NOT be \
             verified live: the live site could not be fetched (check it by hand)"
        } else {
            "alab.systems already committed for the release, but the release could NOT be \
             verified live: the live site could not be fetched (check it by hand)"
        }),
        Ok(Outcome::NoDeploy) => note(
            "alab.systems committed and pushed for the release but NOT deployed: SITE_NO_DEPLOY \
             asked for no deploy (deploy with the site's deploy.sh)",
        ),
        Ok(Outcome::NotDeployed) => note(
            "alab.systems committed and pushed for the release but NOT deployed: Firebase's \
             login here is missing or expired (deploy with the site's deploy.sh)",
        ),
        Ok(Outcome::Skipped | Outcome::Deferred | Outcome::Planned { .. }) => {
            note("alab.systems left as it is (ship site --latest did not sync)")
        }
        Ok(Outcome::NoCheckout(why)) => Ok(AfterCut {
            note: format!("ship site said: {why}"),
            site_deferred: true,
        }),
        Ok(Outcome::LiveDisagrees { .. }) => Err(Refusal::new(
            "the site is deployed, but the live site does not agree yet (ship site exited 4)",
            format!("{RELEASE_COMMAND} again in a few minutes"),
        )
        .detail(detail)),
        Err(why) => Err(Refusal::new(
            format!("the release is well shaped, but the site sync failed: {why}"),
            format!(
                "read its log (a failed push leaves its site commit on this machine), then \
                 {RELEASE_COMMAND}"
            ),
        )
        .detail(detail)),
    }
}

/// Whether the journal's bundle is built for this release: its store entry COMPLETE in the
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

/// The variables cleared from every child, so every step builds what the operator's shell would: each `RUST*` and `CARGO*` (target-directory overrides included) but the inert knobs (job count, incremental, logging, backtraces, cargo's terminal output), and the C toolchain's (the `cc` crate's names, also as `<NAME>_<target>`, `TARGET_<NAME>`, `HOST_<NAME>`).
pub(crate) fn clears(name: &str) -> bool {
    const INERT: [&str; 8] = [
        "CARGO_BUILD_JOBS",
        "CARGO_MAKEFLAGS",
        "CARGO_INCREMENTAL",
        "CARGO_LOG",
        "RUST_BACKTRACE",
        "RUST_LIB_BACKTRACE",
        "RUST_LOG",
        "RUST_TEST_THREADS",
    ];
    const NATIVE: [&str; 8] = [
        "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "ARFLAGS",
    ];
    if name.starts_with("RUST") || name.starts_with("CARGO") {
        return !INERT.contains(&name) && !name.starts_with("CARGO_TERM_");
    }
    if matches!(
        name,
        "MACOSX_DEPLOYMENT_TARGET" | "SDKROOT" | "CRATE_CC_NO_DEFAULTS"
    ) {
        return true;
    }
    let bare = name
        .strip_prefix("TARGET_")
        .or_else(|| name.strip_prefix("HOST_"))
        .unwrap_or(name);
    NATIVE.iter().any(|n| {
        bare == *n
            || bare
                .strip_prefix(n)
                .is_some_and(|rest| rest.starts_with('_'))
    })
}

/// What `targo run` handed this process as its launcher (`CARGO`, `CARGO_PRIMARY_PACKAGE`, `CARGO_MANIFEST_*`, `CARGO_PKG_*`, their `TARGO_*` twins, `TRUST_TARGO_FRONTEND`, `DYLD_FALLBACK_LIBRARY_PATH`): cleared too, silently, so every step runs as from the operator's shell, never as a child of the alias.
pub(crate) fn launcher_env(name: &str) -> bool {
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

/// The identity a release's handoff fixtures are written as: its version and the
fn fixture_pins(c: &Candidate, reserved: &Reservation) -> crate::fixtures::Pins {
    crate::fixtures::Pins {
        version: c.version.clone(),
        build: reserved.build,
        commit: reserved.commit.clone(),
    }
}

/// Where the clone's release cache stages them.
fn fixture_root(repo: &Path) -> Result<PathBuf, Refusal> {
    crate::snapshot::CacheRoot::for_repo(repo)
        .map(|cache| cache.handoff_fixtures())
        .map_err(|e| {
            Refusal::new(
                format!("the release cache: {e}"),
                format!("run {RELEASE_COMMAND} again once it is readable"),
            )
            .could_not_run()
        })
}

/// A child run beside another, joined: what it answered — or, its thread having
fn joined(
    handle: std::thread::ScopedJoinHandle<'_, Result<String, Refusal>>,
    what: &str,
) -> Result<String, Refusal> {
    handle.join().unwrap_or_else(|_| {
        Err(Refusal::new(
            format!("{what} stopped unexpectedly (its thread panicked)"),
            format!("read its log, then run {RELEASE_COMMAND} again to resume"),
        )
        .could_not_run())
    })
}

/// One of the cut's own gates, refusing in its own words, as a refusal: its first line
fn cut_refusal(gate: &str, why: &str) -> Refusal {
    let first = why.lines().next().unwrap_or("");
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
}

/// A captured child's output appended to `log`, both streams (a write that fails loses
fn append_output(log: &Path, out: &std::process::Output) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = f.write_all(&out.stdout);
        let _ = f.write_all(&out.stderr);
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

/// Where one `ship check` lays its sources out: its own `runs/<pid>-<n>` inside the [`preflight_cache`], so no two runs share one and none waits for another.
pub(crate) struct Scratch {
    pub(crate) dir: PathBuf,
    pub(crate) cache: PathBuf,
}

impl Scratch {
    /// A run's own directory, the runs whose process is gone removed first.
    pub(crate) fn read_only(repo: &Path) -> Result<Self, String> {
        static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let cache = preflight_cache(repo)?;
        let runs = cache.join("runs");
        let me = std::process::id();
        for entry in std::fs::read_dir(&runs).into_iter().flatten().flatten() {
            let pid = entry
                .file_name()
                .to_str()
                .and_then(|n| n.split('-').next())
                .and_then(|p| p.parse::<u32>().ok());
            if pid.is_some_and(|pid| pid != me && started_of(pid).is_none()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = runs.join(format!("{me}-{nth}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        Ok(Self { dir, cache })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for sources in ["dry-run", "engine", "wasm-src"] {
            let _ = std::fs::remove_dir_all(self.dir.join(sources));
        }
    }
}

/// Where the wasm32 gate finds rustup, and the PATH its clause runs under.
pub(crate) struct Wasm32Tools {
    /// The directory holding `rustup`.
    pub(crate) rustup: PathBuf,
    /// The clause's PATH: this process's with `rustup`'s directory appended.
    pub(crate) path: std::ffi::OsString,
}

impl Wasm32Tools {
    /// This machine's: rustup on PATH, in `$CARGO_HOME/bin` or in `~/.cargo/bin`.
    pub(crate) fn find() -> Option<Self> {
        rustup_dir().map(|rustup| Self {
            path: path_with(&rustup),
            rustup,
        })
    }
}

/// Where rustup is for the engine's children: the PATH entry that has it, else
fn rustup_dir() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(std::env::var_os("CARGO_HOME").map(|h| PathBuf::from(h).join("bin")))
        .chain(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo/bin")))
        .find(|d| d.join("rustup").is_file())
}

/// This process's PATH with `dir` appended when it is not on it — appended, so
fn path_with(dir: &Path) -> std::ffi::OsString {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    if !dirs.iter().any(|d| d == dir) {
        dirs.push(dir.to_path_buf());
    }
    std::env::join_paths(dirs).unwrap_or(path)
}

/// The engine's report files (its `.out`'s top level: the hits, the gitleaks reports,
fn keep_reports(out: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    for entry in std::fs::read_dir(out).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::copy(entry.path(), to.join(entry.file_name()));
        }
    }
}

/// The report a failing gitleaks guard names (`gitleaks findings (see <report>)`).
fn gitleaks_report(guard: &str) -> Option<&str> {
    guard
        .strip_prefix("gitleaks findings (see ")?
        .strip_suffix(')')
}

/// Up to six gitleaks findings as `<path in the export>:<line> <rule>` — never the matched text or the secret.
#[must_use]
pub fn gitleaks_locations(report: &str, export: &Path) -> Vec<String> {
    let Ok(aterm_json::Value::Array(findings)) = aterm_json::from_str(report) else {
        return Vec::new();
    };
    findings
        .iter()
        .take(6)
        .map(|f| {
            let file = f
                .get("File")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("?");
            let file = Path::new(file)
                .strip_prefix(export)
                .map_or_else(|_| file.to_string(), |p| p.display().to_string());
            let line = f
                .get("StartLine")
                .and_then(aterm_json::Value::as_u64)
                .unwrap_or(0);
            let rule = f
                .get("RuleID")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or("?");
            format!("{file}:{line} {rule}")
        })
        .collect()
}

/// What the engine's dry run printed, read in the engine's own words: `== guard ok: …` per passing guard, `== GUARD FAIL: …` per failing one (its indented lines its locations), `FAIL: …` per refusal.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EngineRun {
    pub code: i32,
    pub passed: Vec<String>,
    /// Each failing guard and up to six of its location lines.
    pub failed: Vec<(String, Vec<String>)>,
    pub refusals: Vec<String>,
    /// The run's last three lines, for a run that said nothing else.
    pub tail: Vec<String>,
}

impl EngineRun {
    /// The run that exited `code` and printed `text`.
    #[must_use]
    pub fn read(code: i32, text: &str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let mut run = Self {
            code,
            tail: lines[lines.len().saturating_sub(3)..]
                .iter()
                .map(|l| (*l).to_string())
                .collect(),
            ..Self::default()
        };
        for (i, line) in lines.iter().enumerate() {
            if let Some(what) = line.strip_prefix("== guard ok: ") {
                run.passed.push(what.to_string());
            } else if let Some(what) = line.strip_prefix("== GUARD FAIL: ") {
                let at = lines[i + 1..]
                    .iter()
                    .take_while(|d| d.starts_with("  "))
                    .take(6)
                    .map(|d| d.trim().to_string())
                    .collect();
                run.failed.push((what.to_string(), at));
            } else if let Some(why) = line.strip_prefix("FAIL: ") {
                run.refusals.push(why.to_string());
            }
        }
        run
    }
}

/// Why the engine refused, read from its `FAIL:` line: the commit's tree, or the machine's — something that decided nothing about the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineCause {
    /// The tree's own (its manifest, its config, its transforms, its version): a refusal of the commit.
    Tree,
    /// gitleaks is not on PATH: the engine stops at that fail-closed guard.
    NoGitleaks,
    /// gitleaks crashed or timed out.
    GitleaksBroke,
    /// A remote (the origins, the staging repository, the public tags) could not be read, or moved while it was.
    Network,
    /// The engine's own checkout, records or lock.
    Engine,
    /// The commit is not on its origin's main, or the source clone is not one the engine stages.
    Source,
}

/// The engine's words for each refusal of the machine's (a substring of its `FAIL:`
const MACHINE_REFUSALS: [(&str, EngineCause); 17] = [
    ("gitleaks not installed", EngineCause::NoGitleaks),
    ("gitleaks crashed", EngineCause::GitleaksBroke),
    ("release notes: cannot ", EngineCause::Network),
    ("cannot query ", EngineCause::Network),
    ("cannot fetch ", EngineCause::Network),
    (" fetch failed", EngineCause::Network),
    ("fetch for pin failed", EngineCause::Network),
    ("while fetching", EngineCause::Network),
    ("publication engine ", EngineCause::Engine),
    ("another publication operation", EngineCause::Engine),
    ("mappings.json", EngineCause::Engine),
    ("mapping history", EngineCause::Engine),
    ("append-only violation", EngineCause::Engine),
    ("--commit ", EngineCause::Source),
    ("--repo-path", EngineCause::Source),
    ("source override", EngineCause::Source),
    ("source origin", EngineCause::Source),
];

impl EngineCause {
    /// The cause of the engine's refusal `line` (its `FAIL:` line, without the prefix).
    #[must_use]
    pub fn of(line: &str) -> Self {
        MACHINE_REFUSALS
            .iter()
            .find(|(words, _)| line.contains(words))
            .map_or(Self::Tree, |(_, cause)| *cause)
    }

    fn remedy(self) -> String {
        match self {
            Self::Tree => "fix the source the refusal names (never the export)".into(),
            Self::NoGitleaks => aterm_uds::host::tool("gitleaks").remedy(),
            Self::GitleaksBroke => {
                "run `gitleaks version` to see why it cannot scan, then run it again".into()
            }
            Self::Network => "check the network and access to the remote it names (ssh for a \
                              git@ remote), then run it again"
                .into(),
            Self::Engine => {
                "fix the publication engine's checkout or records it names, then run it again"
                    .into()
            }
            Self::Source => "push the commit to origin/main (the engine stages only mainline \
                             commits), or fix the source clone it names, then run it again"
                .into(),
        }
    }
}

/// The export guards' [`Outcome`] from the engine's dry run of `r` (`toolchain`: why the cargo-metadata guard cannot run here, if it cannot; `log`: the whole run; `reports`: the engine's report files).
#[must_use]
pub fn export_outcome(
    run: &EngineRun,
    r: &str,
    toolchain: Option<&Refusal>,
    log: &Path,
    reports: &Path,
) -> Outcome {
    let whole = format!("the whole run: {}", log.display());
    let heard = (!run.passed.is_empty())
        .then(|| format!("the guards that passed first: {}", run.passed.join("; ")));
    let machine_guard =
        |name: &str| toolchain.is_some() && name.starts_with("targo metadata failed");
    let findings: Vec<&(String, Vec<String>)> = run
        .failed
        .iter()
        .filter(|(name, _)| !machine_guard(name))
        .collect();
    let refusals: Vec<(&str, EngineCause)> = run
        .refusals
        .iter()
        .map(String::as_str)
        .filter(|l| !l.starts_with("guards failed"))
        .map(|l| (l, EngineCause::of(l)))
        .collect();
    let tree: Vec<&str> = refusals
        .iter()
        .filter(|(_, cause)| *cause == EngineCause::Tree)
        .map(|(line, _)| *line)
        .collect();
    let first_tree = tree
        .iter()
        .find(|l| **l != "transforms failed")
        .or(tree.first())
        .copied();
    let machine = refusals
        .iter()
        .find(|(_, cause)| *cause != EngineCause::Tree)
        .copied();
    if !findings.is_empty() || first_tree.is_some() {
        let why = if findings.is_empty() {
            format!(
                "the engine refuses to stage {}: {}",
                short(r),
                first_tree.unwrap_or_default()
            )
        } else {
            let names: Vec<&str> = findings.iter().map(|(name, _)| name.as_str()).collect();
            format!(
                "the engine's export guards refuse {}: {}",
                short(r),
                names.join("; ")
            )
        };
        let guards = findings.iter().flat_map(|(name, at)| {
            std::iter::once(format!("GUARD FAIL: {name}"))
                .chain(at.iter().map(|l| format!("  {l}")))
        });
        let others = tree
            .iter()
            .filter(|l| !findings.is_empty() || Some(**l) != first_tree)
            .take(3)
            .map(|l| format!("FAIL: {l}"));
        let stopped = machine.map(|(line, cause)| {
            format!(
                "then the engine stopped, judging nothing more: {line} (fix: {})",
                cause.remedy()
            )
        });
        return Outcome::Refuse(
            Refusal::new(
                why,
                "fix the SOURCE, never the export — patterns: the engine's \
                 baseline/forbidden-content.txt + publish/forbidden-extra.txt",
            )
            .detail(
                guards
                    .chain(others)
                    .chain(stopped)
                    .chain(std::iter::once(format!(
                        "the engine's reports: {}",
                        reports.display()
                    )))
                    .chain(std::iter::once(whole)),
            ),
        );
    }
    if let Some((line, cause)) = machine {
        return Outcome::CouldNotRun(
            Refusal::new(
                format!("the engine could not judge {}: {line}", short(r)),
                cause.remedy(),
            )
            .detail(heard.into_iter().chain(std::iter::once(whole))),
        );
    }
    if let Some(toolchain) = toolchain
        && run.failed.iter().any(|(name, _)| machine_guard(name))
    {
        return Outcome::CouldNotRun(
            toolchain
                .clone()
                .detail(heard.into_iter().chain(std::iter::once(whole))),
        );
    }
    if run.code == 0 && !run.passed.is_empty() {
        return Outcome::Pass(format!(
            "export guards: the engine's dry run of {} passed {} guard(s): {}",
            short(r),
            run.passed.len(),
            run.passed.join("; ")
        ));
    }
    Outcome::CouldNotRun(
        Refusal::new(
            format!(
                "the engine's dry run of {} ended (exit {}) without a guard verdict",
                short(r),
                run.code
            ),
            "read the whole run, and teach this gate the engine's words if they changed",
        )
        .detail(run.tail.iter().cloned().chain(std::iter::once(whole))),
    )
}

/// The wasm32 clause of `publish/config.sh`'s `CHECK_CMD_DEFAULT` and the stock channel it names: one reader, aterm-verify's, whose `verify provision` acquires the pin.
pub use aterm_verify::rustup::WasmClause;

/// Read the wasm32 clause out of `config` (the engine's own line shape, `CHECK_CMD_DEFAULT="…"`), with every refusal the stage would meet for it (`aterm_verify::rustup::wasm_clause`, as a release refusal).
/// # Errors
/// No clause; no stock channel; no target-add before it; no `RUSTC` naming the pin's own rustc.
pub fn wasm_clause(config: &str) -> Result<WasmClause, Refusal> {
    aterm_verify::rustup::wasm_clause(config).map_err(|e| Refusal::new(e.why, e.remedy))
}

/// The problems with the source manifest pair a completed `pub publish` leaves on the
fn source_attestation_findings(release: &aterm_json::Value) -> Vec<String> {
    crate::channel::SOURCE_ATTESTATION_ASSETS
        .into_iter()
        .filter_map(|name| {
            let count = release
                .get("assets")
                .and_then(aterm_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter(|a| a.get("name").and_then(aterm_json::Value::as_str) == Some(name))
                .count();
            match count {
                1 => None,
                0 => Some(format!(
                    "{name} is missing — the signed source manifest `pub publish` attaches \
                     (`pub sign-release aterm` attaches it to a published release)"
                )),
                _ => Some(format!(
                    "{name} is duplicated — the source manifest must be unambiguous"
                )),
            }
        })
        .collect()
}

/// The lean-download shape of a published release, from what GitHub serves (its release JSON, never the files the cut wrote): the problems, empty when it carries the shape.
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
    bad.extend(source_attestation_findings(release));
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

/// What the after-cut step makes of the pointer's answer: its shape problems, or, when
fn pointer_verdict(read: Result<Vec<String>, String>) -> Result<Vec<String>, Refusal> {
    read.map_err(|why| {
        Refusal::new(
            format!("{why}, so the release pointer's shape is unknown"),
            format!("{RELEASE_COMMAND} again when GitHub answers"),
        )
        .could_not_run()
    })
}

/// The pointer's first hop judged: `Ok(None)` for a 302 to exactly `want`;
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

/// The appcast served at the pointer's target: it names `version`, and its `url` is exactly the tag-specific download URL of its own `dmg` (the credential-less updater refuses any other).
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
        self.step_log(step)
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
                    " — the prover this release started with ({j}) is gone; resolving a replacement"
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
            .filter(|k| clears(k) && !launcher_env(k))
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
        out.extend(self.engine_unready());
        match self.dirty() {
            Ok(dirty) if dirty.is_empty() => {}
            Ok(dirty) => out.push(
                Refusal::new(
                    format!(
                        "this clone ({}) is not clean — a release needs its own clean checkout",
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

    /// Whether the engine that would publish is the one that staged: its HEAD at its
    fn engine_now(&mut self, staged: &str) -> Result<EngineNow, String> {
        let e = self.engine.display().to_string();
        let head = self.engine_commit("HEAD")?;
        let origin = self.engine_commit("origin/main")?;
        let (Some(head), Some(origin)) = (head, origin) else {
            return Ok(EngineNow::Off(format!(
                "the engine {e} has no HEAD or no origin/main commit — pub stage and pub \
                 publish refuse such an engine"
            )));
        };
        if head != origin {
            return Ok(EngineNow::Off(format!(
                "the engine {e} is at {}, not its origin/main {} — pub stage and pub publish \
                 refuse such an engine",
                short(&head),
                short(&origin)
            )));
        }
        let Some(staged) = self.engine_commit(staged)? else {
            return Ok(EngineNow::Moved(format!(
                "the engine no longer holds the commit that staged ({}); its origin/main is {}",
                short(staged),
                short(&head)
            )));
        };
        if staged == head {
            return Ok(EngineNow::Same(format!(
                "the engine is the one that staged ({}, its origin/main)",
                short(&head)
            )));
        }
        let (from, to) = (staged.as_str(), head.as_str());
        let (code, out) = self.engine_git_code(&["merge-base", "--is-ancestor", from, to])?;
        match code {
            0 => {}
            1 => {
                return Ok(EngineNow::Moved(format!(
                    "the engine's origin/main {} does not contain the commit that staged ({}): \
                     its history was rewritten",
                    short(to),
                    short(from)
                )));
            }
            _ => {
                return Err(format!(
                    "git -C {e} merge-base --is-ancestor {} {} exited {code}: {}",
                    short(from),
                    short(to),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        let (code, out) = self.engine_git_code(&[
            "diff",
            "--quiet",
            from,
            to,
            "--",
            ".",
            ":(exclude)verification-log.jsonl",
            ":(exclude)mappings.json",
            ":(exclude)handoff/",
        ])?;
        match code {
            0 => Ok(EngineNow::Same(format!(
                "the engine at its origin/main {} has the code that staged ({}; only its data \
                 plane moved)",
                short(to),
                short(from)
            ))),
            1 => Ok(EngineNow::Moved(format!(
                "the engine's code moved since it staged: {}..{} (its origin/main)",
                short(from),
                short(to)
            ))),
            _ => Err(format!(
                "git -C {e} diff {}..{} exited {code}: {}",
                short(from),
                short(to),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    /// `git ls-remote --refs <staging.remote> refs/heads/main` against the attestation's
    fn staging_moved(&mut self) -> Result<Option<String>, String> {
        let path = self.repo.join("publish/.out/attestation.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let Some((remote, attested)) = attested_staging(&text) else {
            return Ok(Some(format!(
                "{} names no staging tier (staging.remote, staging.attested_rev)",
                path.display()
            )));
        };
        let out = self
            .remote_git(&["ls-remote", "--refs", &remote, "refs/heads/main"])
            .map_err(|e| format!("git ls-remote {remote}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git ls-remote {remote} refs/heads/main exited {}: {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listing = String::from_utf8_lossy(&out.stdout);
        Ok(staging_moved_from(&listing, &attested))
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

    /// The bump, committed on origin/main's tip and pushed before anything is staged (`pub
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
            let mut commit = command("git");
            commit
                .args(["commit", "-q", "-m", &msg, "--", "Cargo.toml", "Cargo.lock"])
                .current_dir(&self.repo);
            if self.logged(Step::Commit, &mut commit).code != 0 {
                return Err(Refusal::new(
                    "could not commit the bump",
                    "read the commit step's log",
                ));
            }
            let mut push = command("git");
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

    fn own_cutter(&mut self, commit: &str) -> Result<String, crate::cutter::NotOwn> {
        crate::gates::own_cutter(&self.git(), commit)
    }

    fn reexec(&mut self, prover: &Prover) -> Refusal {
        let mut cmd = self.scrubbed(prover.bin.join("targo"));
        cmd.args(["--unverified", "ship"])
            .args(&self.argv)
            .current_dir(&self.repo);
        #[cfg(unix)]
        {
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
        let built = built_answer(&self.repo, artifact, prover);
        let verified = match artifact.filter(|_| built.yes) {
            None => Answer::no("its bundle is not built yet (step 5)"),
            Some(a) => match crate::gates::verified(&git, &c.commit, &a.macho_sha256) {
                Ok((yes, words)) => answer(yes, words),
                Err(e) => Answer::unanswered(format!("the verified judge did not answer: {e}")),
            },
        };
        let attestation =
            std::fs::read_to_string(self.repo.join("publish/.out/attestation.json")).ok();
        let attested = attestation
            .as_deref()
            .is_some_and(|t| attestation_promotes(t, &c.commit));
        let staged_by = attestation
            .as_deref()
            .filter(|_| attested)
            .and_then(attested_engine);
        let fixtures = match crate::fixtures::on_commit(&git, "origin/main", &c.version) {
            Ok(Some(found)) => Answer::yes(format!(
                "v{}'s handoff fixtures are on main ({} desks)",
                c.version,
                found.desks.len()
            )),
            Ok(None) => Answer::no(format!("v{}'s handoff fixtures are not on main", c.version)),
            Err(e) => Answer::no(e.to_string()),
        };
        Facts {
            cut: self.cut_journal(),
            engine: self.engine_rows(Some(&c.commit)),
            attested,
            staged_by,
            built,
            verified,
            fixtures,
        }
    }

    fn disk(&mut self) -> (Option<u64>, bool) {
        let free = command("df")
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
        let warm = self.repo.join("target/debug").is_dir();
        (free, warm)
    }

    fn commit_gates(&mut self, c: &Candidate, dry: bool) -> Vec<Refusal> {
        let mut blocks: Vec<Refusal> = Vec::new();
        let git = self.git();
        if dry && let Err(not) = crate::gates::own_cutter(&git, &c.commit) {
            self.say(&format!(
                "        note: R's pre-claim gates asked of this cutter, not yet R's own (a real \
                 run re-execs as R's): {}",
                not.why
            ));
        }
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        let gates: [(&str, crate::ledger::Result<()>); 4] = [
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
            (
                "locked metadata",
                crate::gates::locked_metadata_gate(&self.repo),
            ),
        ];
        for (name, outcome) in gates {
            if let Err(e) = outcome {
                blocks.push(cut_refusal(name, &e.to_string()));
            }
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let gitleaks = std::env::split_paths(&path).any(|d| d.join("gitleaks").is_file());
        if !gitleaks {
            blocks.push(Refusal::new(
                "gitleaks is not installed — every stage dies at the guard",
                aterm_uds::host::tool("gitleaks").remedy(),
            ));
        }
        let seam = std::fs::read_to_string(self.engine.join("bin/pub"))
            .is_ok_and(|t| t.contains("run_pre_promote_hook"));
        if !seam {
            blocks.push(Refusal::new(
                format!(
                    "the engine {} has no pre-promote seam (`run_pre_promote_hook` in its \
                     bin/pub): `pub publish` would not run publish/pre-promote, and a publish \
                     by hand would promote R's attestation before it is verified",
                    self.engine.display()
                ),
                "use a publication engine whose main carries the seam (on its main since \
                 2026-09-29: git -C <engine> pull --ff-only), or name one with --engine",
            ));
        }
        blocks
    }

    fn cut_gates(&mut self, c: &Candidate, creds: &Path) -> Vec<Refusal> {
        let gates = crate::publish::pre_publish_gates(&self.repo, &c.version, creds);
        let mut found: Vec<Refusal> = gates
            .into_iter()
            .map(|(gate, why)| cut_refusal(gate, &why))
            .collect();
        let git = self.git();
        let (commit, version) = (c.commit.as_str(), c.version.as_str());
        if let Err(e) = crate::gates::handoff_fixture_gate_at(&git, commit, version) {
            found.push(cut_refusal("handoff fixtures", &e.to_string()));
        }
        if let Err(e) = crate::gates::handoff_policy_gate_at(&git, commit, version) {
            found.push(cut_refusal("handoff policy", &e.to_string()));
        }
        found
    }

    fn stage_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal> {
        let log = self.step_log(self.step);
        self.stage_fixtures_run(c, reserved, prover, &log)
    }

    fn land_fixtures(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
    ) -> Result<String, Refusal> {
        let pins = fixture_pins(c, reserved);
        let again = || {
            format!(
                "run {RELEASE_COMMAND} again, which lands them on the tip it finds (or add them \
                 by hand: {}, \"By hand\")",
                crate::gates::HANDOFF_FIXTURE_README
            )
        };
        let git = self.git();
        let fetched = crate::ledger::git_ok(&git, &["fetch", "origin", "main"]);
        if fetched.is_ok()
            && let Ok(Some(found)) = crate::fixtures::on_commit(&git, "origin/main", &c.version)
        {
            return Ok(crate::fixtures::Landed::Already {
                tip: self
                    .git_out(&["rev-parse", "origin/main"])
                    .unwrap_or_default(),
                desks: found.desks,
            }
            .words(&pins));
        }
        let root = fixture_root(&self.repo)?;
        let mut words = Vec::new();
        let staged = match crate::fixtures::staged(&root, &pins) {
            Some(staged) => staged,
            None => {
                words.push(self.stage_fixtures(c, reserved, prover)?);
                crate::fixtures::staged(&root, &pins).ok_or_else(|| {
                    Refusal::new(
                        format!(
                            "v{}'s handoff fixtures were written and are gone",
                            c.version
                        ),
                        again(),
                    )
                })?
            }
        };
        let landed = crate::fixtures::land(&git, &staged, &pins, crate::ledger::MAX_CLAIM_ATTEMPTS)
            .map_err(|e| Refusal::new(e.to_string(), again()))?;
        words.push(landed.words(&pins));
        Ok(words.join("; "))
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

    /// Beside each other: the fixture generator on a thread of its own (its own target
    fn build(
        &mut self,
        c: &Candidate,
        reserved: &Reservation,
        prover: &Prover,
        fixtures: bool,
    ) -> Built {
        let fixtures_log = self.side_log(Step::Build, "fixtures");
        let this = &*self;
        std::thread::scope(|s| {
            let staged = if fixtures {
                let log = &fixtures_log;
                Some(s.spawn(move || this.stage_fixtures_run(c, reserved, prover, log)))
            } else {
                None
            };
            let artifact = this.build_run(c, reserved, prover);
            Built {
                artifact,
                fixtures: staged.map(|h| joined(h, "the handoff fixture generator")),
            }
        })
    }

    fn verify(
        &mut self,
        c: &Candidate,
        prover: &Prover,
        artifact: &Artifact,
    ) -> Result<Ran, Refusal> {
        let r = short(&c.commit).to_string();
        let fix = format!(
            "put the clone back at {r} and clean (git -C {} status), then {RELEASE_COMMAND}",
            self.repo.display()
        );
        if let Some(off) = self.off_tree(&c.commit) {
            return Err(Refusal::new(
                format!("the clone is not at {r}'s tree and clean, so `verify release` would not judge it"),
                fix,
            )
            .detail(off)
            .could_not_run());
        }
        let mut cmd = self.child(prover.bin.join("targo"), Some(prover));
        cmd.args(crate::release::VERIFY_ARGS)
            .arg("--app")
            .arg(&artifact.app);
        let mut ran = self.logged_to(Step::Verify, &mut cmd, true);
        if ran.code != 0 {
            return Ok(ran);
        }
        if let Some(off) = self.off_tree(&c.commit) {
            return Err(Refusal::new(
                format!(
                    "the clone moved during `verify release`, so its pass is not {r}'s — \
                     nothing was recorded"
                ),
                fix,
            )
            .detail(off)
            .could_not_run());
        }
        if let Err(e) =
            crate::gates::record_verified(&self.git(), &c.commit, &artifact.macho_sha256)
        {
            ran.detail.push(format!("its record was not written: {e}"));
        }
        Ok(ran)
    }

    fn stage(&mut self, c: &Candidate, prover: &Prover) -> Result<Ran, Refusal> {
        self.stage_run(c, prover)
    }

    fn publish(&mut self, prover: &Prover) -> Result<Ran, Refusal> {
        self.engine_sync()?;
        let mut cmd = self.publish_command(prover);
        Ok(self.logged(Step::Publish, &mut cmd))
    }

    fn source_publication(&mut self, c: &Candidate) -> Result<(), Refusal> {
        let slug = self.slug();
        let tag = format!("v{}", c.version);
        let release = self.release_json(&slug, &tag).map_err(|why| {
            Refusal::new(
                why,
                format!("run {RELEASE_COMMAND} again once the channel answers"),
            )
            .could_not_run()
        })?;
        let missing = source_attestation_findings(&release);
        if missing.is_empty() {
            return Ok(());
        }
        Err(Refusal::new(
            format!(
                "the source publication of {tag} is incomplete: {}",
                missing.join("; ")
            ),
            format!("finish pub sign-release aterm, then {RELEASE_COMMAND} (nothing was claimed)"),
        ))
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
            .output_forwarded(command("gh").args([
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
fn notice(verb: &[&str], line: &str) -> Result<String, String> {
    let out = {
        command("aterm")
            .arg("ctl")
            .args(verb)
            .args(line.split(' '))
            .stdin(Stdio::null())
            .output()
    };
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

/// The engine's ledger for one commit: its newest aterm row, whether `commit` has one, and that row's `release_rev`.
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

/// A promotable attestation of `commit`: its source is `commit`, `public_clone` passed (or was skipped on a source-only repo), and it names the engine commit that staged it ([`attested_engine`]), which promote also requires; one that names none is staged again rather than carried to a publish that refuses it.
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
        && attested_engine(json).is_some()
}

/// The engine commit an attestation names (`engine.commit`): the engine that staged it.
#[must_use]
pub fn attested_engine(json: &str) -> Option<String> {
    let a = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    a.get("engine")?
        .get("commit")?
        .as_str()
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// The staging tier an attestation names, and the commit of its main it attested (`staging.remote`, `staging.attested_rev`): what `pub promote` asks again before it promotes.
#[must_use]
pub fn attested_staging(json: &str) -> Option<(String, String)> {
    let a = aterm_json::from_str::<aterm_json::Value>(json).ok()?;
    let staging = a.get("staging")?;
    let field = |key: &str| {
        staging
            .get(key)
            .and_then(aterm_json::Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    Some((field("remote")?, field("attested_rev")?))
}

/// Why the staging tier's main in `listing` (`git ls-remote --refs <staging> refs/heads/main`) is not the `attested` commit — `pub promote` refuses the attestation then — or `None` when it is.
#[must_use]
pub fn staging_moved_from(listing: &str, attested: &str) -> Option<String> {
    let mains: Vec<&str> = listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (sha, name) = (fields.next()?, fields.next()?);
            (name == "refs/heads/main" && fields.next().is_none()).then_some(sha)
        })
        .collect();
    match mains.as_slice() {
        [now] if now.eq_ignore_ascii_case(attested) => None,
        [now] => Some(format!(
            "the staging tier's main moved since R was attested ({}..{}: a peer's pub stage), \
             and pub promote refuses that attestation",
            short(attested),
            short(now)
        )),
        [] => Some("the staging tier has no main".to_string()),
        many => Some(format!(
            "the staging tier answered {} copies of its main",
            many.len()
        )),
    }
}

/// Run a release in the clone at `repo`; the exit code.
#[must_use]
pub fn run(repo: &Path, opts: &crate::release::ReleaseOptions) -> i32 {
    interrupt::install();
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

/// What `ship check` asks: of which commit, with which engine, and which of its three sections — `--cut`, `--export`, `--wasm32`; none named is all three.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckOptions {
    pub commit: Option<String>,
    pub engine: Option<PathBuf>,
    pub cut: bool,
    pub export: bool,
    pub wasm32: bool,
}

impl CheckOptions {
    /// The sections asked: `(cut, export, wasm32)`.
    #[must_use]
    pub fn sections(&self) -> (bool, bool, bool) {
        if self.cut || self.export || self.wasm32 {
            (self.cut, self.export, self.wasm32)
        } else {
            (true, true, true)
        }
    }
}

/// `targo --unverified ship check`: would a release of the commit pass — the cut's pre-claim gates that read only the commit and this clone's records ([`crate::gates::precheck`]), the publication engine's export guards ([`Real::export_gate`]) and the wasm32 engine ([`Real::wasm32_gate`]), each line printed as its gate ends, then the verdict.
#[must_use]
pub fn check(repo: &Path, opts: &CheckOptions) -> i32 {
    interrupt::install();
    let found = match checked(repo, opts) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("aterm-release check: {e}");
            return 2;
        }
    };
    println!("{}", found.verdict());
    if interrupt::caught().is_some() {
        return 130;
    }
    found.exit_code()
}

/// [`check`]'s sections for the commit `opts` names, each gate's lines printed as it
/// # Errors
/// The commit names no commit, or its `Cargo.toml` carries no release version.
fn checked(repo: &Path, opts: &CheckOptions) -> Result<Precheck, String> {
    let rev = opts.commit.as_deref().unwrap_or("HEAD");
    let git = GitCli::new(repo);
    let (cut, export, wasm32) = opts.sections();
    let found = if cut {
        crate::gates::precheck(&git, rev)
    } else {
        Precheck::of(&git, rev)
    };
    let mut found = found.map_err(|e| e.to_string())?;
    print!("{}", found.header());
    for gate in &found.gates {
        print!("{}", Precheck::gate_lines(gate));
    }
    let add = |found: &mut Precheck, name: &'static str, outcome: Outcome| {
        let gate = PrecheckGate { name, outcome };
        print!("{}", Precheck::gate_lines(&gate));
        let _ = std::io::stdout().flush();
        found.gates.push(gate);
    };
    if !(export || wasm32) {
        return Ok(found);
    }
    let commit = found.commit.clone();
    let ready = Real::new(repo, opts.engine.as_deref(), true, false)
        .and_then(|real| Ok((real, Scratch::read_only(repo)?)))
        .map_err(|why| {
            Refusal::new(
                format!("the engine's gates have no place to run: {why}"),
                "fix what that names, then run it again",
            )
        });
    match ready {
        Ok((mut real, scratch)) => {
            let mut left = None;
            if export {
                let (outcome, export) = real.export_gate(&scratch, &commit);
                left = export;
                add(&mut found, EXPORT_GATE, outcome);
            }
            if wasm32 {
                let tools = Wasm32Tools::find();
                let outcome = real.wasm32_gate(&scratch, &commit, left.as_deref(), tools.as_ref());
                add(&mut found, WASM32_GATE, outcome);
            }
        }
        Err(why) => {
            for (asked, name) in [(export, EXPORT_GATE), (wasm32, WASM32_GATE)] {
                if asked {
                    add(&mut found, name, Outcome::CouldNotRun(why.clone()));
                }
            }
        }
    }
    Ok(found)
}

/// `ship check`'s name for the engine's dry run.
pub const EXPORT_GATE: &str = "export guards";
/// `ship check`'s name for the wasm32 compile.
pub const WASM32_GATE: &str = "wasm32 engine";

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor without FD_CLOEXEC, held open as one is between a `pipe` and its
    #[cfg(unix)]
    #[test]
    fn forwarded_steps_and_object_git_exclude_an_unflagged_descriptor() {
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
        unsafe extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        }
        let file = std::fs::File::open("/dev/null").unwrap();
        // SAFETY: F_DUPFD creates a fresh descriptor without FD_CLOEXEC.
        let fd = unsafe { fcntl(file.as_raw_fd(), 0, 200) };
        assert!(fd >= 200);
        let _held = unsafe { OwnedFd::from_raw_fd(fd) };
        let probe = aterm_spec::host::held_probe(fd);
        let control = aterm_uds::spawnfd::transparent("/bin/sh")
            .args(["-c", &probe])
            .status()
            .unwrap();
        assert_eq!(control.code(), Some(3));

        let repo = std::env::current_dir().unwrap();
        let real = Real::new(&repo, None, true, false).unwrap();
        let out = real
            .output_forwarded(command("/bin/sh").args(["-c", &probe]))
            .unwrap();
        assert!(out.status.success(), "detached step inherited fd {fd}");

        let alias = format!("alias.fd-proof=!{probe}");
        let out = crate::ledger::object_git(&repo)
            .args(["-c", &alias, "fd-proof"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "object git inherited fd {fd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The site sync's children keep the release's rules: each leads a session and group of
    #[cfg(unix)]
    #[test]
    fn the_site_syncs_children_keep_the_releases_rules() {
        use crate::site::Launch as _;
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-step-children-")
            .tempdir()
            .unwrap();
        let children = StepChildren {
            noted: Some((dir.path().to_path_buf(), Step::AfterCut)),
        };
        assert_eq!(children.interrupted(), None);
        let mut sh = aterm_uds::spawnfd::command("/bin/sh");
        sh.args([
            "-c",
            "echo \"$$ $(ps -o pgid= -p $$ | tr -d ' ') ${GIT_TERMINAL_PROMPT-unset} [${GIT_ASKPASS-unset}]\"",
        ]);
        let out = crate::site::output(&children, &mut sh).unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = said.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "{said}");
        assert_eq!(
            fields[0], fields[1],
            "the child leads its own group: {said}"
        );
        assert_eq!(fields[2], "0", "git's prompt is off: {said}");
        assert_eq!(fields[3], "[]", "and so is its askpass: {said}");
        assert!(
            crate::release::child_notes(dir.path()).is_empty(),
            "the note goes with the child"
        );
        let mut sleeper = aterm_uds::spawnfd::command("/bin/sh");
        sleeper
            .args(["-c", "read line"])
            .stdin(aterm_uds::spawnfd::Stdio::piped());
        let (mut child, guard) = children.spawn(&mut sleeper).unwrap();
        let note =
            std::fs::read_to_string(crate::release::child_note(dir.path(), child.id())).unwrap();
        assert!(
            note.starts_with(&format!("pid {}\n", child.id()))
                && note.ends_with("step after-cut\n"),
            "{note}"
        );
        drop(child.stdin.take());
        child.wait().unwrap();
        drop(guard);
        assert!(crate::release::child_notes(dir.path()).is_empty());
    }

    /// Only a 404 or a 302 elsewhere is the pointer's shape; a read that says nothing about
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
            Ok(Some(_))
        ));
        assert!(matches!(
            pointer_hop_problem(ptr, want, "v0.6.0", 302, Some("https://x/v0.5.0")),
            Ok(Some(_))
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
            ("SHA256SUMS", 680_000),
            ("SHA256SUMS.sig", 64),
        ]
    }

    #[test]
    fn source_publication_requires_one_manifest_and_one_signature_before_binary_assets() {
        let pair = [("SHA256SUMS", 100), ("SHA256SUMS.sig", 64)];
        assert!(source_attestation_findings(&release(&pair, "")).is_empty());
        for assets in [
            vec![],
            vec![pair[0]],
            vec![pair[1]],
            vec![pair[0], pair[0], pair[1]],
        ] {
            assert!(!source_attestation_findings(&release(&assets, "")).is_empty());
        }
    }

    /// The lean shape passes; each way a release is out of shape is named.
    #[test]
    fn a_release_is_judged_by_the_lean_shape_github_serves() {
        assert_eq!(
            release_shape(&release(&lean(), "notes"), "0.6.0"),
            Vec::<String>::new()
        );
        let mut seeded = lean();
        seeded[0].1 = 1_070_000_000;
        seeded.push(("aterm-0.6.0-x86_64.dmg", 9));
        seeded.retain(|(n, _)| *n != "aterm-mac.zip.sha256" && *n != "SHA256SUMS.sig");
        seeded[2].1 = 1;
        let bad = release_shape(&release(&seeded, "the batteries-included image"), "0.6.0");
        assert_eq!(bad.len(), 6, "{bad:#?}");
        for asset in [
            "aterm-mac.zip.sha256",
            "aterm-0.6.0-x86_64.dmg",
            "SHA256SUMS.sig",
        ] {
            assert!(bad.iter().any(|p| p.contains(asset)), "{asset}: {bad:#?}");
        }
        let mut linux = lean();
        linux.push(("aterm-0.6.0-linux-x86_64", 40_000_000));
        linux.push(("aterm-0.6.0-linux-riscv64", 40_000_000));
        linux.push(("aterm-0.5.0-linux-aarch64", 40_000_000));
        linux.push(("aterm-0.6.0-linux-aarch64.tar.gz", 1_000));
        linux.retain(|(n, _)| *n != "aterm-machines.toml.sig");
        let bad = release_shape(&release(&linux, ""), "0.6.0");
        for asset in [
            "aterm-0.6.0-linux-riscv64",
            "aterm-0.5.0-linux-aarch64",
            "aterm-0.6.0-linux-aarch64.tar.gz.sha256",
            "aterm-machines.toml.sig",
        ] {
            assert!(bad.iter().any(|p| p.contains(asset)), "{asset}: {bad:#?}");
        }
    }

    #[test]
    fn the_appcast_the_pointer_serves_names_this_version_and_binds_its_url() {
        let slug = "alabsystems/aterm";
        let good = "version = \"0.6.0\"\ndmg = \"aterm-0.6.0.dmg\"\n\
                    url = \"https://github.com/alabsystems/aterm/releases/download/v0.6.0/aterm-0.6.0.dmg\"\n";
        assert!(appcast_problems(good, slug, "v0.6.0", "0.6.0").is_empty());
        assert!(!appcast_problems(good, slug, "v0.7.0", "0.7.0").is_empty());
        let unbound = good.replace("download/v0.6.0", "download/v0.5.0");
        assert!(!appcast_problems(&unbound, slug, "v0.6.0", "0.6.0").is_empty());
    }

    /// The engine's dry run, read in its own words: each guard's verdict with its
    #[test]
    fn the_engines_dry_run_is_read_in_its_own_words() {
        let text = "== exported 5807 of 7058 tracked files from abc\n\
                    == guard ok: no forbidden content\n\
                    == GUARD FAIL: private references (2)\n  a.rs:3\n  b.md:9\n\
                    FAIL: guards failed — nothing was pushed\n";
        let run = EngineRun::read(1, text);
        assert_eq!(run.passed, ["no forbidden content"]);
        assert_eq!(
            run.failed,
            [(
                "private references (2)".to_string(),
                vec!["a.rs:3".to_string(), "b.md:9".to_string()]
            )]
        );
        assert_eq!(run.refusals, ["guards failed — nothing was pushed"]);
        assert_eq!(EngineRun::read(0, "").tail, Vec::<String>::new());
    }

    /// Every engine refusal is the tree's unless its words name the machine's,
    #[test]
    fn an_engine_refusal_is_sorted_by_its_cause() {
        for line in [
            "malformed manifest line (bare '!'): '!'",
            "empty manifest",
            "captured aterm commit has no publish/manifest.txt",
            "publication policy contains a symlink, which could escape immutable inputs: x",
            "CHECK_TIMEOUT_SECONDS_DEFAULT must be a positive integer",
            "vendor/astream is not a gitlink in abc",
            "transforms failed",
            "the export does not pin the Trust toolchain the dev commit pins",
            "detected version '1.2' is not a semver string",
            "no staging remote configured",
            "staging remote must identify alabsystems/aterm-staging, found 'x'",
        ] {
            assert_eq!(EngineCause::of(line), EngineCause::Tree, "{line}");
        }
        for (line, cause) in [
            (
                "gitleaks not installed — a fail-closed guard cannot be skipped",
                EngineCause::NoGitleaks,
            ),
            (
                "gitleaks crashed/timed out — aborting rather than fail open",
                EngineCause::GitleaksBroke,
            ),
            (
                "release notes: cannot read the public tags of alabsystems/aterm: git \
                 ls-remote failed",
                EngineCause::Network,
            ),
            (
                "release notes: cannot fetch the previous public release v0.1.0",
                EngineCause::Network,
            ),
            (
                "cannot query aterm origin/main: fatal: unable to access",
                EngineCause::Network,
            ),
            (
                "astream public release main fetch failed: Host key verification failed.",
                EngineCause::Network,
            ),
            (
                "publication engine HEAD a does not exactly match queried origin/main b",
                EngineCause::Engine,
            ),
            (
                "another publication operation for aterm is running; cannot stage",
                EngineCause::Engine,
            ),
            (
                "--commit abc is not an ancestor of origin/main def",
                EngineCause::Source,
            ),
        ] {
            assert_eq!(EngineCause::of(line), cause, "{line}");
        }
    }

    /// The export guards' outcome, by cause: a finding or a refusal of the tree refuses
    #[test]
    fn the_export_guards_pass_refuse_or_could_not_run() {
        let (log, reports) = (Path::new("/c/dry-run.log"), Path::new("/c/reports"));
        let outcome = |code: i32, text: &str, toolchain: Option<&Refusal>| {
            export_outcome(&EngineRun::read(code, text), "r", toolchain, log, reports)
        };
        let refused = |code: i32, text: &str, toolchain: Option<&Refusal>| match outcome(
            code, text, toolchain,
        ) {
            Outcome::Refuse(r) => r,
            other => panic!("{text:?}: {other:?}"),
        };
        let not_run = |code: i32, text: &str, toolchain: Option<&Refusal>| match outcome(
            code, text, toolchain,
        ) {
            Outcome::CouldNotRun(r) => r,
            other => panic!("{text:?}: {other:?}"),
        };
        let no_toolchain = Refusal::new("no rustup", "install rustup");
        let exported = "== exported 1 of 1 tracked files from r\n";
        let ok =
            format!("{exported}== guard ok: no forbidden content\n== guard ok: gitleaks clean\n");
        assert!(matches!(outcome(0, &ok, None), Outcome::Pass(_)));
        assert!(matches!(
            outcome(0, &ok, Some(&no_toolchain)),
            Outcome::Pass(_)
        ));
        let hit =
            format!("{exported}== GUARD FAIL: forbidden content (1 hits; first 5):\n  x.rs:1\n");
        for after in [
            "FAIL: guards failed — nothing was pushed\n",
            "FAIL: gitleaks not installed — a fail-closed guard cannot be skipped\n",
            "FAIL: gitleaks crashed/timed out — aborting rather than fail open\n",
        ] {
            let r = refused(1, &format!("{hit}{after}"), None);
            assert!(r.detail.iter().any(|l| l == "  x.rs:1"), "{r:?}");
        }
        for (text, line) in [
            (
                "FAIL: malformed manifest line (bare '!'): '!'\n".to_string(),
                "malformed manifest line (bare '!'): '!'",
            ),
            (
                format!(
                    "{exported}FAIL: vendor/astream is not a gitlink in abc\nFAIL: transforms \
                     failed\n"
                ),
                "vendor/astream is not a gitlink in abc",
            ),
        ] {
            assert!(refused(1, &text, None).why.contains(line), "{text}");
        }
        for (text, line) in [
            (
                "FAIL: cannot query aterm origin/main: fatal: unable to access\n".to_string(),
                "cannot query aterm origin/main",
            ),
            (
                format!(
                    "{exported}== guard ok: no forbidden content\nFAIL: gitleaks not installed \
                     — a fail-closed guard cannot be skipped\n"
                ),
                "gitleaks not installed",
            ),
            (
                format!(
                    "{exported}== guard ok: no forbidden content\nFAIL: gitleaks crashed/timed \
                     out — aborting rather than fail open\n"
                ),
                "gitleaks crashed",
            ),
            (
                format!(
                    "{exported}FAIL: release notes: cannot read the public tags of \
                     alabsystems/aterm: git ls-remote failed\n"
                ),
                "release notes: cannot read the public tags",
            ),
        ] {
            assert!(not_run(1, &text, None).why.contains(line), "{text}");
        }
        let metadata = format!(
            "{exported}== guard ok: gitleaks clean\n== GUARD FAIL: targo metadata failed (see \
             cargo-metadata.err)\nFAIL: guards failed — nothing was pushed\n"
        );
        refused(1, &metadata, None);
        assert_eq!(
            not_run(1, &metadata, Some(&no_toolchain)).why,
            no_toolchain.why
        );
        for (code, text) in [
            (0, exported),
            (1, exported),
            (-1, "cannot run /e/bin/pub: No such file or directory"),
        ] {
            not_run(code, text, None);
        }
    }

    /// A gitleaks finding is named by its place in the export and its rule, never by
    #[test]
    fn a_gitleaks_finding_is_named_by_place_and_rule_never_by_its_secret() {
        let report = r#"[{"File":"/x/export/a/b.rs","StartLine":7,"RuleID":"generic-api-key","Match":"key=hunter2","Secret":"hunter2"}]"#;
        assert_eq!(
            gitleaks_locations(report, Path::new("/x/export")),
            ["a/b.rs:7 generic-api-key"]
        );
        assert!(gitleaks_locations("not json", Path::new("/x")).is_empty());
        assert_eq!(
            gitleaks_report("gitleaks findings (see /o/gitleaks-adjudicated.json)"),
            Some("/o/gitleaks-adjudicated.json")
        );
        assert_eq!(gitleaks_report("private references (2)"), None);
    }

    /// `ship check --export` end to end, offline: a stub engine whose `bin/pub` prints
    #[test]
    fn ship_check_export_drives_a_scratch_copy_of_the_engine_at_its_origins_main() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |cwd: &Path, args: &[&str]| -> String {
            let out = crate::ledger::testing::git()
                .current_dir(cwd)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let (engine, engine_origin) = (root.join("publication"), root.join("publication.git"));
        let (repo, origin) = (root.join("aterm"), root.join("aterm.git"));
        for bare in [&engine_origin, &origin] {
            git(
                &root,
                &[
                    "init",
                    "-q",
                    "--bare",
                    "-b",
                    "main",
                    &bare.display().to_string(),
                ],
            );
        }
        for (dir, remote) in [(&engine, &engine_origin), (&repo, &origin)] {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
            git(
                dir,
                &["remote", "add", "origin", &remote.display().to_string()],
            );
        }
        let stub = |say: &str, code: i32| {
            std::fs::create_dir_all(engine.join("bin")).unwrap();
            let pubbin = engine.join("bin/pub");
            std::fs::write(
                &pubbin,
                format!(
                    "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")/..\" && pwd)\necho '{{\"row\":1}}' \
                     >> \"$here/verification-log.jsonl\"\n[ \"$1 $2 $3 $4\" = 'stage aterm \
                     --dry-run --no-check' ] || exit 9\nprintf '%s' '{say}'\nexit {code}\n"
                ),
            )
            .unwrap();
            let mut perms = std::fs::metadata(&pubbin).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
            std::fs::set_permissions(&pubbin, perms).unwrap();
            std::fs::write(engine.join("verification-log.jsonl"), "").unwrap();
            git(&engine, &["add", "-A"]);
            git(&engine, &["commit", "-q", "-m", "stub"]);
            git(&engine, &["push", "-q", "--force", "origin", "main"]);
        };
        std::fs::write(
            repo.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        git(&repo, &["fetch", "-q", "origin"]);
        let opts = |commit: &str, engine: &Path| CheckOptions {
            commit: Some(commit.to_string()),
            engine: Some(engine.to_path_buf()),
            export: true,
            ..CheckOptions::default()
        };
        let export = |commit: &str| {
            let found = checked(&repo, &opts(commit, &engine)).expect("a release");
            let gate = found.gates.last().expect("the export gate");
            assert_eq!(gate.name, EXPORT_GATE);
            (gate.outcome.clone(), found.exit_code())
        };
        let exported = "== exported 1 of 1 tracked files from r\n";
        stub(&format!("{exported}== guard ok: no forbidden content\n"), 0);
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::Pass(_)), "{outcome:?}");
        assert_eq!(code, 0);
        assert_eq!(
            std::fs::read_to_string(engine.join("verification-log.jsonl")).unwrap(),
            ""
        );
        assert_eq!(git(&engine, &["status", "--porcelain"]), "");
        stub("FAIL: malformed manifest line (bare !): !\n", 1);
        git(&engine, &["reset", "-q", "--hard", "HEAD~1"]);
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::Refuse(_)), "{outcome:?}");
        assert_eq!(code, 1);
        stub(
            "FAIL: cannot query aterm origin/main: fatal: unable to access\n",
            1,
        );
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
        assert_eq!(code, 3);
        stub(&format!("{exported}== guard ok: no forbidden content\n"), 0);
        std::fs::write(repo.join("new.rs"), "\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "unpushed"]);
        let (outcome, code) = export("HEAD");
        assert!(matches!(outcome, Outcome::CouldNotRun(_)), "{outcome:?}");
        assert_eq!(code, 3);
        let found = checked(&repo, &opts("HEAD~1", &root.join("none"))).unwrap();
        assert!(matches!(
            found.gates.last().map(|g| &g.outcome),
            Some(Outcome::CouldNotRun(_))
        ));
        assert!(checked(&repo, &opts("no-such-commit", &engine)).is_err());
    }

    /// THE WASM32 GATE, offline: a stub rustup on the clause's PATH answers `target add`,
    #[test]
    fn the_wasm32_gate_runs_rs_own_clause_under_a_stub_rustup() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let git = |cwd: &Path, args: &[&str]| -> String {
            let out = crate::ledger::testing::git()
                .current_dir(cwd)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let repo = root.join("aterm");
        std::fs::create_dir_all(repo.join("publish")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join("publish/config.sh"),
            "CHECK_CMD_DEFAULT=\"rustup target add --toolchain 9.9.9 wasm32-unknown-unknown && \
             RUSTC=$(rustup which --toolchain 9.9.9 rustc) rustup run 9.9.9 cargo check \
             --locked --target wasm32-unknown-unknown -p aterm-wasm\"\n",
        )
        .unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "r"]);
        let r = git(&repo, &["rev-parse", "HEAD"]);
        let stub = root.join("rustup-bin");
        std::fs::create_dir_all(&stub).unwrap();
        std::fs::write(
            stub.join("rustup"),
            "#!/bin/sh\nhere=$(cd \"$(dirname \"$0\")\" && pwd)\ncase \"$1\" in\n\
             target) exit 0 ;;\n\
             which) echo \"$here/rustc\"; exit 0 ;;\n\
             run) [ -f Cargo.toml ] || { echo \"error: no tree laid out in $(pwd)\" >&2; exit 101; }\n\
             \x20 [ -f \"$here/broken\" ] && { echo 'error[E0425]: cannot find value `x`' >&2; exit 101; }\n\
             \x20 echo 'Finished'; exit 0 ;;\nesac\n\
             echo \"stub rustup: unexpected $*\" >&2; exit 2\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(stub.join("rustup"))
            .unwrap()
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(stub.join("rustup"), perms).unwrap();
        let tools = Wasm32Tools {
            rustup: stub.clone(),
            path: std::env::join_paths([stub.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                .unwrap(),
        };
        let scratch = Scratch {
            dir: root.join("scratch"),
            cache: root.join("cache"),
        };
        std::fs::create_dir_all(&scratch.dir).unwrap();
        let export = root.join("export");
        std::fs::create_dir_all(&export).unwrap();
        std::fs::write(export.join("Cargo.toml"), "[workspace]\n").unwrap();
        let mut real = Real::new(&repo, None, true, false).expect("a backend");

        let none = real.wasm32_gate(&scratch, &r, Some(&export), None);
        assert!(
            matches!(none, Outcome::CouldNotRun(_)),
            "no rustup: {none:?}"
        );
        let passed = real.wasm32_gate(&scratch, &r, Some(&export), Some(&tools));
        assert!(matches!(passed, Outcome::Pass(_)), "{passed:?}");
        std::fs::write(stub.join("broken"), "").unwrap();
        let broken = real.wasm32_gate(&scratch, &r, Some(&export), Some(&tools));
        match &broken {
            Outcome::Refuse(why) => assert!(
                why.detail.iter().any(|l| l.starts_with("error[E0425]")),
                "the compiler's errors ride along: {why:?}"
            ),
            other => panic!("a compile that fails refuses: {other:?}"),
        }
        std::fs::remove_file(stub.join("broken")).unwrap();
        let archived = real.wasm32_gate(&scratch, &r, None, Some(&tools));
        assert!(matches!(archived, Outcome::Pass(_)), "{archived:?}");
        assert!(scratch.dir.join("wasm-src/publish/config.sh").is_file());
    }

    /// The env that turns [`probe_every_tree_the_release_writes_under_a_crlf_config`] on:
    const CRLF_PROBE_ENV: &str = "ATERM_RELEASE_CRLF_PROBE_DIR";
    /// That probe's name, as `--exact` wants it.
    const CRLF_PROBE: &str =
        "release_steps::tests::probe_every_tree_the_release_writes_under_a_crlf_config";

    /// The child half of [`every_tree_the_release_writes_is_the_commits_bytes`]: the engine
    #[test]
    fn probe_every_tree_the_release_writes_under_a_crlf_config() {
        use crate::ledger::testing;
        use std::fs;
        let Some(root) = std::env::var_os(CRLF_PROBE_ENV).map(PathBuf::from) else {
            return;
        };
        let ident = [
            ("user.name", "t"),
            ("user.email", "t@example.invalid"),
            ("commit.gpgsign", "false"),
        ];
        let git = |at: &Path, args: &[&str]| -> String {
            let out = testing::git()
                .arg("-C")
                .arg(at)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let write = |path: &Path, body: &str| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        };
        let sub = root.join("sub");
        testing::init(&sub, &ident);
        write(&sub.join("s.sh"), "#!/bin/sh\n");
        git(&sub, &["add", "-A"]);
        git(&sub, &["commit", "-q", "-m", "s"]);
        let (repo, origin) = (root.join("aterm"), root.join("aterm.git"));
        testing::init(&repo, &ident);
        write(&repo.join(".gitattributes"), "*.txt text\n");
        write(&repo.join("bin/pub"), "#!/bin/sh\n");
        write(&repo.join("notes.txt"), "one\n");
        write(&repo.join("RELEASES.ledger"), "1783354739 0.25.0\n");
        write(
            &repo.join("CHANGELOG.md"),
            "## [Unreleased]\n\n### Added\n- An entry.\n",
        );
        let sub_s = sub.display().to_string();
        let add_sub = ["submodule", "add", "-q", &sub_s, "vendor/sub"];
        git(
            &repo,
            &[&["-c", "protocol.file.allow=always"][..], &add_sub].concat(),
        );
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "r"]);
        git(&root, &["init", "-q", "--bare", "-b", "main", "aterm.git"]);
        git(
            &repo,
            &["remote", "add", "origin", &origin.display().to_string()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        let r = git(&repo, &["rev-parse", "HEAD"]);
        let lf = |tree: &Path, notes: &str, with_sub: bool| {
            let read = |rel: &str| fs::read_to_string(tree.join(rel)).unwrap();
            assert_eq!(read("bin/pub"), "#!/bin/sh\n", "{}", tree.display());
            assert_eq!(read("notes.txt"), notes, "{}", tree.display());
            if with_sub {
                assert_eq!(read("vendor/sub/s.sh"), "#!/bin/sh\n", "{}", tree.display());
            }
        };
        let real = Real::new(&repo, Some(&repo), true, false).unwrap();
        let scratch = Scratch {
            dir: root.join("scratch"),
            cache: root.join("cache"),
        };
        fs::create_dir_all(&scratch.dir).unwrap();
        lf(&real.scratch_engine(&scratch).unwrap(), "one\n", false);
        let clone = scratch.dir.join("dry-run/aterm");
        real.dry_clone(&scratch.dir, &clone, &r).unwrap();
        lf(&clone, "one\n", true);
        let archive = root.join("archive");
        fs::create_dir_all(&archive).unwrap();
        assert!(real.archive_into(&r, &archive));
        lf(&archive, "one\n", true);
        let place = |commit: &str| {
            let source = crate::gates::PublishedSource {
                commit: commit.to_string(),
                verified_at: "2026-10-21T00:00:00+00:00".to_string(),
            };
            crate::gates::place_published(&GitCli::new(&repo), &repo, &source).unwrap()
        };
        lf(&place(&r).tree, "one\n", true);
        write(&repo.join("notes.txt"), "one\ntwo\n");
        git(&repo, &["commit", "-q", "-am", "two"]);
        git(&repo, &["push", "-q", "origin", "main"]);
        let source = git(&repo, &["rev-parse", "HEAD"]);
        let moved = place(&source);
        assert!(moved.moved);
        lf(&moved.tree, "one\ntwo\n", true);
        let plan = crate::ledger::ClaimPlan {
            version: "0.26.0",
            now: 0,
            allow_existing_section: false,
            max_attempts: 1,
            source: &source,
        };
        let rolls = |source: &str, main: &str| {
            crate::changelog::claim_changelogs(source, main, "0.26.0", "2026-10-21")
        };
        let cut = GitCli::new(&moved.tree);
        let reserved = crate::ledger::reserve(&cut, &plan, &rolls).unwrap();
        crate::ledger::claim(&cut, &moved.tree, &plan, &reserved, &rolls).unwrap();
        let rolled = fs::read_to_string(moved.tree.join("CHANGELOG.md")).unwrap();
        assert!(
            rolled.contains("## [0.26.0]") && !rolled.contains('\r'),
            "{rolled:?}"
        );
    }

    /// Every tree a release writes from git holds the commit's own bytes under a
    #[test]
    fn every_tree_the_release_writes_is_the_commits_bytes() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let config = root.join("gitconfig");
        std::fs::write(
            &config,
            "[core]\n\tautocrlf = true\n\tsafecrlf = true\n\teol = crlf\n[protocol \"file\"]\n\tallow = always\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("xdg/git")).unwrap();
        std::fs::write(root.join("xdg/git/attributes"), "* text eol=crlf\n").unwrap();
        let work = root.join("work");
        std::fs::create_dir(&work).unwrap();
        for leaf in ["home", "tmp", "state"] {
            std::fs::create_dir(root.join(leaf)).unwrap();
        }
        let mut child = command(std::env::current_exe().unwrap());
        aterm_uds::fixture_git::hermetic(&mut child)
            .args(["--exact", "--nocapture", "--test-threads=1", CRLF_PROBE])
            .env(CRLF_PROBE_ENV, &work)
            .env("HOME", root.join("home"))
            .env("TMPDIR", root.join("tmp"))
            .env("TMP", root.join("tmp"))
            .env("TEMP", root.join("tmp"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .stdin(Stdio::null());
        let out = aterm_uds::deadline::outputs_until(
            &mut [&mut child],
            std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            aterm_spec::wait::MUST_HAPPEN,
        )
        .pop()
        .expect("one probe");
        let out = match out {
            Ok(out) => out,
            Err(error) => {
                let kept = dir.keep();
                panic!("CRLF probe: {error}; retained fixture {}", kept.display());
            }
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the probe under a CRLF config: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `ship check`'s exit: a refusal outranks a gate that could not run, which
    #[test]
    fn a_refusal_outranks_a_gate_that_could_not_run() {
        let gate = |outcome: Outcome| PrecheckGate { name: "g", outcome };
        let pass = || gate(Outcome::Pass("ok".into()));
        let refuse = || gate(Outcome::Refuse(Refusal::new("no", "fix")));
        let not_run = || gate(Outcome::CouldNotRun(Refusal::new("no tool", "install")));
        for (gates, code) in [
            (vec![pass()], 0),
            (vec![pass(), not_run()], 3),
            (vec![not_run(), refuse(), pass()], 1),
            (vec![refuse(), not_run()], 1),
        ] {
            let found = Precheck {
                commit: "c".repeat(40),
                version: "0.2.0".into(),
                gates,
            };
            assert_eq!(found.exit_code(), code);
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
                 \"release_mode\":\"{mode}\",\"engine\":{{\"commit\":\"{b}\"}}}}"
            )
        };
        assert!(attestation_promotes(&attestation("pass", "full"), &a));
        assert!(attestation_promotes(
            &attestation("skipped", "source-only"),
            &a
        ));
        assert!(!attestation_promotes(&attestation("skipped", "full"), &a));
        assert!(!attestation_promotes(&attestation("pass", "full"), &b));
        assert_eq!(
            attested_engine(&attestation("pass", "full")).as_deref(),
            Some(b.as_str())
        );
        let engineless =
            attestation("pass", "full").replace(&format!(",\"engine\":{{\"commit\":\"{b}\"}}"), "");
        assert_eq!(attested_engine(&engineless), None);
        assert!(!attestation_promotes(&engineless, &a));
    }

    /// The staging tier as `pub promote` asks it again: its main, by `git ls-remote
    #[test]
    fn a_staging_tier_that_moved_since_the_attestation_is_named() {
        let (s, t) = ("5".repeat(40), "6".repeat(40));
        let json = format!(
            "{{\"staging\":{{\"remote\":\"git@gh:o/aterm-staging.git\",\
             \"attested_rev\":\"{s}\"}}}}"
        );
        assert_eq!(
            attested_staging(&json),
            Some(("git@gh:o/aterm-staging.git".to_string(), s.clone()))
        );
        assert_eq!(attested_staging("{\"staging\":{\"remote\":\"r\"}}"), None);
        assert_eq!(attested_staging("not json"), None);
        let main = |sha: &str| format!("{sha}\trefs/heads/main\n");
        assert_eq!(staging_moved_from(&main(&s), &s), None);
        assert_eq!(staging_moved_from(&main(&s.to_uppercase()), &s), None);
        assert!(staging_moved_from(&main(&t), &s).is_some());
        assert!(staging_moved_from("", &s).is_some());
        let twice = format!("{}{}", main(&s), main(&t));
        assert!(staging_moved_from(&twice, &s).is_some());
    }

    /// `--engine` reaches every child: the cut, the stage and `pub publish`'s pre-promote
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
        let publish = real.publish_command(&prover);
        assert_eq!(
            publish.get_program(),
            std::ffi::OsStr::new("/other/publication/bin/pub")
        );
        assert!(
            publish.get_args().any(|a| a == "--skip-site"),
            "the site follows the cut, not the promote"
        );
        let envs: Vec<_> = publish.get_envs().collect();
        assert!(envs.contains(&engine), "{envs:?}");
        let exe = std::env::current_exe().unwrap();
        assert!(
            envs.contains(&(
                std::ffi::OsStr::new(RELEASE_CUTTER_ENV),
                Some(exe.as_os_str())
            )),
            "{envs:?}"
        );
        let edit = real.scrubbed("/other/publication/bin/pub-bump-edit");
        assert!(edit.get_envs().any(|kv| kv == engine));
    }

    /// A step's child under `--verbose`: both its streams reach the step's log on one
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

    /// A signal the release was started ignoring (`nohup`) stays ignored, and every other
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
                let mut cmd = command("nohup");
                cmd.arg(exe);
                cmd
            } else {
                command(exe)
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

    #[test]
    fn kept_audit_rows_go_back_in_time_order() {
        let text = "{\"at\":\"1\"}\n{\"at\":\"3\"}\n";
        let kept = vec!["{\"at\":\"2\"}".to_string(), "{\"at\":\"3\"}".to_string()];
        assert_eq!(
            audit_rows_merged(text, &kept),
            "{\"at\":\"1\"}\n{\"at\":\"2\"}\n{\"at\":\"3\"}\n"
        );
        assert_eq!(parse_offset("+0200"), Some(7200));
        assert_eq!(parse_offset("-0730"), Some(-27000));
        assert!(clears("RUSTFLAGS") && clears("CARGO_TARGET_DIR") && !clears("PATH"));
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
