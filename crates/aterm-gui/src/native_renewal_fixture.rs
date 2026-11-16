// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit Rust-only real-process qualification host; never a CLI/environment
//! activation path in the installed application.
use super::*;
use aterm_session::LaunchNonce;
use aterm_window::application::ApplicationHandler;
use aterm_window::event::WindowEvent;
use aterm_window::event_loop::ActiveEventLoop;
use aterm_window::platform::pump_events::EventLoopExtPumpEvents;
use aterm_window::window::WindowId;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "native_renewal_live_snapshot.rs"]
mod live_snapshot;

#[cfg(test)]
#[path = "native_renewal_fixture_tests.rs"]
mod tests;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRenewalFixture {
    pub root: PathBuf,
    /// Explicit test-only shared pool, preseeded by the fixture driver. Each
    /// native host still owns its own root, control socket, PTY and receipts.
    pub shared_pool_root: Option<PathBuf>,
    pub codex: PathBuf,
    pub cwd: PathBuf,
    pub auth_path: PathBuf,
    pub successor_auth_path: Option<PathBuf>,
    #[serde(default)]
    pub additional_auth_paths: Vec<PathBuf>,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub guardian_socket: Option<PathBuf>,
    pub adopt: Option<NativeRenewalAdoption>,
    pub human_grace_s: Option<u32>,
    pub continue_policy: Option<bool>,
    #[serde(default)]
    pub weekly_quota: bool,
    /// A lost login's move (`codex-login-rotation`): the pool, policy and usage server a
    /// usage-limit handoff's fixture lays out, with the move rather than the handoff enabled.
    #[serde(default)]
    pub lost_login: bool,
    /// How the shell starts codex ([`NativeRenewalLaunch`]); absent, codex leads the PTY's
    /// foreground job, as it always did.
    #[serde(default)]
    pub launch: NativeRenewalLaunch,
}

/// How the fixture's shell starts codex. Only [`Self::Leader`] leaves codex leading the PTY's
/// foreground process group (`tcgetpgrp`), which an exec run's claim of its tab requires; each
/// other shape starts it where a run a person or an agent starts often sits. Each but the leader
/// runs codex through [`RUN_REPORT`], which appends its pid to the host's launch record and then
/// execs codex in its own place, so the record names codex's own process. On the wire it is a bare
/// word (`"sh-child"`), or `{"background": <runs>}`; any other word fails the config's parse.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NativeRenewalLaunch {
    /// The shell runs codex as its foreground job, which codex leads.
    #[default]
    Leader,
    /// A child of `sh -c`, which leads the foreground job.
    ShChild,
    /// The last stage of `true | codex …`, in the foreground job its first stage leads (zsh makes
    /// a pipeline's first stage its leader, so `codex … | cat` would leave codex leading). Codex's
    /// stdin is that empty pipe: `codex exec` with a positional prompt says `Reading additional
    /// input from stdin...` on stderr, reads the pipe to its end and adds it to the prompt only
    /// when it is not empty, so the prompt is unchanged.
    Pipeline,
    /// That many runs, each started with `&` and leading a background job of its own, while the
    /// shell waits for them and, for an exec run, reports each one's pid and status as it ends
    /// ([`BACKGROUND_WAIT`]).
    Background(LaunchRuns),
    /// A child of the `sh -c` a `setsid` helper ([`SETSID`]) became: in a session of its own,
    /// with no controlling terminal. The helper leads the foreground job, so it forks first and
    /// waits, as util-linux `setsid --wait` does.
    Setsid,
    /// A tool shell's `sh -c 'setsid sh -c "codex …"'`, run under one more `sh -c` as an agent's
    /// tool shell runs it: codex as [`Self::Setsid`] has it, four hops below the tab's shell.
    SetsidChain,
}

/// How many runs [`NativeRenewalLaunch::Background`] starts: 1 to [`LaunchRuns::MAX`], a bare
/// number on the wire, any other number failing the parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchRuns(u8);

impl LaunchRuns {
    /// The most runs one PTY starts.
    pub const MAX: u8 = 16;
}

impl TryFrom<u8> for LaunchRuns {
    type Error = String;

    fn try_from(runs: u8) -> Result<Self, String> {
        if (1..=Self::MAX).contains(&runs) {
            Ok(Self(runs))
        } else {
            Err(format!("background runs must be 1 to {}", Self::MAX))
        }
    }
}

impl<'de> Deserialize<'de> for LaunchRuns {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(u8::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for LaunchRuns {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

/// `sh -c` script that runs its arguments as one command and exits with its status; the trailing
/// `exit` keeps every `sh` from replacing itself with that command.
const SH_CHILD: &str = "\"$@\"; exit \"$?\"";

/// `sh -c` script that appends its own pid to the launch record its first argument names, then
/// replaces itself with the rest of its arguments: codex's argv, never parsed by a shell.
const RUN_REPORT: &str = "printf \"%s\\n\" \"$$\" >>\"$1\" || exit 126; shift; exec \"$@\"";

/// The `setsid` helper and its arguments up to the program it runs. macOS ships no `setsid`, so
/// this is util-linux `setsid --wait` in perl: a process leading its group (as each job a shell
/// starts does) forks and waits for its child, exiting with that child's status; the process that
/// does not lead one starts a session of its own and execs the program in its own place.
const SETSID: [&str; 4] = [
    "/usr/bin/perl",
    "-e",
    "use POSIX (); if (getpgrp == $$) { defined(my $child = fork) or die qq(setsid: fork: $!\\n); \
     if ($child) { waitpid($child, 0) == $child or die qq(setsid: wait: $!\\n); \
     exit($? & 127 ? 128 + ($? & 127) : $? >> 8) } } \
     defined POSIX::setsid() or die qq(setsid: $!\\n); \
     exec { $ARGV[0] } @ARGV or die qq(setsid: exec: $!\\n)",
    "--",
];

/// What an exec run's line ends with: its exit status on a line of its own, as the driver reads it.
const EXEC_STATUS: &str = "printf '\\nEXEC_FIXTURE_EXIT=%s\\n' \"$?\"";

/// How the shell waits for the exec runs [`NativeRenewalLaunch::Background`] started, their pids in
/// `$fixture_runs`: as each one ends, in whatever order, it prints `EXEC_FIXTURE_RUN=<pid>` and
/// then [`EXEC_STATUS`]'s line for it, so a status names its run and none waits behind a run still
/// going. zsh's `wait` takes no `-n`, so the shell looks at each run every tenth of a second, with
/// `kill -0` and `zselect` (`zsh/zselect`), both builtins: the wait starts no process. A run zsh has
/// reaped answers no `kill -0`, and `wait` still hands back its status.
const BACKGROUND_WAIT: &str = "while (( $#fixture_runs )); do for fixture_run in $fixture_runs; do \
     kill -0 $fixture_run 2>/dev/null && continue; wait $fixture_run; \
     printf '\\nEXEC_FIXTURE_RUN=%s\\nEXEC_FIXTURE_EXIT=%s\\n' $fixture_run \"$?\"; \
     fixture_runs=( ${fixture_runs:#$fixture_run} ); done; \
     (( $#fixture_runs )) && zselect -t 10; done";

/// `word` as one word of the line the fixture types into its interactive zsh: in single quotes
/// when it is all printable ASCII (the only form before launch shapes), else as `$'…'` with each
/// byte outside printable ASCII a `\xHH` escape, so no byte of it reaches the line editor as a key
/// it acts on.
///
/// # Errors
/// `word` holds a NUL, which no argv word can carry.
fn typed_word(word: &str) -> Result<String, String> {
    if word.contains('\0') {
        return Err("fixture argv word holds a NUL".into());
    }
    if word.bytes().all(|byte| (0x20..0x7f).contains(&byte)) {
        return Ok(format!("'{}'", word.replace('\'', "'\\''")));
    }
    let mut typed = String::from("$'");
    for byte in word.bytes() {
        match byte {
            b'\\' => typed.push_str("\\\\"),
            b'\'' => typed.push_str("\\'"),
            0x20..=0x7e => typed.push(char::from(byte)),
            _ => typed.push_str(&format!("\\x{byte:02x}")),
        }
    }
    typed.push('\'');
    Ok(typed)
}

impl NativeRenewalLaunch {
    /// How many codex runs it starts.
    fn runs(self) -> usize {
        match self {
            Self::Background(runs) => usize::from(runs.0),
            _ => 1,
        }
    }

    /// The line the fixture types into its shell ([`type_line`]) to start `codex` with `args` as
    /// this shape has it, each run but a leader's reporting its pid to `record`. An exec run
    /// (`args` opening with `exec`) ends each run with [`EXEC_STATUS`]. Every word is typed through
    /// [`typed_word`], and each `sh -c` and the helper pass codex's argv on as arguments, so no
    /// word of it is parsed by a shell. A leader's line, for an argv of printable ASCII, is the one
    /// the fixture typed before launch shapes, byte for byte.
    ///
    /// # Errors
    /// A word holds a NUL, or `record` is not UTF-8.
    fn line(self, codex: &Path, args: &[String], record: &Path) -> Result<String, String> {
        let record = record.to_str().ok_or("fixture launch record path")?;
        let report = ["/bin/sh", "-c", RUN_REPORT, "fixture-run", record];
        let run = std::iter::once(codex.to_string_lossy().into_owned())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>();
        let words = |prefix: &[&[&str]]| -> Result<String, String> {
            let report: &[&str] = if self == Self::Leader { &[] } else { &report };
            let typed = prefix
                .iter()
                .chain([&report])
                .flat_map(|group| group.iter().copied())
                .chain(run.iter().map(String::as_str))
                .map(typed_word)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(typed.join(" "))
        };
        let sh = |name| ["/bin/sh", "-c", SH_CHILD, name];
        let status = if args.first().is_some_and(|arg| arg == "exec") {
            format!("; {EXEC_STATUS}")
        } else {
            String::new()
        };
        let line = match self {
            Self::Leader => words(&[])?,
            Self::ShChild => words(&[&sh("fixture-sh-child")])?,
            Self::Pipeline => format!("{} | {}", typed_word("/usr/bin/true")?, words(&[])?),
            Self::Setsid => words(&[&SETSID, &sh("fixture-setsid")])?,
            Self::SetsidChain => words(&[
                &sh("fixture-tool-shell"),
                &sh("fixture-sh"),
                &SETSID,
                &sh("fixture-setsid"),
            ])?,
            Self::Background(runs) => {
                // The blank after `$!` keeps interactive zsh's history expansion off it: `!)`
                // would read as a history event and the shell would run nothing.
                let start = format!(
                    "repeat {} do {} & fixture_runs+=( $! ); done",
                    runs.0,
                    words(&[])?
                );
                // An exec run's wait needs `zsh/zselect`; where it does not load, no run starts.
                return Ok(if status.is_empty() {
                    format!("fixture_runs=(); {start}; wait\n")
                } else {
                    format!("fixture_runs=(); zmodload zsh/zselect && {start}; {BACKGROUND_WAIT}\n")
                });
            }
        };
        Ok(format!("{line}{status}\n"))
    }

    /// Ready this shape to start: a leader needs nothing; each other shape needs the empty launch
    /// record its runs report to, made here (one already there would name runs this host did
    /// not start), and a setsid shape needs its helper.
    ///
    /// # Errors
    /// The helper is missing, or the record exists or cannot be made.
    fn prepare(self, record: &Path) -> Result<(), String> {
        self.prepare_with(record, Path::new(SETSID[0]))
    }

    /// [`Self::prepare`], the setsid helper's interpreter at `helper`.
    fn prepare_with(self, record: &Path, helper: &Path) -> Result<(), String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        if self == Self::Leader {
            return Ok(());
        }
        if matches!(self, Self::Setsid | Self::SetsidChain) && !helper.is_file() {
            return Err(format!(
                "fixture setsid helper {} missing",
                helper.display()
            ));
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(record)
            .map(drop)
            .map_err(|error| format!("fixture launch record: {error}"))
    }

    /// The pid of each codex run the line started, each run added to `runs` the moment its pid is
    /// seen, so a teardown after a failure here still ends it ([`end_runs`]). A leader's is the
    /// PTY's foreground group once it is not the shell's (`shell_pid`), or none after
    /// [`MUST_HAPPEN`], as before launch shapes; each other shape's are the pids its runs appended
    /// to `record`, every run's within [`MUST_HAPPEN`]. A pid shows a moment before its process
    /// execs codex, while it is still the shell's fork or [`RUN_REPORT`]'s `sh`, so each is
    /// answered only once its argument record is no longer that one's: the image is then the one
    /// the line started. A run that ended before that is answered as it is.
    ///
    /// # Errors
    /// Not every run reported within [`MUST_HAPPEN`], or `record` holds more runs than started,
    /// or a line that is no pid, or does not read; or a run still there did not exec within
    /// [`MUST_HAPPEN`], or the shell's argument record does not read.
    fn started(
        self,
        master: i32,
        shell_pid: i32,
        record: &Path,
        runs: &mut Vec<Run>,
    ) -> Result<Vec<i32>, String> {
        self.started_reading(
            master,
            shell_pid,
            record,
            runs,
            aterm_uds::process::read_process_argv,
        )
    }

    /// [`Self::started`], each process's argument record read by `argv`.
    fn started_reading(
        self,
        master: i32,
        shell_pid: i32,
        record: &Path,
        runs: &mut Vec<Run>,
        mut argv: impl FnMut(i32) -> Option<Vec<OsString>>,
    ) -> Result<Vec<i32>, String> {
        let pids = if self == Self::Leader {
            let launched = Instant::now();
            let child_pid = loop {
                let foreground = crate::quit_safety::foreground_pgrp(master);
                if foreground > 1 && foreground != shell_pid {
                    break Some(foreground);
                }
                if launched.elapsed() >= MUST_HAPPEN {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            runs.extend(child_pid.and_then(Run::observe));
            child_pid.into_iter().collect::<Vec<_>>()
        } else {
            self.reported(record, runs)?
        };
        if pids.is_empty() {
            return Ok(pids);
        }
        // Until it execs, a process's argument record is the one it was forked or exec'd with.
        let forked = if self == Self::Leader {
            argv(shell_pid).ok_or("the fixture shell's argument record does not read")?
        } else {
            ["/bin/sh", "-c", RUN_REPORT].map(OsString::from).to_vec()
        };
        let waiting = Instant::now();
        for &pid in &pids {
            while runs.iter().any(|run| run.pid == pid && run.live())
                && argv(pid).is_none_or(|words| words.starts_with(&forked))
            {
                if waiting.elapsed() >= MUST_HAPPEN {
                    return Err(format!("fixture run {pid} never ran past its fork"));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(pids)
    }

    /// The pids this shape's runs appended to `record`, each run added to `runs` (where its pid
    /// still names a live process) the moment it is seen.
    ///
    /// # Errors
    /// Not every run reported within [`MUST_HAPPEN`], or `record` holds more runs than started,
    /// or a line that is no pid, or does not read.
    fn reported(self, record: &Path, runs: &mut Vec<Run>) -> Result<Vec<i32>, String> {
        let launched = Instant::now();
        let expected = self.runs();
        let mut pids = Vec::new();
        loop {
            let text = std::fs::read_to_string(record)
                .map_err(|error| format!("fixture launch record: {error}"))?;
            let complete = text.rsplit_once('\n').map_or("", |(lines, _)| lines);
            for line in complete.lines().skip(pids.len()) {
                let pid = line
                    .parse::<i32>()
                    .ok()
                    .filter(|pid| *pid > 1)
                    .ok_or("fixture launch record holds a line that is no pid")?;
                if pids.len() == expected {
                    return Err(format!(
                        "fixture launch record names more runs than the {expected} the line starts"
                    ));
                }
                pids.push(pid);
                runs.extend(Run::observe(pid));
            }
            if pids.len() == expected {
                return Ok(pids);
            }
            if launched.elapsed() >= MUST_HAPPEN {
                return Err(format!(
                    "fixture launch started {} of {expected} runs",
                    pids.len()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A codex run the launch started: its pid, and the birth that tells it from a later process
/// given the same pid. The birth is the fork's, kept through every exec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    pid: i32,
    birth: aterm_uds::process::Birth,
}

impl Run {
    /// `pid`'s run, or none where no live process of this user answers it.
    fn observe(pid: i32) -> Option<Self> {
        aterm_uds::process::read_identity(pid).map(|identity| Self {
            pid,
            birth: identity.birth,
        })
    }

    /// Whether this run is still there: its pid names a live process (no zombie) with its birth.
    fn live(self) -> bool {
        aterm_uds::process::read_identity(self.pid).is_some_and(|now| now.birth == self.birth)
    }

    /// Send `signal` to this run while its pid still names it.
    fn signal(self, signal: i32) {
        if self.live() {
            // SAFETY: `kill` posts a signal to one pid, just read as this run's, and takes no
            // pointers.
            unsafe { libc::kill(self.pid, signal) };
        }
    }
}

/// End each of `runs` still there: `SIGTERM`, then `SIGKILL` for any left after
/// [`aterm_uds::process::END_GRACE`], then wait until none is left. The shell's hang-up reaches
/// only the runs in the PTY's session (zsh hangs up its jobs, the kernel the foreground group); a
/// run in a session of its own, as both setsid shapes start it, has no controlling terminal and
/// hears nothing of it, and nothing else ends it.
///
/// # Errors
/// A run outlived its `SIGKILL` by [`MUST_HAPPEN`].
fn end_runs(runs: &[Run]) -> Result<(), String> {
    let left = || {
        runs.iter()
            .copied()
            .filter(|run| run.live())
            .collect::<Vec<_>>()
    };
    let left_after = |within: Duration| {
        let since = Instant::now();
        loop {
            let left = left();
            if left.is_empty() || since.elapsed() >= within {
                return left;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    for run in left() {
        run.signal(libc::SIGTERM);
    }
    for run in left_after(aterm_uds::process::END_GRACE) {
        run.signal(libc::SIGKILL);
    }
    let left = left_after(MUST_HAPPEN);
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "fixture runs {:?} outlived their SIGKILL",
            left.iter().map(|run| run.pid).collect::<Vec<_>>()
        ))
    }
}

/// Type `line` into the shell on `master` once its line editor reads. Until zsh's line editor
/// takes the terminal out of canonical mode, the line discipline holds what is typed as one line
/// of at most `MAX_INPUT` bytes (1024 on macOS) and drops the rest, its newline with it, so a
/// longer line would never run; the editor takes a line of any length. A master that does not
/// block waits out a full input queue. Some reader must drain `master` meanwhile: zsh does not
/// switch modes until what it wrote has been read.
///
/// # Errors
/// The terminal's modes do not read, the editor did not start within [`MUST_HAPPEN`], or the
/// terminal refused the line or took none of it for [`MUST_HAPPEN`].
fn type_line(master: i32, line: &str) -> Result<(), String> {
    let spawned = Instant::now();
    while aterm_pty::tty_echo(master)
        .ok_or("the fixture shell's terminal modes do not read")?
        .canonical
    {
        if spawned.elapsed() >= MUST_HAPPEN {
            return Err("the fixture shell's line editor never started".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut rest = line.as_bytes();
    let mut progressed = Instant::now();
    while !rest.is_empty() {
        match aterm_pty::write_some(master, rest) {
            Ok(written) if written > 0 => {
                rest = rest.get(written..).unwrap_or_default();
                progressed = Instant::now();
                continue;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(format!("typing the fixture launch: {error}")),
        }
        if progressed.elapsed() >= MUST_HAPPEN {
            return Err(format!(
                "the fixture shell took {} of its line's {} bytes",
                line.len() - rest.len(),
                line.len()
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

impl NativeRenewalFixture {
    /// Whether this fixture moves its session to another account: a usage-limit handoff's or a
    /// lost login's.
    const fn moves_account(&self) -> bool {
        self.weekly_quota || self.lost_login
    }

    /// The line this host types to start codex, its runs reporting to `record`; none for a host
    /// that adopts a PTY, which starts no run.
    ///
    /// # Errors
    /// An adopting host is given a launch shape other than the leader, which it could not honour;
    /// or the line refuses ([`NativeRenewalLaunch::line`]).
    fn launch_line(&self, record: &Path) -> Result<Option<String>, String> {
        if self.adopt.is_none() {
            return self.launch.line(&self.codex, &self.args, record).map(Some);
        }
        if self.launch != NativeRenewalLaunch::Leader {
            return Err(
                "an adopting fixture host starts no run, so it takes no launch shape".into(),
            );
        }
        Ok(None)
    }
}

fn fixture_pool_root<'a>(
    root: &'a std::path::Path,
    shared: Option<&'a std::path::Path>,
    weekly_quota: bool,
    adopting: bool,
) -> Result<&'a std::path::Path, String> {
    let Some(shared) = shared else {
        return Ok(root);
    };
    let plain_absolute = |path: &std::path::Path| {
        path.is_absolute()
            && path.components().all(|part| {
                matches!(
                    part,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
    };
    if !weekly_quota
        || adopting
        || !plain_absolute(shared)
        || !plain_absolute(root)
        || root == shared
        || !root.starts_with(shared)
    {
        return Err("shared fixture pool must contain a distinct quota host root".into());
    }
    Ok(shared)
}

#[cfg(test)]
#[test]
fn native_renewal_shared_pool_requires_distinct_owned_quota_hosts() {
    use std::path::Path;
    let pool = Path::new("/fixture-pool");
    let host = pool.join("host-one");
    assert_eq!(fixture_pool_root(&host, None, false, false).unwrap(), host);
    assert_eq!(
        fixture_pool_root(&host, Some(pool), true, false).unwrap(),
        pool
    );
    for (root, weekly, adopting) in [
        (pool, true, false),
        (Path::new("/elsewhere"), true, false),
        (Path::new("/fixture-pool/../elsewhere"), true, false),
        (host.as_path(), false, false),
        (host.as_path(), true, true),
    ] {
        assert!(fixture_pool_root(root, Some(pool), weekly, adopting).is_err());
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRenewalAdoption {
    pub master: i32,
    pub shell_pid: i32,
    pub sid: String,
    pub nonce: String,
    pub prior_host_pid: u32,
    pub prior_host_birth: u64,
    pub waited_pid: u32,
    pub wait_outcome: NativeRenewalWaitOutcome,
}

/// Positive wait/reap evidence from the fixture guardian which spawned the host.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeRenewalWaitOutcome {
    Exited { code: u8 },
    Signaled { signal: u8 },
}

struct FixturePty {
    master: i32,
    shell: aterm_pty::ShellIdentity,
}
impl Drop for FixturePty {
    fn drop(&mut self) {
        aterm_pty::hangup_shell(&self.shell, self.master);
        aterm_pty::close_fd(self.master);
    }
}

fn note_fixture_person(
    ctx: &crate::SessionCtx,
    event: &crate::input::InputEvent,
    source: crate::input::Source,
) {
    if matches!(source, crate::input::Source::Human) && crate::human_input::is_person_gesture(event)
    {
        let _ = crate::app_input::note_person_event(ctx, event);
    }
}

#[cfg(test)]
#[test]
fn codex_renewal_fixture_counts_real_human_source_only() {
    let handle = crate::session_store::test_handle_on(8876, -1);
    let event = crate::input::InputEvent::KeySequence(vec![b'x']);
    let before = handle.ctx.human_input.seq();
    note_fixture_person(&handle.ctx, &event, crate::input::Source::Controller);
    assert_eq!(handle.ctx.human_input.seq(), before);
    note_fixture_person(&handle.ctx, &event, crate::input::Source::Human);
    assert!(handle.ctx.human_input.seq() > before);
}

pub(super) struct Pump {
    ctx: Arc<SessionCtx>,
    term: Arc<Mutex<Terminal>>,
    handle: crate::session_store::SessionHandle,
    unhandled_wakes: std::collections::BTreeSet<String>,
}
impl ApplicationHandler<Wake> for Pump {
    fn resumed(&mut self, _: &ActiveEventLoop) {}
    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    fn user_event(&mut self, _: &ActiveEventLoop, wake: Wake) {
        self.dispatch(wake);
    }
}

impl Pump {
    pub(super) fn source_fixture(
        handle: crate::session_store::SessionHandle,
        term: Arc<Mutex<Terminal>>,
    ) -> Self {
        Self {
            ctx: Arc::clone(&handle.ctx),
            term,
            handle,
            unhandled_wakes: Default::default(),
        }
    }

    fn dispatch(&mut self, wake: Wake) {
        let wake = crate::control::control_media::take_hop(wake);
        let named = match &wake {
            Wake::ReadSessionStatus { .. } => Some("ReadSessionStatus"),
            Wake::ReadSessionStatuses { .. } => Some("ReadSessionStatuses"),
            Wake::ReadHarnessPolicy { .. } => Some("ReadHarnessPolicy"),
            Wake::ReadSessionWindows { .. } => Some("ReadSessionWindows"),
            Wake::ReadAppStatus { .. } => Some("ReadAppStatus"),
            Wake::PermissionWalkAsked { .. } => Some("PermissionWalkAsked"),
            Wake::FrontControlSurface { .. } => Some("FrontControlSurface"),
            _ => None,
        };
        if let Some(name) = named {
            eprintln!(
                "native-renewal-fixture pid={} wake={name}",
                std::process::id()
            );
        }
        let tag = format!("{:?}", std::mem::discriminant(&wake));
        match wake {
            Wake::Input {
                batch,
                mark,
                reply,
                fence,
                src,
                ..
            } => {
                let _fenced = match fence.as_ref() {
                    None => None,
                    Some(crate::session_timeline::SupervisorInputDelivery::Legacy(holder)) => {
                        let lease = crate::session_timeline::FencedWrite::enter(
                            &self.ctx,
                            holder,
                            crate::metrics::now_us(),
                        )
                        .ok();
                        if lease.is_none() {
                            if let Some(reply) = reply {
                                let _ = reply.send(InputOutcome::WriteFailed);
                            }
                            return;
                        }
                        lease
                    }
                    Some(fence @ crate::session_timeline::SupervisorInputDelivery::Scoped(_)) => {
                        if fence.check(&self.ctx, crate::metrics::now_us()).is_err() {
                            if let Some(reply) = reply {
                                let _ = reply.send(InputOutcome::WriteFailed);
                            }
                            return;
                        }
                        None
                    }
                };
                let mut outcome = InputOutcome::Ok;
                for event in batch {
                    note_fixture_person(&self.ctx, &event, src);
                    let receipt = crate::app_input::tracked_egress(
                        &self.term,
                        &self.ctx.modes,
                        &self.ctx.sink,
                        &self.ctx.output_echo,
                        &event,
                        crate::input::EgressMode::Backpressured,
                    );
                    let event_outcome = crate::app_input::receipt_outcome(receipt);
                    if event_outcome.failed() {
                        outcome = event_outcome;
                    }
                    if let Some(mark) = mark.as_ref() {
                        mark.completion().finish(receipt);
                    }
                }
                if let Some(reply) = reply {
                    let _ = reply.send(outcome);
                }
            }
            Wake::ReadSessionStatus { session, reply } => {
                let result = if session == self.handle.local_id {
                    crate::session_status::native_control_status(
                        &self.ctx,
                        &self.term,
                        self.handle.master,
                        session,
                    )
                } else {
                    Err("no such fixture session".to_owned())
                };
                let mut observed = status_observation(&result);
                let sent = reply.send(result).is_ok();
                observed
                    .as_object_mut()
                    .expect("fixture status observation")
                    .insert("replySent".into(), aterm_json::json!(sent));
                self.ctx.native_policy.renewal_control_observed(&observed);
            }
            Wake::ReadSessionStatuses { reply } => {
                let result = crate::session_status::native_control_status(
                    &self.ctx,
                    &self.term,
                    self.handle.master,
                    self.handle.local_id,
                )
                .map(|row| {
                    format!(
                        "OK 1\n{} {} {} {row}\n",
                        self.handle.local_id,
                        self.handle.sid.as_str(),
                        self.handle.nonce.to_hex()
                    )
                })
                .unwrap_or_else(|error| format!("ERR {error}\n"));
                let _ = reply.send(result);
            }
            Wake::ReadHarnessPolicy { session, reply } => {
                let result = if session == self.handle.local_id {
                    self.ctx
                        .native_policy
                        .config()
                        .map(|config| {
                            crate::harness_policy_verb::configured_facts(
                                &self.ctx, config, 1, true, true, false,
                            )
                        })
                        .ok_or_else(|| "fixture policy unavailable".to_owned())
                } else {
                    Err("no such fixture session".to_owned())
                };
                let _ = reply.send(result);
            }
            _ => {
                if self.unhandled_wakes.insert(tag.clone()) {
                    eprintln!(
                        "native-renewal-fixture pid={} unhandled={} tag={tag}",
                        std::process::id(),
                        named.unwrap_or("Other")
                    );
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn codex_renewal_fixture_dispatches_real_counted_status_hop() {
    static HOPS: crate::control::control_media::Hops = crate::control::control_media::Hops::new();
    let handle = crate::session_store::test_handle_on(8878, -1);
    let mut pump = Pump {
        ctx: Arc::clone(&handle.ctx),
        term: Arc::clone(&handle.term),
        handle,
        unhandled_wakes: Default::default(),
    };
    let mut queued = None;
    let (answer, _hop) = {
        let mut post = crate::control::control_media::MainPost::injected(
            |wake| {
                queued = Some(wake);
                true
            },
            &HOPS,
        );
        post.ask(|reply| Wake::ReadSessionStatus {
            session: 8878,
            reply,
        })
        .unwrap()
    };
    pump.dispatch(queued.unwrap());
    assert!(answer.recv().unwrap().unwrap().contains("schema=1"));
}

#[cfg(test)]
#[test]
fn codex_renewal_native_status_matches_app_claim_and_counter_fields() {
    let app = crate::App::headless_for_test();
    let pooled = app.pool.get(0).unwrap();
    let full = app.session_status_record(0).unwrap();
    let native =
        crate::session_status::native_control_status(&pooled.ctx, &pooled.term, pooled.master, 0)
            .unwrap();
    let field = |text: &str, key: &str| {
        text.split_whitespace().find_map(|word| {
            word.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value.to_owned())
        })
    };
    for key in [
        "hold",
        "hand",
        "hand_pid",
        "human_ms",
        "human_seq",
        "supervisor",
        "supervisor_proven",
        "supervisor_contract",
        "supervisor_scope",
        "supervisor_revision",
        "gen",
        "seq",
        "hash",
    ] {
        assert_eq!(field(&full, key), field(&native, key), "{key}");
    }
    assert_eq!(field(&native, "observed").as_deref(), Some("false"));
    assert_eq!(field(&native, "phase").as_deref(), Some("unknown"));
}

/// Run an owned real PTY and control host until `<root>/stop` exists. The caller
/// starts this helper process with HOME and ATERM_HARNESS_STATE inside root.
/// Credentials, provider config and account bindings are fixture-owned inputs.
pub fn run_native_renewal_host_fixture(config: NativeRenewalFixture) -> Result<(), String> {
    let env = aterm_agent::harness::cli::Env::from_process()?;
    let home = env.home.as_ref().ok_or("fixture HOME missing")?;
    let pool_root = fixture_pool_root(
        &config.root,
        config.shared_pool_root.as_deref(),
        config.moves_account(),
        config.adopt.is_some(),
    )?;
    if !config.root.is_absolute()
        || !home.starts_with(pool_root)
        || !env.state.starts_with(pool_root)
        || env
            .config
            .as_ref()
            .is_none_or(|path| !path.starts_with(pool_root))
        || !config.cwd.starts_with(&config.root)
        || !config.auth_path.starts_with(pool_root)
        || !config.codex.is_absolute()
        || config
            .successor_auth_path
            .as_ref()
            .is_some_and(|path| !path.starts_with(pool_root))
        || config
            .additional_auth_paths
            .iter()
            .any(|path| !path.starts_with(pool_root))
    {
        return Err("fixture paths must remain in the owned root".into());
    }
    let record = config.root.join("launch-record");
    let line = config.launch_line(&record)?;
    std::fs::create_dir_all(&config.root).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(home.join(".native-renewal-fixture")).map_err(|e| e.to_string())?;
    let roster = env
        .accounts_roster_for(aterm_agent::harness::accounts::Cli::Codex)
        .ok_or("fixture roster path")?;
    let old_home = config.auth_path.parent().ok_or("fixture auth path")?;
    let email = aterm_agent::harness::codex_app_login::read_account_email(old_home)
        .ok_or("fixture native email")?;
    if config.shared_pool_root.is_some() && !roster.is_file() {
        return Err("shared fixture pool requires a preseeded roster".into());
    }
    if roster.exists() && config.adopt.is_none() && config.shared_pool_root.is_none() {
        return Err("fixture requires an unseeded roster".into());
    }
    std::fs::create_dir_all(roster.parent().ok_or("fixture roster parent")?)
        .map_err(|e| e.to_string())?;
    if config.adopt.is_none() && config.shared_pool_root.is_none() {
        let mut roster_text = format!(
            "[[account]]\nlabel='main'\nclass='dedicated'\ndir={}\nemail={}\n",
            aterm_toml::Value::from(old_home.to_str().ok_or("fixture old home")?),
            aterm_toml::Value::from(email.as_str()),
        );
        if config.moves_account() {
            let next = config
                .successor_auth_path
                .as_ref()
                .ok_or("quota fixture successor")?;
            for (index, auth_path) in std::iter::once(next)
                .chain(config.additional_auth_paths.iter())
                .enumerate()
            {
                let home = auth_path.parent().ok_or("quota fixture successor")?;
                let email = aterm_agent::harness::codex_app_login::read_account_email(home)
                    .ok_or("quota fixture successor email")?;
                let label = if index == 0 {
                    "target".into()
                } else {
                    format!("target-{index}")
                };
                roster_text.push_str(&format!(
                    "\n[[account]]\nlabel={label:?}\nclass='dedicated'\ndir={}\nemail={}\n",
                    aterm_toml::Value::from(home.to_str().ok_or("quota fixture successor path")?),
                    aterm_toml::Value::from(email.as_str()),
                ));
            }
            let path = env.config.as_deref().ok_or("quota fixture config")?;
            aterm_uds::atomic_file::write(path, format!(
                "[harness]\naccounts='early'\nrelaunch=true\nresume_limits=true\nhuman_grace_s={}\n",
                config.human_grace_s.unwrap_or(0),
            ).as_bytes(), 0o600).map_err(|e| e.to_string())?;
        }
        aterm_uds::atomic_file::write(&roster, roster_text.as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        if config.moves_account() {
            env.accounts_store_for(aterm_agent::harness::accounts::Cli::Codex)
                .ok_or("quota fixture store")?
                .transact(env.now, &mut None, |state| {
                    state.active = Some("main".into());
                    Ok(Vec::new())
                })?;
        }
    }

    let mut builder = crate::quiet_driver_event_loop_builder::<Wake>();
    builder.with_headless();
    let mut event_loop = builder.build().map_err(|e| e.to_string())?;
    let term = Arc::new(Mutex::new(Terminal::new(40, 140)));
    let mut handle = super::super::control_redraw_conformance::registered_session(1, &term);
    if let Some(adopt) = &config.adopt {
        if adopt.waited_pid != adopt.prior_host_pid
            || matches!(
                adopt.wait_outcome,
                NativeRenewalWaitOutcome::Signaled {
                    signal: 0 | 128..=255
                }
            )
        {
            return Err("fixture guardian wait evidence differs".into());
        }
        if aterm_agent::harness::native_act::Process::observe(adopt.prior_host_pid)
            .is_some_and(|process| process.birth == adopt.prior_host_birth)
        {
            return Err("fixture prior host is still alive".into());
        }
        let sid = SessionId::parse(&adopt.sid).ok_or("fixture invalid retained session")?;
        let nonce = LaunchNonce::from_hex(&adopt.nonce).ok_or("fixture invalid retained nonce")?;
        handle.sid = sid.clone();
        handle.nonce = nonce;
        let ctx = Arc::get_mut(&mut handle.ctx).ok_or("fixture shared too soon")?;
        ctx.self_id = sid;
        ctx.nonce = nonce;
    }
    let sid = handle.sid.as_str().to_owned();
    let socket = config.root.join("control.sock");
    let socket_text = socket.to_str().ok_or("fixture socket path")?.to_owned();
    let names = aterm_types::control_socket::token_names_for_sock("control.sock");
    let token_path = config.root.join(&names[0]);
    let token = if config.adopt.is_some() {
        std::fs::read_to_string(&token_path)
            .map_err(|e| e.to_string())?
            .trim()
            .to_owned()
    } else {
        let token = control_auth::random_token_hex().ok_or("fixture entropy")?;
        aterm_uds::atomic_file::write(&token_path, format!("{token}\n").as_bytes(), 0o600)
            .map_err(|e| e.to_string())?;
        token
    };
    crate::proxy::set_self_sock(&config.root, &socket_text);
    let runtime = config.root.join("runtime");
    let graph_dir = runtime.join("aterm");
    std::fs::create_dir_all(&graph_dir).map_err(|e| e.to_string())?;
    crate::proxy::publish_graph_entry(&graph_dir, &handle.sid, &socket_text, &handle.nonce);
    let mut child_env: Vec<(String, String)> = config
        .env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    child_env.extend([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        (
            "CODEX_AUTH_PATH".into(),
            config.auth_path.to_string_lossy().into_owned(),
        ),
        ("ATERM_SESSION_ID".into(), sid.clone()),
        ("ATERM_PARENT_SESSION_ID".into(), sid.clone()),
        (
            "XDG_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
        ("ATERM_CHILD".into(), "1".into()),
        ("TERM_PROGRAM".into(), "aterm".into()),
    ]);
    // SAFETY: an explicit Rust debug factory owns this shell, PTY and isolated root.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
    let shell_args = ["-f".into(), "-i".into()];
    let (master, shell_pid, shell_identity) = if let Some(adopt) = &config.adopt {
        let identity = aterm_pty::record_adopted_shell(adopt.shell_pid, adopt.master);
        if !identity.verify() {
            return Err("fixture retained PTY identity changed".into());
        }
        (adopt.master, adopt.shell_pid, identity)
    } else {
        let shell = aterm_pty::spawn_shell_with_pid(
            40,
            140,
            &spawn,
            &sandbox,
            &child_env,
            Some("/bin/zsh"),
            Some(&shell_args),
            None,
            None,
            config.cwd.to_str(),
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .map_err(|e| e.to_string())?;
        (
            shell.master,
            shell.pid,
            aterm_pty::record_spawned_shell(shell.pid),
        )
    };
    let pty = FixturePty {
        master,
        shell: shell_identity,
    };
    handle.master = master;
    handle.shell_identity = shell_identity;
    let image =
        aterm_agent::harness::native_act::Image::at(&config.codex).ok_or("fixture codex image")?;
    // The fixture's catalog is grok-alab's row alone, the one its captured image stands for.
    let grok = aterm_agent::harness::contract::row_named("grok-alab").ok_or("grok-alab row")?;
    let policy = crate::native_session_policy::FixturePolicy::new(
        std::slice::from_ref(grok),
        vec![crate::native_session_policy::FixtureImages {
            row: "grok-alab",
            images: vec![image],
        }],
        aterm_agent::supervise::SupervisorConfig {
            accounts: if config.moves_account() {
                aterm_agent::harness::accounts::Mode::Early
            } else {
                aterm_agent::supervise::SupervisorConfig::default().accounts
            },
            continue_policy: config.continue_policy.unwrap_or(false),
            human_grace_s: config.human_grace_s.unwrap_or(0),
            ..Default::default()
        },
        &[aterm_agent::harness::native_act::Constructor::PrivateRestart],
    )
    .map_err(str::to_owned)?
    .with_renewal_fault_root(config.root.clone());
    let policy = if config.moves_account() {
        let root = config.root.join("fixture-ca.der");
        let metadata = std::fs::symlink_metadata(&root).map_err(|_| "fixture TLS root missing")?;
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Err("fixture TLS root invalid".into());
        }
        let der = std::fs::read(root).map_err(|_| "fixture TLS root unreadable")?;
        let policy = if config.lost_login {
            policy.with_login_rotation()
        } else {
            policy
        };
        let policy = if config.weekly_quota {
            policy.with_quota_rotation()
        } else {
            policy
        };
        policy.with_quota_roots(vec![der])
    } else {
        policy
    };
    let ctx = Arc::get_mut(&mut handle.ctx).ok_or("fixture shared too soon")?;
    ctx.sink = Arc::new(aterm_session::sink::SinkWriter::new(master));
    ctx.install_fixture_policy(policy)
        .map_err(|_| "fixture policy")?;
    let ctx = Arc::clone(&handle.ctx);
    let store = crate::session_store::new_store();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle.clone());
    let active = Arc::new(Mutex::new(Some(ActiveSession {
        term: Arc::clone(&term),
        master,
        id: handle.local_id,
        ctx: Arc::clone(&ctx),
    })));
    if config.adopt.is_some() && socket.exists() {
        std::fs::remove_file(&socket).map_err(|e| e.to_string())?;
    }
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&socket)).map_err(|e| e.to_string())?;
    let context = Arc::new(ControlWorkerContext {
        active,
        store,
        proxy: event_loop.create_proxy(),
        account_services: fixture_account_services(),
        token: Arc::new(token),
        sock_dir: config.root.clone(),
        recovery_count_admission: Arc::new(
            crate::keeper_link::RecoveryCountAdmission::from_retained(Vec::new()),
        ),
    });
    let server_stop = Arc::new(AtomicBool::new(false));
    let server_context = Arc::clone(&context);
    let server = spawn_fixture_server(listener, Arc::clone(&server_stop), move |stream| {
        serve_connection_mode(&server_context, stream, true)
    });
    let pty_stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&pty_stop);
    let reader_term = Arc::clone(&term);
    let reader_ctx = Arc::clone(&ctx);
    let output = config.root.join("pty.log");
    let reader = std::thread::spawn(move || {
        use std::io::Write;
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output)?;
        let mut bytes = [0u8; 65536];
        while !reader_stop.load(Ordering::Acquire) {
            // SAFETY: this reader owns the live fixture PTY descriptor and output buffer.
            let n = unsafe { libc::read(master, bytes.as_mut_ptr().cast(), bytes.len()) };
            if n <= 0 {
                break;
            }
            let chunk = &bytes[..n as usize];
            log.write_all(chunk)?;
            log.flush()?;
            let mut term = reader_term.lock().unwrap_or_else(|p| p.into_inner());
            term.process(chunk);
            reader_ctx
                .frame
                .publish_text(Arc::new(crate::frame_memo::ScreenText::of(&term)));
        }
        Ok::<_, io::Error>(())
    });
    let mut serving = Serving {
        server_stop,
        server,
        pty_stop,
        pty,
        runs: Vec::new(),
        reader,
    };
    let mut pump = Pump {
        ctx: Arc::clone(&ctx),
        term,
        handle: handle.clone(),
        unhandled_wakes: Default::default(),
    };
    // Every refusal from here on tears down what serves and ends each run already started.
    let launched = (|| -> Result<(), String> {
        let codex_pids = match &line {
            Some(line) => {
                config.launch.prepare(&record)?;
                type_line(master, line)?;
                config
                    .launch
                    .started(master, shell_pid, &record, &mut serving.runs)?
            }
            // The adopted PTY's run, which the host it adopts from started.
            None => config
                .launch
                .started(master, shell_pid, &record, &mut serving.runs)?,
        };
        let child_pid = match codex_pids.as_slice() {
            [pid] => Some(*pid),
            _ => None,
        };
        if line.is_some() && !codex_pids.is_empty() {
            // This facade launched the actual prompt above, just as a managed worker
            // launch creates its task. The owner Stop uses the production task check.
            crate::task_record::mark_session_task_within(
                &ctx,
                handle.local_id,
                &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
                ctx.fabric.fence_wait(),
            )
            .map_err(|_| "fixture initial task mark fenced")?;
        }
        let host = aterm_agent::harness::native_act::Process::observe(std::process::id())
            .ok_or("fixture host identity unavailable")?;
        if let Some(guardian) = &config.guardian_socket {
            if !guardian.starts_with(&config.root) {
                return Err("fixture guardian outside root".into());
            }
            use std::os::fd::BorrowedFd;
            let stream = CtlStream::connect(guardian).map_err(|e| e.to_string())?;
            let payload =
                aterm_json::to_vec(&aterm_json::json!({"sid":sid,"nonce":handle.nonce.to_hex(),
                "shell_pid":shell_pid,"prior_host_pid":host.pid,"prior_host_birth":host.birth}))
                .map_err(|e| e.to_string())?;
            // SAFETY: the live fixture retains this master throughout sendmsg; SCM_RIGHTS duplicates it.
            let descriptor = unsafe { BorrowedFd::borrow_raw(master) };
            if aterm_uds::fdpass::send_with_fds(&stream, &payload, &[descriptor])
                .map_err(|e| e.to_string())?
                != payload.len()
            {
                return Err("fixture guardian short transfer".into());
            }
        }
        println!(
            "{}",
            aterm_json::json!({"socket":socket_text,"sid":sid,"shell_pid":shell_pid,"child_pid":child_pid,
            "codex_pids":codex_pids,"host_pid":host.pid,"host_birth":host.birth,"nonce":handle.nonce.to_hex(),"root":config.root,"token_path":config.root.join(&names[0])})
        );
        std::io::stdout().flush().map_err(|e| e.to_string())
    })();
    let opts =
        crate::harness_host::window_drive_opts(home.clone(), env.state.clone(), Some(socket_text));
    let live_reader = match launched.and_then(|()| {
        live_snapshot::Reader::start(Arc::clone(&ctx), master, opts, config.root.clone())
    }) {
        Ok(reader) => reader,
        Err(why) => {
            let torn_down = serving.tear_down(None, || {
                event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut pump);
            });
            return Err(match torn_down {
                Ok(()) => why,
                Err(also) => format!("{why}; {also}"),
            });
        }
    };
    let run_result = (|| -> Result<(), String> {
        while !config.root.join("stop").exists() {
            event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut pump);
            let native_observation = config.root.join("observe-native-operation.json");
            if native_observation.exists()
                && !config.root.join("native-operation-observed.json").exists()
            {
                let request: aterm_json::Value = aterm_json::from_slice(
                    &std::fs::read(&native_observation).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let id = request["operation"]
                    .as_str()
                    .ok_or("fixture operation observation ID")?;
                if let Some(observed) = crate::native_act::fixture_observe_operation(&ctx, id) {
                    aterm_uds::atomic_file::write(
                        &config.root.join("native-operation-observed.json"),
                        &aterm_json::to_vec(&observed).map_err(|e| e.to_string())?,
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            if config.root.join("person-gesture").exists()
                && !config.root.join("person-gesture-completed.json").exists()
            {
                let before = ctx.human_input.seq();
                let before_monotonic = aterm_agent::harness::native_act::monotonic_us();
                note_fixture_person(
                    &ctx,
                    &crate::input::InputEvent::KeySequence(vec![b'x']),
                    crate::input::Source::Human,
                );
                let event = aterm_json::json!({"before":before,"after":ctx.human_input.seq(),
                "beforeMonotonicUs":before_monotonic,"humanAgeMs":ctx.human_input.ms_since(crate::metrics::now_us()),
                "monotonicUs":aterm_agent::harness::native_act::monotonic_us()});
                aterm_uds::atomic_file::write(
                    &config.root.join("person-gesture-completed.json"),
                    &aterm_json::to_vec(&event).map_err(|e| e.to_string())?,
                    0o600,
                )
                .map_err(|e| e.to_string())?;
            }
            if config.root.join("owner-stop").exists()
                && !config.root.join("owner-stop-completed.json").exists()
            {
                let operation = crate::native_act::fixture_active_signalled_operation(&ctx);
                let before = operation
                    .as_deref()
                    .and_then(|id| crate::native_act::fixture_operation_state(&ctx, id, false));
                // An explicit fixture owner gesture, never a control-client signal
                // or a forged native receipt. Use the same person clock and confirmed
                // Stop Worker operation as the UI, on this facade's actual session.
                note_fixture_person(
                    &ctx,
                    &crate::input::InputEvent::KeySequence(vec![b'x']),
                    crate::input::Source::Human,
                );
                let outcome =
                    crate::app_overview::stop_worker_in(master, &ctx, aterm_slog::Actor::Human);
                let after = operation
                    .as_deref()
                    .and_then(|id| crate::native_act::fixture_operation_state(&ctx, id, false));
                let event = match outcome {
                    Ok(reply) => {
                        aterm_json::json!({"ok":true,"reply":reply,"operation":operation,"before":before,"after":after})
                    }
                    Err(refusal) => {
                        aterm_json::json!({"ok":false,"refusal":refusal.words(),"operation":operation,"before":before,"after":after})
                    }
                };
                aterm_uds::atomic_file::write(
                    &config.root.join("owner-stop-completed.json"),
                    &aterm_json::to_vec(&event).map_err(|e| e.to_string())?,
                    0o600,
                )
                .map_err(|e| e.to_string())?;
            }
            if config.root.join("renew").exists()
                && !config.root.join("renewed").exists()
                && let Some(next) = &config.successor_auth_path
            {
                if config.moves_account() {
                    aterm_uds::atomic_file::write(
                        &config.root.join("renewed"),
                        b"quota-probe-open\n",
                        0o600,
                    )
                    .map_err(|e| e.to_string())?;
                    continue;
                }
                let next_home = next.parent().ok_or("fixture successor home")?;
                let identity = aterm_agent::harness::codex_app_login::renewal_identity(old_home)
                    .ok_or("fixture old identity")?;
                if aterm_agent::harness::codex_app_login::renewal_identity(next_home).as_ref()
                    != Some(&identity)
                {
                    return Err("fixture successor identity differs".into());
                }
                let accounts = env
                    .accounts_store_for(aterm_agent::harness::accounts::Cli::Codex)
                    .ok_or("fixture account store")?;
                let _signin = accounts
                    .signin_within()
                    .map_err(|e| e.to_string())?
                    .ok_or("fixture sign-in busy")?;
                accounts.transact_logins(env.now, &mut None, |state, logins| {
                    Ok(aterm_agent::harness::accounts::note_renewed(
                        state,
                        logins,
                        "main",
                        old_home.to_str(),
                        next_home.to_str().ok_or("fixture successor text")?,
                        Some("pro"),
                        env.now,
                    ))
                })?;
                aterm_uds::atomic_file::write(&config.root.join("renewed"), b"published\n", 0o600)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })();
    let torn_down = serving.tear_down(Some(live_reader), || {
        event_loop.pump_app_events(Some(Duration::from_millis(10)), &mut pump);
    });
    run_result?;
    torn_down
}

/// What the host serves with: the control server, the PTY (`P`, whose drop hangs up the shell)
/// with the thread that reads it, and the codex runs its launch started.
struct Serving<P> {
    server_stop: Arc<AtomicBool>,
    server: std::thread::JoinHandle<io::Result<()>>,
    pty_stop: Arc<AtomicBool>,
    pty: P,
    runs: Vec<Run>,
    reader: std::thread::JoinHandle<io::Result<()>>,
}

impl<P> Serving<P> {
    /// End the live reader, then the control server, then each run, then the PTY. Hanging up a
    /// connection does not wake a worker that waits on a main-thread hop (`call_main`, for up to
    /// a minute), so `pump` runs the event loop until the server has joined every worker, and the
    /// PTY is hung up only after that. Nothing pumps once it is gone: dispatch reads and writes its
    /// master, whose number may by then name another file. The runs end before the hang-up, which
    /// does not reach a run in a session of its own ([`end_runs`]), and while the reader still
    /// logs what the shell prints of them.
    ///
    /// # Errors
    /// The live reader failed, or a run outlived its `SIGKILL` ([`end_runs`]).
    ///
    /// # Panics
    /// The live reader or the server outlives its one-minute hang detector.
    fn tear_down(
        self,
        live_reader: Option<live_snapshot::Reader>,
        mut pump: impl FnMut(),
    ) -> Result<(), String> {
        let live = live_reader.map_or(Ok(()), |reader| reader.shutdown(&mut pump));
        self.server_stop.store(true, Ordering::Release);
        let _ = join_pumping(
            self.server,
            "fixture control server shutdown hang detector",
            &mut pump,
        );
        let ended = end_runs(&self.runs);
        self.pty_stop.store(true, Ordering::Release);
        drop(self.pty);
        let _ = self.reader.join();
        live.and(ended)
    }
}

/// Join `thread`, running `pump` until it has ended, under a one-minute hang detector.
fn join_pumping<T>(
    thread: std::thread::JoinHandle<T>,
    hang: &str,
    mut pump: impl FnMut(),
) -> std::thread::Result<T> {
    let until = Instant::now() + MUST_HAPPEN;
    while !thread.is_finished() {
        assert!(Instant::now() < until, "{hang}");
        pump();
    }
    thread.join()
}

/// Accept control connections on `listener` until `stop`, each served by `serve` on its own
/// worker; then hang up every connection still open and join every worker.
fn spawn_fixture_server(
    listener: CtlListener,
    stop: Arc<AtomicBool>,
    serve: impl Fn(CtlStream) -> io::Result<bool> + Send + Sync + 'static,
) -> std::thread::JoinHandle<io::Result<()>> {
    let serve = Arc::new(serve);
    std::thread::spawn(move || {
        let mut workers: Vec<(CtlStream, std::thread::JoinHandle<io::Result<bool>>)> = Vec::new();
        while !stop.load(Ordering::Acquire) {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].1.is_finished() {
                    let (_, worker) = workers.swap_remove(index);
                    let _ = worker.join();
                } else {
                    index += 1;
                }
            }
            match aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Ready,
                aterm_uds::AcceptedMode::Blocking,
            ) {
                Ok((stream, _)) => {
                    let retained = stream.try_clone()?;
                    let serve = Arc::clone(&serve);
                    let worker = std::thread::spawn(move || serve(stream));
                    workers.push((retained, worker));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(e) => return Err(e),
            }
        }
        stop_fixture_connections(workers.iter().map(|(stream, _)| stream));
        for (_, worker) in workers {
            let _ = worker.join();
        }
        Ok(())
    })
}

fn stop_fixture_connections<'a>(connections: impl IntoIterator<Item = &'a CtlStream>) {
    for stream in connections {
        aterm_uds::hangup::hang_up(stream);
    }
}

#[cfg(test)]
#[test]
fn codex_renewal_fixture_stop_closes_kept_open_subscription() {
    use std::io::Read as _;
    let (mut client, mut server) = CtlStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let retained = server.try_clone().unwrap();
    let (finished, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let active = Arc::new(Mutex::new(None));
        let store = crate::session_store::new_store();
        super::run_subscribe_socket(
            aterm_types::control_request::head("subscribe @* sessions"),
            &active,
            &store,
            Scope::Owner,
            &mut server,
        );
        finished.send(()).unwrap();
    });
    let mut initial = [0; b"OK subscribe 0\n".len()];
    client.read_exact(&mut initial).unwrap();
    assert_eq!(&initial, b"OK subscribe 0\n");
    assert!(done.try_recv().is_err(), "subscription remains live");
    stop_fixture_connections(&[retained]);
    done.recv_timeout(Duration::from_secs(60)).unwrap();
    worker.join().unwrap();
    drop(client);
}

fn status_observation(result: &Result<String, String>) -> aterm_json::Value {
    let text = result.as_ref().map(String::as_str).unwrap_or("");
    let field = |key: &str| {
        text.split_whitespace().find_map(|word| {
            word.split_once('=')
                .filter(|(name, _)| *name == key)
                .map(|(_, value)| value)
        })
    };
    let number = |key| field(key).and_then(|value| value.parse::<u64>().ok());
    let holder = field("supervisor").and_then(|value| {
        if value == "-" {
            None
        } else if crate::self_claim::holder_pid(value).is_some() {
            Some(value)
        } else {
            Some("other")
        }
    });
    let scope = field("supervisor_scope").filter(|value| {
        *value == "-"
            || value.split(',').all(|word| {
                aterm_agent::harness::contract::Scope::ALL
                    .into_iter()
                    .any(|scope| scope.word() == word)
            })
    });
    aterm_json::json!({"kind":"session-status","outcome":if result.is_ok(){"ok"}else{"error"},
        "holder":holder,"proven":field("supervisor_proven")==Some("1"),
        "contract":number("supervisor_contract"),"scope":scope,
        "revision":number("supervisor_revision"),"hold":field("hold")==Some("1"),
        "hand":if field("hand")==Some("-"){"none"}else{"present"},
        "handPid":number("hand_pid"),"humanSeq":number("human_seq"),
        "monotonicUs":aterm_agent::harness::native_act::monotonic_us()})
}

#[cfg(test)]
#[test]
fn codex_renewal_completed_status_trace_copies_only_whitelisted_fields() {
    let status = "supervisor=codex-alab@4242 supervisor_proven=1 supervisor_contract=3 \
        supervisor_scope=relaunch,upgrade supervisor_revision=9 hold=0 hand=- hand_pid=- \
        human_seq=7 native_cap=do-not-copy token=do-not-copy command=do-not-copy";
    let observed = status_observation(&Ok(status.into()));
    assert_eq!(observed["holder"].as_str(), Some("codex-alab@4242"));
    assert_eq!(observed["revision"].as_u64(), Some(9));
    assert_eq!(observed["humanSeq"].as_u64(), Some(7));
    assert_eq!(observed["scope"].as_str(), Some("relaunch,upgrade"));
    assert!(!observed.to_string().contains("do-not-copy"));
    let other = status_observation(&Ok(
        "supervisor=arbitrary-private-value supervisor_scope=arbitrary-private-value".into(),
    ));
    assert!(!other.to_string().contains("arbitrary-private-value"));
    let error = status_observation(&Err("private error body".into()));
    assert!(!error.to_string().contains("private error body"));
}
