// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `ship release --pump`: the pump's world over this clone and the cutter.
//!
//! The decisions are [`aterm_release::pump`]'s. This file is what they read (origin/main and
//! its tags through agh, this clone's release journal and repair record, a repair worker's
//! report) and how a verb runs: this same executable, started again in the clone with the
//! verb's arguments, which selects and dispatches to the frozen cutter of the commit it judges
//! exactly as a run a person starts does. Nothing here judges, builds or publishes.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use aterm_release::pump::{self, Handed, Main, Memory, Opened, Progress, Ran, Verb};
use aterm_release::repair;

use crate::interrupt;
use crate::ledger::{GitCli, git_ok};
use crate::release::{
    JOURNAL_FILE, Journal, RELEASE_COMMAND, RUNNING_FILE, ReleaseOptions, SELECTION_FILE,
};

struct Host {
    /// The executable each verb is run with: this one.
    exe: PathBuf,
    repo: PathBuf,
    release: PathBuf,
    /// The Linux declaration the pump was started with, as `ship release` flags.
    declared: Vec<String>,
    /// The flags the pump was started with that its verbs take, each handed only to a verb
    /// that takes it ([`Verb::argv`]).
    handed: Handed,
    /// `--adopt`: the pump may take over a fleet line another holder started.
    adopt: bool,
    /// main's `publish/config.sh`, as last read.
    config: String,
    /// The supersede candidate last found, for the head and main it was found on.
    candidate: Option<((String, String), Option<String>)>,
    tip: String,
}

impl Host {
    fn git(&self) -> GitCli {
        GitCli::new(&self.repo)
    }

    fn text(&self, args: &[&str]) -> Option<String> {
        git_ok(&self.git(), args)
            .ok()
            .map(|out| out.stdout_utf8().trim().to_owned())
    }

    fn version_at(&self, rev: &str) -> Option<String> {
        let manifest = self.text(&["show", &format!("{rev}:Cargo.toml")])?;
        crate::publish::workspace_version(&manifest).ok()
    }

    /// The commit of main that opened `version`: the newest whose subject is the bump's and
    /// whose tree declares it where its parent's does not.
    fn opened(&self, version: &str, tip: &str) -> Option<String> {
        let subject = format!("release: {version} ");
        let named = self.text(&[
            "log",
            "-n",
            "16",
            "--format=%H",
            "--fixed-strings",
            &format!("--grep={subject}"),
            tip,
        ])?;
        named
            .lines()
            .find(|commit| {
                self.version_at(commit).as_deref() == Some(version)
                    && self.version_at(&format!("{commit}^")).as_deref() != Some(version)
            })
            .map(str::to_string)
    }

    /// The Linux declaration of a line the pump starts: its own flags, else main's
    /// `PUMP_LINUX_DEFAULT`, which must parse as a release's Linux flags and nothing else.
    fn linux(&self) -> Result<Vec<String>, String> {
        if !self.declared.is_empty() {
            return Ok(self.declared.clone());
        }
        let Some(value) = pump::config_value(&self.config, pump::LINUX_KEY) else {
            return Err(format!(
                "the pump starts lines and a line needs its Linux declaration: pass --mac-only \
                 or --linux-artifacts DIRECTORY to --pump, or set {} in publish/config.sh",
                pump::LINUX_KEY
            ));
        };
        let words: Vec<String> = value.split_whitespace().map(str::to_string).collect();
        let mut args = vec!["release".to_string()];
        args.extend(words.iter().cloned());
        match crate::cli::parse(&args) {
            Ok(crate::cli::Cmd::Release(opts))
                if opts.linux.declared()
                    && opts
                        == (ReleaseOptions {
                            linux: opts.linux.clone(),
                            ..ReleaseOptions::default()
                        }) =>
            {
                Ok(words)
            }
            _ => Err(format!(
                "publish/config.sh's {}=\"{value}\" is not a Linux declaration (--mac-only, or \
                 --linux-artifacts DIRECTORY with its targets and workers)",
                pump::LINUX_KEY
            )),
        }
    }

    fn journal_file(&self) -> Result<Option<Journal>, String> {
        match Journal::load(&self.release.join(SELECTION_FILE))? {
            Some(pending) => Ok(Some(pending)),
            None => Journal::load(&self.release.join(JOURNAL_FILE)),
        }
    }

    /// Whether this clone holds the cut claim of `version`: its cut journal
    /// (`dist/cut-state.toml`) names it, which is what a resumed run reads to finish a version
    /// whose tag is on origin.
    fn claimed(&self, version: &str) -> Result<bool, String> {
        crate::publish::JournalHeader::read_classified(&self.repo.join("dist/cut-state.toml"))
            .map(|header| header.is_some_and(|header| header.version == version))
            .map_err(|unread| match unread {
                crate::publish::JournalUnread::Read(e)
                | crate::publish::JournalUnread::Invalid(e) => {
                    format!("dist/cut-state.toml does not read: {e}")
                }
            })
    }

    /// The repair record's worker `pick` chooses, with what it reports now.
    fn worker(&self, pick: impl Fn(&repair::Worker) -> bool) -> Option<Opened> {
        let record = repair::Record::load(&self.release.join(repair::RECORD_FILE)).ok()?;
        let worker = record.workers.iter().rev().find(|worker| pick(worker))?;
        Some(Opened {
            sid: worker.sid.clone(),
            commit: worker.commit.clone(),
            at: worker.at,
            report: repair::report(&worker.sid),
        })
    }
}

/// This clone's journal as the pump reads it, `cut` saying its version's tag is on origin and
/// `claimed` that this clone holds that cut's claim. A tag on origin is this journal's cut only
/// with the claim: without it another clone took and cut the line, the bare run here is
/// refused for good ("this clone journals no cut of it"), and the journal is lost.
fn journal_seen(journal: Journal, cut: bool, claimed: bool) -> pump::Journal {
    let finished = journal.finished();
    let cut = cut && !finished;
    pump::Journal {
        prepared: journal.prepared.is_some() && !cut && !finished,
        prepared_at: journal.prepared.as_ref().map_or(0, |prepared| prepared.at),
        version: journal.version,
        head: journal.commit,
        finished,
        published: cut && claimed,
        lost: cut && !claimed,
    }
}

/// The newest completed row among `lines`, the survey's `release_line` records of this Mac
/// ([`aterm_agent::release_progress::survey`]): the newest of the live holders of `version`'s
/// line. A dead or unidentified holder's record is no progress of a running verb.
fn newest_row(lines: &[String], version: &str) -> Option<Progress> {
    lines
        .iter()
        .filter_map(|line| {
            let (alive, data) = line
                .strip_prefix("release_line alive=")?
                .split_once(" data=")?;
            if alive != "1" {
                return None;
            }
            let record: aterm_json::Value =
                aterm_json::from_str(&aterm_codec::pct::decode(data)).ok()?;
            (record["line"]["version"].as_str()? == version).then_some(())?;
            Some(Progress {
                at: record["at"].as_u64()?,
                p95_s: record["p95_s"].as_u64().unwrap_or(0),
                boundary: format!(
                    "{} — {}",
                    record["step"].as_str().unwrap_or("?"),
                    record["boundary"].as_str().unwrap_or("?")
                ),
            })
        })
        .max_by_key(|progress| progress.at)
}

/// A verb's child as the pump attends it ([`pump::attend`]).
struct Attended<'a> {
    ended: &'a mut dyn FnMut() -> Option<i32>,
    pid: i32,
    /// The version whose line the verb runs, and the harness state directory its holder
    /// records its rows under.
    version: String,
    state: Option<PathBuf>,
    /// How long the child's process tree has made no progress, as its reads find it.
    stall: aterm_toolchain::stall::Stall,
    /// The last look and when: the rows and the tree are read twice a minute, which is also
    /// the pace the tree's progress is judged over.
    looked: Option<(std::time::Instant, Option<Progress>, Option<u64>)>,
}

impl Attended<'_> {
    /// The run's newest completed row and how long its process tree has been idle, read at
    /// most twice a minute.
    fn look(&mut self) -> (Option<Progress>, Option<u64>) {
        if let Some((at, progress, idle)) = &self.looked
            && at.elapsed() < std::time::Duration::from_secs(30)
        {
            return (progress.clone(), *idle);
        }
        let progress = self.state.as_ref().and_then(|state| {
            newest_row(&aterm_agent::release_progress::survey(state), &self.version)
        });
        let now = std::time::Instant::now();
        let idle = u32::try_from(self.pid)
            .ok()
            .filter(|pid| *pid > 0)
            .and_then(|pid| {
                self.stall
                    .read(pid, aterm_toolchain::stall::tree_work(pid, &[]), now)
            })
            .map(|idle| idle.as_secs());
        self.looked = Some((now, progress.clone(), idle));
        (progress, idle)
    }
}

impl pump::Running for Attended<'_> {
    fn now(&mut self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn ended(&mut self) -> Option<i32> {
        (self.ended)()
    }

    fn progress(&mut self) -> Option<Progress> {
        self.look().0
    }

    fn idle(&mut self) -> Option<u64> {
        self.look().1
    }

    fn stop_asked(&mut self) -> bool {
        interrupt::caught().is_some()
    }

    fn interrupt(&mut self) {
        if self.pid > 0 {
            interrupt::send(self.pid, 15);
        }
    }

    fn pause(&mut self) {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    fn say(&mut self, line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

impl pump::World for Host {
    fn now(&mut self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn main(&mut self) -> Result<Main, String> {
        let origin = crate::publish::native_origin(&self.repo).map_err(|e| e.to_string())?;
        let tip = origin.refresh_main()?;
        let version = self
            .version_at(&tip)
            .ok_or_else(|| format!("{tip} declares no workspace version"))?;
        let published = origin.get(&format!("refs/tags/v{version}"))?.is_some();
        let fixtures = published
            && crate::fixtures::on_commit(&self.git(), &tip, &version)
                .map_err(|e| e.to_string())?
                .is_some();
        let (line, holder) = if published {
            (None, None)
        } else {
            let claim = crate::line_claim::existing_with_holder(&self.repo, &version)?;
            (
                claim.line.map(|line| line.head().to_owned()),
                claim.holder.map(|(_, holder)| holder),
            )
        };
        let opened = (!published).then(|| self.opened(&version, &tip)).flatten();
        let behind = opened
            .as_ref()
            .and_then(|opened| {
                self.text(&["rev-list", "--count", &format!("{opened}..{tip}")])?
                    .parse()
                    .ok()
            })
            .unwrap_or(0);
        self.config = self
            .text(&["show", &format!("{tip}:publish/config.sh")])
            .unwrap_or_default();
        self.tip.clone_from(&tip);
        Ok(Main {
            opened,
            behind,
            tip,
            version,
            published,
            fixtures,
            line,
            holder,
        })
    }

    fn journal(&mut self) -> Result<Option<pump::Journal>, String> {
        let Some(journal) = self.journal_file()? else {
            return Ok(None);
        };
        if journal.supersede.is_some() {
            return Err(format!(
                "{SELECTION_FILE} holds an undecided supersession, which is a session's to \
                 decide: {RELEASE_COMMAND} --commit <its replacement> --supersede"
            ));
        }
        let finished = journal.finished();
        let cut = !finished && {
            let origin = crate::publish::native_origin(&self.repo).map_err(|e| e.to_string())?;
            origin
                .get(&format!("refs/tags/v{}", journal.version))?
                .is_some()
        };
        let claimed = cut && self.claimed(&journal.version)?;
        Ok(Some(journal_seen(journal, cut, claimed)))
    }

    fn holder(&mut self) -> Option<String> {
        crate::release::live_holder(&self.release.join(RUNNING_FILE), &mut |pid| {
            crate::release_steps::started_of(pid)
        })
        .map(|holder| format!("pid {}, started {}", holder.pid, holder.started))
    }

    fn publishes(&mut self) -> bool {
        pump::config_value(&self.config, pump::PUBLISH_KEY) == Some("1")
    }

    fn adopts(&mut self) -> bool {
        self.adopt
    }

    fn repair(&mut self, version: &str, commit: Option<&str>) -> Option<Opened> {
        // A red of --open-next is kept under the version it would have opened, at whatever tip
        // it judged: it is no red of a line, and `opening` reads it.
        self.worker(|worker| {
            worker.of_cut()
                && worker.version == version
                && commit.is_none_or(|commit| worker.commit == commit)
        })
    }

    fn opening(&mut self, from: &str) -> Option<Opened> {
        let hash = repair::opening_hash(from);
        self.worker(|worker| worker.hash == hash)
    }

    fn holds(&mut self, commit: &str, tip: &str) -> bool {
        git_ok(&self.git(), &["merge-base", "--is-ancestor", commit, tip]).is_ok()
    }

    fn admits(&mut self, fix: &str) -> Result<(), String> {
        let journal = self
            .journal_file()?
            .ok_or("this clone has no line to carry a fix onto")?;
        let git = self.git();
        crate::line::git_ready(&git).map_err(|stop| stop.to_string())?;
        crate::line::admit(&git, &journal.line(), fix)
            .map(|_| ())
            .map_err(|stop| stop.to_string())
    }

    fn green(&mut self, head: &str) -> Option<String> {
        let key = (head.to_string(), self.tip.clone());
        if let Some((found_for, found)) = &self.candidate
            && *found_for == key
        {
            return found.clone();
        }
        let found = crate::main_lane::newest_pass(&self.repo, head);
        self.candidate = Some((key, found.clone()));
        found
    }

    fn run(&mut self, verb: &Verb) -> Ran {
        let unstarted = |why: String| Ran {
            code: 2,
            last: format!("pump: {why}"),
        };
        let linux = if matches!(verb, Verb::Start(_) | Verb::Adopt) {
            match self.linux() {
                Ok(linux) => linux,
                Err(why) => return unstarted(why),
            }
        } else {
            Vec::new()
        };
        // The line the verb runs is of this clone's unfinished journal, else of main's version.
        let version = self
            .journal_file()
            .ok()
            .flatten()
            .filter(|journal| !journal.finished())
            .map(|journal| journal.version)
            .or_else(|| self.version_at(&self.tip))
            .unwrap_or_default();
        let exe = self.exe.clone();
        let mut command = aterm_uds::spawnfd::command(&exe);
        command
            .args(verb.argv(&linux, &self.handed))
            .current_dir(&self.repo)
            .env(repair::PUMP_ENV, "1")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return unstarted(format!("cannot start {}: {error}", exe.display())),
        };
        // The run's lines are passed on as they come. A reader of its own keeps the pump off a
        // pipe that a child the run left behind (its background pre-freeze) still holds.
        let (lines, said) = std::sync::mpsc::channel::<String>();
        let reader = child.stdout.take().map(|stdout| {
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{line}");
                        let _ = out.flush();
                    }
                    if lines.send(line).is_err() {
                        break;
                    }
                }
            })
        });
        // A signal to the pump is the run's: it ends INTERRUPTED at its next boundary, which
        // its journal resumes from, and the pump then stops. A run that prepares and has
        // stalled is interrupted the same way, and the pump resumes it; the publish is not.
        let pid = i32::try_from(child.id()).unwrap_or(0);
        let mut ended = || match child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(130)),
            Ok(None) => None,
            Err(_) => Some(3),
        };
        let code = pump::attend(
            &mut Attended {
                ended: &mut ended,
                pid,
                version,
                state: aterm_agent::harness::cli::Env::from_process()
                    .ok()
                    .map(|env| env.state),
                stall: aterm_toolchain::stall::Stall::default(),
                looked: None,
            },
            !matches!(verb, Verb::Finish),
        );
        // What the run wrote before it ended is in the pipe: give the reader the release's own
        // five-second drain grace, then go on without it.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while reader.as_ref().is_some_and(|reader| !reader.is_finished())
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let mut last = String::new();
        while let Ok(line) = said.try_recv() {
            let kept = pump::final_line([last.as_str(), line.as_str()]).to_owned();
            last = kept;
        }
        Ran { code, last }
    }

    fn wait(&mut self, secs: u64) -> bool {
        for _ in 0..secs {
            if interrupt::caught().is_some() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        interrupt::caught().is_none()
    }

    fn say(&mut self, line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn remember(&mut self, memory: &Memory) {
        let path = self.release.join(pump::MEMORY_FILE);
        let kept = aterm_toml::to_string(memory)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o600)
                    .map_err(|e| e.to_string())
            });
        if let Err(why) = kept {
            self.say(&format!(
                "pump: cannot keep {} ({why}); a restart may repeat the last run",
                path.display()
            ));
        }
    }
}

fn remembered(release: &Path) -> Memory {
    crate::durable::read_regular_text(&release.join(pump::MEMORY_FILE), 1024 * 1024)
        .ok()
        .and_then(|text| aterm_toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// `ship release --pump`: 0 once asked to stop (or after `--once`'s cycle, or a dry look), 1
/// when another pump holds this clone, 2 where there is no clone to run in.
pub fn run(repo: &Path, opts: &ReleaseOptions) -> i32 {
    let release = match crate::release::release_dir(&GitCli::new(repo)) {
        Ok(release) => release,
        Err(why) => {
            eprintln!("release pump: {why}");
            return 2;
        }
    };
    if let Err(error) = std::fs::create_dir_all(&release) {
        eprintln!("release pump: create {}: {error}", release.display());
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| repo.to_path_buf());
    let opts = opts.anchored(&cwd);
    let handed = Handed {
        engine: opts.engine.as_ref().map(|path| path.display().to_string()),
        credentials: opts
            .release_credentials
            .as_ref()
            .map(|path| path.display().to_string()),
        verbose: opts.verbose,
    };
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            eprintln!("release pump: cannot name this executable: {error}");
            return 2;
        }
    };
    let mut host = Host {
        exe,
        repo: repo.to_path_buf(),
        release: release.clone(),
        declared: if opts.linux.declared() {
            opts.linux.cut_args()
        } else {
            Vec::new()
        },
        handed,
        adopt: opts.pump_adopt,
        config: String::new(),
        candidate: None,
        tip: String::new(),
    };
    let memory = remembered(&release);
    if opts.dry_run {
        // One look and its decision; nothing runs and nothing is kept.
        let line = match pump::observe(&mut host) {
            Ok(seen) => match pump::decide(&seen, &memory) {
                pump::Decision::Run { verb, why, .. } => {
                    let linux = host.linux().unwrap_or_default();
                    format!(
                        "pump: DRY RUN — {why}; would run {RELEASE_COMMAND} {}",
                        verb.argv(&linux, &host.handed)[1..].join(" ")
                    )
                }
                pump::Decision::Wait { why, .. } => format!("pump: DRY RUN — would wait: {why}"),
            },
            Err(why) => format!("pump: DRY RUN — cannot read the world: {why}"),
        };
        pump::World::say(&mut host, &line);
        return 0;
    }
    // One pump per clone: a second one would run the same verbs against the same journal.
    let lock = release.join("pump.lock");
    let Ok(_held) =
        aterm_update_core::FileLock::acquire_within(&lock, std::time::Duration::from_secs(2))
    else {
        eprintln!(
            "release pump: REFUSED — another pump runs in this clone ({} is held); stop it, \
             or use a clone of its own",
            lock.display()
        );
        return 1;
    };
    interrupt::install();
    pump::World::say(
        &mut host,
        &format!(
            "pump: started in {} (pid {}); it runs --open-next, --prepare [--commit|--fix] and, \
             with {}=\"1\" on main, the publish — never --forget, --bump or --supersede; {}",
            repo.display(),
            std::process::id(),
            pump::PUBLISH_KEY,
            if opts.pump_adopt {
                "with --adopt it takes over a fleet line whose holder is dead or stalled"
            } else {
                "it leaves a fleet line another holder started to that holder"
            }
        ),
    );
    pump::pump(&mut host, memory, opts.pump_once.then_some(1));
    pump::World::say(&mut host, "pump: stopped");
    0
}

/// The pump's launchd agent, one per user.
const LABEL: &str = "systems.alab.aterm-release-pump";

/// The seconds launchd gives the pump between SIGTERM and SIGKILL: the pump passes the stop on
/// to the release it runs and waits for it to end INTERRUPTED at a boundary.
const EXIT_TIMEOUT_SECS: u64 = 120;

/// How long an uninstall or a re-install waits for the loaded pump to be gone: its whole
/// `ExitTimeOut` and a margin, so a pump mid-run is waited out, never given up on.
const UNLOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(EXIT_TIMEOUT_SECS + 30);

/// The arguments the installed agent runs the pump with: `release --pump`, the Linux
/// declaration and the flags it hands each verb. Never a mode of one invocation (`--install`,
/// `--once`, `--dry-run`).
fn service_args(opts: &ReleaseOptions) -> Vec<String> {
    let mut args = vec!["release".to_string(), "--pump".to_string()];
    if opts.pump_adopt {
        args.push("--adopt".into());
    }
    if opts.linux.declared() {
        args.extend(opts.linux.cut_args());
    }
    for (flag, path) in [
        ("--engine", &opts.engine),
        ("--release-credentials", &opts.release_credentials),
    ] {
        if let Some(path) = path {
            args.extend([flag.to_string(), path.display().to_string()]);
        }
    }
    if opts.verbose {
        args.push("--verbose".into());
    }
    args
}

/// The agent's property list. `ProcessType Interactive` because a launchd job otherwise runs,
/// and starts every child, at utility QoS, which starves the release's paint measurement
/// (docs/RELEASING.md § *MEASURE's paint rows must run at interactive QoS*); `KeepAlive`
/// because a pump that died is the gap the pump exists to close. The executable is an immutable
/// copy and the working directory the release clone, so neither depends on a worktree.
fn manifest(
    exe: &Path,
    repo: &Path,
    release: &Path,
    home: &Path,
    path: &str,
    args: &[String],
) -> String {
    use aterm_verify::launch_agent::xml;
    let words: String = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(args.iter().cloned())
        .map(|word| format!("<string>{}</string>", xml(&word)))
        .collect();
    let (repo, home, path) = (
        xml(&repo.to_string_lossy()),
        xml(&home.to_string_lossy()),
        xml(path),
    );
    let log = xml(&release.join("pump.log").to_string_lossy());
    let err = xml(&release.join("pump.err").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array>{words}</array>
<key>WorkingDirectory</key><string>{repo}</string>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home}</string><key>PATH</key><string>{path}</string></dict>
<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>
<key>ThrottleInterval</key><integer>60</integer>
<key>ExitTimeOut</key><integer>{EXIT_TIMEOUT_SECS}</integer>
<key>ProcessType</key><string>Interactive</string>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{err}</string>
</dict></plist>
"#
    )
}

/// `--pump --install` and `--pump --uninstall`: the line that says what was done.
/// # Errors
/// Why the agent is not as asked.
pub fn service(repo: &Path, opts: &ReleaseOptions) -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("the pump's service is a launchd agent: macOS only".into());
    }
    let home = std::env::var_os("HOME").ok_or("no HOME")?;
    let home = Path::new(&home);
    if opts.pump_uninstall {
        use aterm_verify::launch_agent::Removed;
        return Ok(
            match aterm_verify::launch_agent::remove(home, LABEL, UNLOAD_WAIT)? {
                Removed::Unloaded => format!(
                    "release pump uninstalled: {LABEL} is unloaded and its property list removed"
                ),
                Removed::Absent => format!("release pump: {LABEL} was not installed"),
                Removed::Exiting(why) => format!(
                    "release pump uninstalled: {LABEL}'s property list is removed, so it does \
                     not start again; its job was still ending when the wait gave up ({why}) — \
                     launchd ends it, and the release it ran stops INTERRUPTED at its next \
                     boundary, which its journal resumes from"
                ),
            },
        );
    }
    let release = crate::release::release_dir(&GitCli::new(repo))?;
    std::fs::create_dir_all(&release).map_err(|e| format!("create {}: {e}", release.display()))?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| repo.to_path_buf());
    let opts = opts.anchored(&cwd);
    // Immutable deployments: never overwrite a possibly executing Mach-O, and never depend on
    // the build directory this image ran from.
    let token = aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?;
    let deployment = home
        .join(".local/state/aterm-release/.ubd/pump/controllers")
        .join(token);
    std::fs::create_dir_all(&deployment).map_err(|e| e.to_string())?;
    let exe = deployment.join("aterm-release");
    std::fs::copy(std::env::current_exe().map_err(|e| e.to_string())?, &exe)
        .map_err(|e| e.to_string())?;
    let text = manifest(
        &exe,
        repo,
        &release,
        home,
        &std::env::var("PATH").unwrap_or_default(),
        &service_args(&opts),
    );
    let target = aterm_verify::launch_agent::replace(home, LABEL, &text, UNLOAD_WAIT)?;
    Ok(format!(
        "release pump installed: {target}\nclone={}\nlog={}\nstop it: {RELEASE_COMMAND} --pump --uninstall",
        repo.display(),
        release.join("pump.log").display()
    ))
}

#[cfg(test)]
#[path = "release_pump_tests.rs"]
mod tests;
