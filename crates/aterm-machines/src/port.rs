// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Operating-system port ([`System`]), with [`Live`] and test (`Fake`) implementations.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "macos")]
mod accessibility;
pub mod claude_keychain;
mod complete_input;
mod console_session;
mod create_new;
mod github_api;
pub(crate) mod github_rest;
mod identity;
#[cfg(target_os = "macos")]
mod keyboard_maps;
pub(crate) mod keyboard_prefix;
pub use keyboard_prefix::KeyboardPrefix;
pub(crate) mod keyboard_usage;
#[cfg(target_os = "macos")]
mod low_data;
mod processes;
mod read_owned_file;
mod relay;
mod remove_empty_dir;
mod rename_noreplace;
mod scratch;
mod screen_lock;
mod scroll_direction;
mod secret;
#[cfg(test)]
pub(crate) mod spawns;
mod staged_child;
mod stops;
mod tty_wait;

pub use github_rest::RestMethod;
pub use scratch::Scratch;
pub use secret::{SECRET_MAX, Secret, no_core_dumps};
pub use staged_child::{START_BYTE, START_LIMIT, StagedChild};
pub use stops::{name as stop_name, say_on_stop};

/// Default deadline for a single tool run.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

pub(crate) fn deadline_after(budget: Duration) -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(budget)
        .or_else(|| now.checked_add(Duration::from_secs(365 * 24 * 60 * 60)))
        .unwrap_or(now)
}

/// What [`Live`] spawns for one run: the program, its arguments and its environment.
#[cfg(test)]
pub(crate) type Spawned = (PathBuf, Vec<OsString>, Vec<(String, String)>);

/// A complete bounded GitHub read, including the pagination headers.
#[derive(Clone, PartialEq, Eq)]
pub struct GithubResponse {
    /// The HTTP status.
    pub status: u16,
    /// Response headers in received order, preserving duplicates for admission.
    pub headers: Vec<(String, String)>,
    /// The complete response body.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for GithubResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubResponse")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

/// A GitHub read that handed back no answer ([`System::github_api`]): the line a report
/// carries, which never holds the credential, and what kind of failure it was, which a
/// reader words for a person (System State's `init_observe::Wait`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubError {
    /// What kind of failure.
    pub failure: GithubFailure,
    /// One line, with no credential.
    pub why: String,
}

/// What kind of failure a [`GithubError`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GithubFailure {
    /// The request reached no answer: the network, a proxy, the trust store or a deadline
    /// (agh's `CouldNotRun`).
    Unreached,
    /// The token is not one that signs in: GitHub answered 401, or agh found it unusable
    /// (agh's `Unauthenticated`).
    Unauthenticated,
    /// Anything else: a refused route, a GitHub error, an answer outside its bounds.
    Other,
}

impl GithubError {
    /// A failure of no particular kind.
    #[must_use]
    pub fn other(why: impl Into<String>) -> Self {
        Self {
            failure: GithubFailure::Other,
            why: why.into(),
        }
    }
}

impl From<GithubError> for String {
    fn from(error: GithubError) -> Self {
        error.why
    }
}

/// One tool run.
#[derive(Clone, PartialEq, Eq)]
pub struct Cmd {
    /// Absolute path of the program.
    pub program: PathBuf,
    /// Arguments after the program.
    pub args: Vec<OsString>,
    /// Run as this account via `sudo -n -u <u> -H -- /usr/bin/env -i ...`.
    pub user: Option<User>,
    /// Run inside the user's login session (`launchctl asuser <uid>`).
    pub session: bool,
    /// A file to read standard input from; standard input is closed otherwise.
    pub stdin: Option<PathBuf>,
    /// The deadline, counted from the start of the run.
    pub budget: Duration,
    /// Variables added to the fixed environment, in order.
    pub env: Vec<(String, String)>,
    /// Variable names in `env` whose values [`Cmd::display`] masks.
    pub masked: Vec<String>,
    /// Kill the process group at the deadline and return `Ok` with `status: None` and what it
    /// printed, instead of [`RunFailed::TimedOut`].
    pub keep_on_timeout: bool,
    /// The directory the program starts in; `/` when `None`.
    pub cwd: Option<PathBuf>,
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<String> = self
            .env
            .iter()
            .map(|(k, _)| format!("{k}=\u{2026}"))
            .collect();
        f.debug_struct("Cmd")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("user", &self.user)
            .field("session", &self.session)
            .field("stdin", &self.stdin)
            .field("budget", &self.budget)
            .field("env", &env)
            .field("keep_on_timeout", &self.keep_on_timeout)
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl Cmd {
    /// A run of `program` (an absolute path) with no arguments.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            user: None,
            session: false,
            stdin: None,
            budget: DEFAULT_BUDGET,
            env: Vec::new(),
            masked: Vec::new(),
            keep_on_timeout: false,
            cwd: None,
        }
    }

    /// Start the program in `dir` ([`Cmd::cwd`]).
    #[must_use]
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Add `key=value` to the child's environment (the `env` field says how it travels).
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// [`Cmd::env`] for a value [`Cmd::display`] must never show (a token).
    #[must_use]
    pub fn secret_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        self.masked.push(key.clone());
        self.env(key, value)
    }

    /// End the run at its deadline and keep what it printed ([`Cmd::keep_on_timeout`]).
    #[must_use]
    pub const fn until_deadline(mut self) -> Self {
        self.keep_on_timeout = true;
        self
    }

    /// Append one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Run as `user`.
    #[must_use]
    pub fn as_user(mut self, user: &User) -> Self {
        self.user = Some(user.clone());
        self
    }

    /// Run in the user's login session (with [`Cmd::as_user`]).
    #[must_use]
    pub const fn in_session(mut self) -> Self {
        self.session = true;
        self
    }

    /// Read standard input from `path`.
    #[must_use]
    pub fn stdin_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.stdin = Some(path.into());
        self
    }

    /// Bound the run by `budget`.
    #[must_use]
    pub const fn budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// The command as one shell-quoted line.
    #[must_use]
    pub fn display(&self) -> String {
        let mut out = String::new();
        if let Some(dir) = &self.cwd {
            out.push_str("cd ");
            out.push_str(&shell_word(&dir.to_string_lossy()));
            out.push_str(" && ");
        }
        if let Some(user) = &self.user {
            out.push_str(&user.name);
            out.push_str(if self.session { "@gui$ " } else { "$ " });
        }
        for (key, value) in &self.env {
            if self.masked.contains(key) {
                out.push_str(key);
                out.push_str("=\u{2026}");
            } else {
                out.push_str(&shell_word(&format!("{key}={value}")));
            }
            out.push(' ');
        }
        out.push_str(&shell_word(&self.program.to_string_lossy()));
        for a in &self.args {
            out.push(' ');
            out.push_str(&shell_word(&a.to_string_lossy()));
        }
        out
    }
}

/// Quote `word` for a POSIX shell.
#[must_use]
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'=' | b',' | b'+' | b'%'
                )
        });
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// What a finished run produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ran {
    /// The exit status, `None` when a signal ended it.
    pub status: Option<i32>,
    /// Standard output (the last MiB at most).
    pub stdout: Vec<u8>,
    /// Standard error (the last MiB at most).
    pub stderr: Vec<u8>,
}

impl Ran {
    /// A run that exited 0 with `stdout`.
    #[must_use]
    pub fn ok(stdout: &str) -> Self {
        Self {
            status: Some(0),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    /// A run that exited `status` with `stderr`.
    #[must_use]
    pub fn exit(status: i32, stderr: &str) -> Self {
        Self {
            status: Some(status),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    /// Exited 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// Standard output as text (lossy).
    #[must_use]
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// Standard error as text (lossy), trimmed, for a one-line `why`.
    #[must_use]
    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }

    /// One line saying how the run ended, for a `why`: `exit 1: <stderr>`.
    #[must_use]
    pub fn describe(&self) -> String {
        let how = self
            .status
            .map_or_else(|| "killed by a signal".to_owned(), |s| format!("exit {s}"));
        let err = self.err();
        let first = err.lines().next().unwrap_or("");
        if first.is_empty() {
            how
        } else {
            format!("{how}: {first}")
        }
    }
}

/// Why a run produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunFailed {
    /// The program could not be started.
    Spawn(String),
    /// A child was started but no usable result was established. It may have acted;
    /// this cause alone does not establish that cleanup or reaping completed.
    AfterStart(String),
    /// The deadline passed; the process group was killed.
    TimedOut(Duration),
    /// The port refused the run before starting it (such as `as_user` without root).
    Refused(String),
}

impl std::fmt::Display for RunFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(why) => write!(f, "could not start: {why}"),
            Self::AfterStart(why) => write!(f, "failed after start: {why}"),
            Self::TimedOut(d) => write!(f, "no answer within {}s", d.as_secs()),
            Self::Refused(why) => write!(f, "refused: {why}"),
        }
    }
}

impl RunFailed {
    /// Keep a collector's post-start failures distinct from a failure to spawn a child.
    #[must_use]
    pub fn from_child(error: aterm_uds::deadline::RunError, budget: Duration) -> Self {
        use aterm_uds::deadline::RunError;
        match error {
            RunError::Spawn(error) => Self::Spawn(error.to_string()),
            RunError::Capture(error) => {
                Self::AfterStart(format!("output capture setup failed: {error}"))
            }
            RunError::Wait(error) => Self::AfterStart(format!("wait failed: {error}")),
            RunError::TimedOut => Self::TimedOut(budget),
        }
    }

    /// Preserve the dispatch phase when complete stdout could not be established.
    #[must_use]
    pub fn from_complete_output(
        error: aterm_uds::deadline::CompleteOutputError,
        budget: Duration,
    ) -> Self {
        use aterm_uds::deadline::CompleteOutputError;
        match error {
            CompleteOutputError::Run(error) => Self::from_child(error, budget),
            CompleteOutputError::StdoutRead(error) => {
                Self::AfterStart(format!("complete stdout read failed: {error}"))
            }
            CompleteOutputError::StdoutTooLarge { limit } => {
                Self::AfterStart(format!("complete stdout exceeds {limit} bytes"))
            }
            CompleteOutputError::StdoutIncomplete => {
                Self::AfterStart("complete stdout did not reach EOF".into())
            }
        }
    }
}

/// A file's owner, mode and kind, as `lstat` reports them (no link is followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Owner uid.
    pub uid: u32,
    /// Group gid.
    pub gid: u32,
    /// Permission bits (`st_mode & 0o7777`).
    pub mode: u32,
    /// A directory.
    pub dir: bool,
    /// A symbolic link (never followed).
    pub symlink: bool,
    /// Size in bytes.
    pub len: u64,
}

/// One live process ([`System::processes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid (`e_ppid`): [`Process::LAUNCHD`] for a program launchd started (an app
    /// LaunchServices opened, a launch agent or daemon) or one whose parent ended.
    pub ppid: u32,
    /// Whether it has a controlling terminal (`e_tdev` names a device): a terminal's shell and
    /// what it starts, which keep it when launchd adopts them (a tab's shell that outlived its
    /// terminal's restart has [`Process::LAUNCHD`] for its parent). An app LaunchServices opened,
    /// a launch agent and a login item have none.
    pub terminal: bool,
    /// The executable the kernel names for it (`proc_pidpath`), in its on-disk spelling; `None`
    /// where the kernel names none (its file was deleted, or the read was refused).
    pub exe: Option<PathBuf>,
    /// The kernel's short name for it (`p_comm`: the name of the file it exec'd, cut as
    /// [`Process::short_name`] cuts it).
    pub comm: String,
}

impl Process {
    /// launchd's pid: the parent of every program it starts, and of every orphan.
    pub const LAUNCHD: u32 = 1;

    /// The short name the kernel keeps for a process that exec'd a file named `name`: its first
    /// 16 bytes (`MAXCOMLEN`), as [`Process::comm`] holds them.
    #[must_use]
    pub fn short_name(name: &str) -> String {
        let bytes = name.as_bytes();
        String::from_utf8_lossy(bytes.get(..16).unwrap_or(bytes)).into_owned()
    }
}

/// A listening [`System::relay`].
pub trait Relaying {
    /// The port it listens on, on `127.0.0.1`.
    fn port(&self) -> u16;

    /// Stop listening, then `SIGKILL` and reap every active connection run.
    fn close(&mut self) -> Result<(), String>;
}

/// An account's service-restart lock, released when the guard drops.
pub trait ServiceRestartHold {}

impl ServiceRestartHold for aterm_update_core::FileLock {}

/// [`System::clt_mark`]'s default: no record, every read, write and lock refused.
struct NoCltMark;

impl aterm_types::clt_mark::Files for NoCltMark {
    fn read(&self, _path: &Path, _max: usize) -> io::Result<Option<String>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn write(&self, _path: &Path, _text: &str) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn remove(&self, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn lock(&self, _path: &Path) -> io::Result<aterm_types::clt_mark::Held> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// One current path observation for the retained installer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstallNetworkReading {
    /// Changes when the native path observation changes, including away and back.
    pub revision: u64,
    /// The path can carry traffic; this does not promise Apple's service answers.
    pub available: bool,
    /// Low Data Mode currently holds downloads.
    pub constrained: bool,
}

/// A network observer owned by one authorized install, cancelled when dropped.
pub trait InstallNetworkWatch {
    /// Latest observation, or unknown before its first update.
    fn reading(&self) -> Option<InstallNetworkReading>;

    /// Wait for a revision other than the caller's, or the bounded timer. Return time spent.
    fn wait(&mut self, revision: Option<u64>, budget: Duration) -> Duration;
}

/// One exact HID service's checked mapping change. Every mapping for another physical key is
/// preserved, and the native writer refuses a service whose identity or tables changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyboardMapChange {
    /// The service's IORegistryEntryID, rather than its row's order or its device model alone.
    pub registry: u64,
    /// Complete upstream mapping state whose Caps passthrough this change relies on.
    pub before_prefix: KeyboardPrefix,
    /// Native modifier mappings read before this change.
    pub before_native: Vec<(i64, i64)>,
    /// User mappings read before this change.
    pub before_user: Vec<(i64, i64)>,
    /// Native modifier mappings to set, if needed.
    pub native: Option<Vec<(i64, i64)>>,
    /// User mappings to set, if needed.
    pub user: Option<Vec<(i64, i64)>>,
}

/// The operating system, as items see it.
pub trait System: Sync {
    /// Keyboard-capable HID service identities, read without events or child processes.
    fn keyboard_service_ids(&self) -> Result<Vec<u64>, String> {
        Err("native keyboard service census is unavailable".into())
    }
    /// Read one GitHub API endpoint through `agh` as `user` with the admitted development
    /// credential.
    ///
    /// # Errors
    /// The read's failure and its kind ([`GithubFailure`]).
    fn github_api(
        &self,
        _user: &User,
        _token: &Secret,
        _endpoint: &str,
    ) -> Result<GithubResponse, GithubError> {
        Err(GithubError::other(
            "GitHub reads are not implemented by this backend",
        ))
    }

    /// Send one GitHub REST request as the controller account, with `token` in `Authorization`
    /// and an optional JSON `body`, refusing redirects.
    fn github_rest(
        &self,
        _token: &Secret,
        _method: RestMethod,
        _endpoint: &str,
        _body: Option<&[u8]>,
    ) -> Result<GithubResponse, String> {
        Err("GitHub REST requests are not implemented by this backend".into())
    }

    /// Where the controller's agh client sends: `Live` answers `HttpTransport::new(ProcessEnv)`
    /// and `Endpoints::github_com()`. The default fails closed, as `github_api`'s and
    /// `github_rest`'s do: a `ScriptedTransport` with nothing queued, which opens no socket and
    /// answers every round trip as not sent, beside endpoints on 127.0.0.1's port 1, so a test
    /// double that never says where GitHub is sends nothing anywhere. `Fake` answers the
    /// transport `Fake::with_github` gave it (agh-fake's loopback), else this default. A release
    /// build has no test double, and there the method has no default.
    #[cfg(any(test, debug_assertions))]
    fn github_transport(
        &self,
    ) -> (
        std::sync::Arc<dyn agh::wire::Transport>,
        agh::wire::Endpoints,
    ) {
        fail_closed_github()
    }

    /// Where the controller's agh client sends: `Live` answers `HttpTransport::new(ProcessEnv)`
    /// and `Endpoints::github_com()`.
    #[cfg(not(any(test, debug_assertions)))]
    fn github_transport(
        &self,
    ) -> (
        std::sync::Arc<dyn agh::wire::Transport>,
        agh::wire::Endpoints,
    );

    /// Where agh.toml is for the `fleet` profile (machines 5b): `Live` answers
    /// `agh::config::Dirs::from_env()`; `Fake` answers the scratch `Dirs` `Fake::with_agh_dirs`
    /// gave it, else this default.
    /// The default is `None`, so a test double never reads the real agh.toml or token files.
    fn agh_dirs(&self) -> Option<agh::config::Dirs> {
        None
    }

    /// The effect seam the controller's agh client runs on: `Fx::live()` by default; `Fake`
    /// answers the one `Fx::simulated` made with it, so the key writes' 1 s pacing (§4.6.5)
    /// takes no real time and a test reads it in `Fx::slept`.
    fn github_fx(&self) -> std::sync::Arc<agh::Fx> {
        std::sync::Arc::new(agh::Fx::live())
    }

    /// Read the active session's natural scrolling preference via the native getter.
    fn natural_scrolling(&self) -> Result<bool, String> {
        Err("native scroll direction is not implemented by this backend".into())
    }

    /// Set natural scrolling in the current macOS login session via the native setter.
    fn set_natural_scrolling(&self, _natural: bool) -> Result<(), String> {
        Err("native scroll direction is not implemented by this backend".into())
    }

    /// Apply exact HID service changes using an entitlement-free Passive property client.
    /// The writer checks their model and current tables, and never requests a permission.
    fn set_keyboard_maps(
        &self,
        _target_uid: u32,
        _vendor: u32,
        _product: u32,
        _changes: &[KeyboardMapChange],
    ) -> Result<(), String> {
        Err("native keyboard mappings are not implemented by this backend".into())
    }

    /// Allocate an exclusively owned temporary directory under `_parent` (or default temp dir).
    fn scratch_dir(&self, _parent: Option<&Path>, _label: &str) -> io::Result<Scratch<'_>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned scratch is not implemented",
        ))
    }

    /// Run `cmd` to completion or its deadline.
    fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed>;

    /// Run `_cmd` and capture complete bounded stdout (failing on truncation or timeout).
    fn run_complete_stdout(&self, _cmd: &Cmd) -> Result<Ran, RunFailed> {
        Err(RunFailed::Refused(
            "complete stdout is not implemented by this backend".into(),
        ))
    }

    /// Feed `_input` via a private stdin pipe and capture complete bounded stdout.
    fn run_complete_stdout_input(&self, _cmd: &Cmd, _input: &[u8]) -> Result<Ran, RunFailed> {
        Err(RunFailed::Refused(
            "complete stdout with piped input is not implemented by this backend".into(),
        ))
    }

    /// Run `cmd` with inherited stdio and no deadline on the controlling terminal.
    fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed>;

    /// Whether `/dev/tty` can be opened for reading and writing.
    fn has_tty(&self) -> bool {
        false
    }

    /// Whether a person is at this Mac for a person step, read as the step asks: the Messages
    /// and FaceTime sign-outs read it right before their clicks, and again after a wait for an
    /// update's handoff (init-on-by-default design §6.4, §6.7). System State's setup run gives
    /// the window's reading ([`Live::with_presence`]); a run with no reading of its own keeps
    /// what its stage decided, and answers yes.
    fn person_present(&self) -> bool {
        true
    }

    /// The registry of update-boundary guards this port's items hold theirs in
    /// (`aterm_types::update_boundary`): this process's, which an update's handoff reads,
    /// unless a test gives its own.
    fn update_boundary(&self) -> &aterm_types::update_boundary::Boundary {
        aterm_types::update_boundary::Boundary::process()
    }

    /// Run `cmd` with `input` and a trailing newline written to a private stdin pipe.
    fn run_fed(&self, cmd: &Cmd, input: &Secret) -> Result<Ran, RunFailed> {
        let _ = (cmd, input);
        Err(RunFailed::Refused("run_fed: not supported".into()))
    }

    /// Prompt on `/dev/tty` and read one line, disabling echo when `secret` is true. With echo
    /// off, whatever was typed before the prompt is discarded.
    fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
        let _ = (prompt, secret);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// [`System::ask`] with echo off that keeps what was typed before it: a line another program
    /// typed into the terminal ahead of the read (System State's go line for the Mac Setup
    /// tab, typed once the tab landed) is read, never discarded.
    fn ask_kept(&self, prompt: &str) -> io::Result<Option<Secret>> {
        self.ask(prompt, true)
    }

    /// The executable path of every process running now ([`System::processes`]), read without a
    /// child process; `None` where the process table or any listed process's executable cannot be
    /// read.
    fn process_paths(&self) -> Option<Vec<PathBuf>> {
        processes::paths(self.processes())
    }

    /// Every live process the kernel lists, this one included: its pid, the executable the kernel
    /// names for it and its short name, read in this process with no child and with no process's
    /// arguments or environment (`docs/DESIGN-leftover-processes-2026-10-16.md` M29). A zombie,
    /// and a process that exited before its executable was read, are left out. `None` where the
    /// process table cannot be read (and off macOS).
    fn processes(&self) -> Option<Vec<Process>> {
        None
    }

    /// `process`'s argument words, `argv[0]` first (never its environment), read in this process
    /// with no child and only once its one thread has run (`aterm_uds::process::thread_ran`: an
    /// argument read of a program macOS's program check holds can hang, M29); kept only while the
    /// same process holds the pid and the kernel names the executable `process` lists across the
    /// read. `None` where they are not read: its thread has not run or that is unknown, it is
    /// gone, it exec'd, or the read failed (another user's process, unless this one is root). No
    /// read is atomic with an exec: one into a held program after the `thread_ran` read can still
    /// hang the argument read, which nothing bounds (§6.4 of that design), so a caller asks this
    /// of the few processes it must.
    fn process_argv(&self, process: &Process) -> Option<Vec<OsString>> {
        let _ = process;
        None
    }

    /// Read the bytes of `path`.
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Metadata identity of a resolved regular file, without reading its bytes or running it.
    /// This is a change observation, never admission of a program or its contents.
    fn file_version(&self, _path: &Path) -> io::Result<[u8; 32]> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Read this tree's native GNU ELF/libc requirements without starting a child. Other
    /// operating systems have no GNU libc gate. Unknown capability refuses a managed build.
    fn admit_runtime(&self, _root: &Path) -> Result<(), String> {
        Err("native runtime requirements are unavailable".into())
    }

    /// The first `max` bytes of the regular file at `path` (all of it when shorter): a program's
    /// `#!` line without reading the rest of a large executable. Refused where a link lies
    /// anywhere on `path`, so nothing outside the folders it names is read, and where it is no
    /// regular file, never waiting on a FIFO ([`aterm_uds::nofollow::open_reader`]); this default,
    /// a test double's, reads `path` through [`System::read_file`].
    fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let mut bytes = self.read_file(path)?;
        bytes.truncate(max);
        Ok(bytes)
    }

    /// Read the regular file at `path` (at most `max` bytes) after checking it is no symlink, is
    /// owned by `uid`, has no set-id or group/other write bit, and, when `private`, no group or
    /// other access at all.
    fn read_owned_file(
        &self,
        path: &Path,
        uid: u32,
        max: usize,
        private: bool,
    ) -> io::Result<Vec<u8>> {
        let _ = (path, uid, max, private);
        Err(io::ErrorKind::Unsupported.into())
    }

    /// The file I/O of this account's record of aterm's own Command Line Tools ask
    /// ([`aterm_types::clt_mark`]), which System State's window reads and writes through the same
    /// code: this Mac's ([`aterm_types::clt_mark::Live`]) on the live system, a test double's own
    /// files in its world. This default, a double's that keeps no such record, refuses every read,
    /// write and lock, which holds the ask as a record naming every build does.
    fn clt_mark(&self) -> &dyn aterm_types::clt_mark::Files {
        &NoCltMark
    }

    /// Write `bytes` to `path` atomically with `mode`, without following symlinks.
    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;

    /// Write `bytes` to `path` (inside `user`'s home) as `user` with `mode`.
    fn write_user_file(&self, user: &User, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;

    /// Flush directory-entry changes in `path` to durable storage.
    fn durable_dir(&self, path: &Path) -> io::Result<()> {
        let _ = path;
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Order directory-entry changes in `path` before every later write to its volume
    /// ([`aterm_durable::ordered_dir`]): a barrier, not a full flush.
    fn ordered_dir(&self, path: &Path) -> io::Result<()> {
        let _ = path;
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Remove a file or empty directory at `path` (succeeding if already absent).
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Remove only an empty directory, or succeed if absent. An appearing file or
    /// symbolic link is never unlinked to make room for a publication.
    fn remove_empty_dir(&self, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Create `path` and any missing parent directories with `mode`.
    fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()>;

    /// Change owner and group of `path` without following symlinks.
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()>;

    /// Set owner `uid:gid` and permission bits `mode` on the single-linked regular file at
    /// `path` via an open descriptor.
    fn set_owner_and_mode(&self, path: &Path, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
        let _ = (path, uid, gid, mode);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Return `lstat(path)` metadata without following symlinks.
    fn stat(&self, path: &Path) -> io::Result<Stat>;

    /// List entry names in directory `path`.
    fn list_dir(&self, path: &Path) -> io::Result<Vec<String>>;

    /// Compute the lowercase hex SHA-256 of the file at `path` without following symlinks.
    fn sha256(&self, path: &Path) -> io::Result<String>;

    /// Atomically copy `from` to `to` with `mode` without following symlinks at either end.
    fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()>;

    /// Rename `from` to `to`.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Atomically rename `from` to an absent `to`, failing with
    /// [`io::ErrorKind::AlreadyExists`] on collision.
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let _ = (from, to);
        Err(io::ErrorKind::Unsupported.into())
    }

    /// This process's effective uid.
    fn euid(&self) -> u32;

    /// Unix seconds now.
    fn now(&self) -> u64;

    /// Exclusively create `path` with `bytes` and `mode`, failing if anything already exists.
    /// Live prepares and orders an owned sibling file before absent-only publication. After a
    /// successful publication, private staging cleanup is best effort and cannot turn success
    /// into an error; process death can leave that private staging behind.
    fn create_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        let _ = (path, bytes, mode);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Whether `pid` names a live process (a stale lock's holder is dead).
    fn pid_alive(&self, pid: u32) -> bool {
        let _ = pid;
        false
    }

    /// Pause this thread for `period`: a poll's wait, in this process, never a `sleep` child.
    fn sleep(&self, period: Duration) {
        std::thread::sleep(period);
    }

    /// The account this process runs as and the machine it runs on ([`Identity`]), read in
    /// this process.
    ///
    /// # Errors
    /// One line when the account database names no account for this uid, or a reading fails.
    fn identity(&self) -> Result<Identity, String> {
        Err("this system reads no account".into())
    }

    /// Home directory of the account this process runs as, when readable without recording an
    /// identity probe.
    fn account_home(&self) -> Option<PathBuf> {
        self.identity().ok().map(|id| PathBuf::from(id.user.home))
    }

    /// Whether the network this Mac uses now is in Low Data Mode, as macOS's network path
    /// reads it, in this process; `None` when it cannot tell (no network path, or none read).
    fn low_data_mode(&self) -> Option<bool> {
        None
    }

    /// Retain a network observer for an authorized installer. No packet or consent request.
    fn install_network(&self) -> Option<Box<dyn InstallNetworkWatch + '_>> {
        None
    }

    /// One look at the macOS service `restart` names and what it holds, read in this process
    /// without root ([`aterm_uds::services::look`]); unread where this system reads none.
    fn service(&self, restart: &aterm_vitals::services::Restart) -> aterm_uds::services::Look {
        let _ = restart;
        aterm_uds::services::Look::Unread
    }

    /// Exclusively hold an account's restart transaction without waiting behind another
    /// run's password prompt. The rendezvous is retained after the guard drops.
    /// # Errors
    /// The lock cannot be opened, is held, or this backend supports no restart lock.
    fn hold_service_restart(&self, _path: &Path) -> io::Result<Box<dyn ServiceRestartHold>> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Whether this process's account administers the Mac, the accounts `sudo` admits
    /// ([`aterm_uds::account::administrator`]); `None` where unread.
    fn administrator(&self) -> Option<bool> {
        None
    }

    /// Whether the app macOS holds responsible for this process holds Accessibility, read in
    /// this process with no prompt; `None` when it cannot be read here.
    fn accessibility_trusted(&self) -> Option<bool> {
        None
    }

    /// Ask macOS to show its Accessibility consent for the app it holds responsible for this
    /// process (nothing when it holds it), answering the trust as it stands; `None` when it
    /// cannot be asked here.
    fn request_accessibility(&self) -> Option<bool> {
        None
    }

    /// The name of the app macOS holds responsible for this process (aterm in aterm's tab,
    /// Terminal under Terminal): the one a privacy grant this run needs goes to.
    fn responsible_app(&self) -> Option<String> {
        None
    }

    /// Hand the Accessibility ask to the aterm window's permission walk (`aterm ctl permissions
    /// walk now accessibility`), only when that window is the app macOS holds responsible for
    /// this process, so the grant it walks is this run's: what the window's answer says of the
    /// ask ([`AccessibilityWalk`]), [`AccessibilityWalk::NotTaken`] where no such window answers.
    fn walk_accessibility(&self) -> AccessibilityWalk {
        AccessibilityWalk::NotTaken
    }

    /// When `pid` started, in Unix seconds, as the kernel recorded it for any uid's process (what
    /// `ps -o lstart=` prints), or `None` when there is no such process.
    fn process_started(&self, pid: u32) -> Option<u64> {
        let _ = pid;
        None
    }

    /// Spawn a detached child gated on a one-byte stdin start token.
    fn stage_detached(
        &self,
        _cmd: &Cmd,
        _log: &Path,
    ) -> Result<Box<dyn StagedChild + '_>, RunFailed> {
        Err(RunFailed::Refused(
            "staged child is not implemented by this backend".into(),
        ))
    }

    /// Wait up to `_limit` for the parent's one-byte start token on stdin.
    fn await_start(&self, _limit: Duration) -> Result<(), RunFailed> {
        Err(RunFailed::Refused(
            "start gate is not implemented by this backend".into(),
        ))
    }

    /// Start `cmd` detached in its own session (`setsid`) with output appended to `log`.
    fn spawn_detached(&self, cmd: &Cmd, log: &Path) -> Result<u32, RunFailed> {
        let _ = (cmd, log);
        Err(RunFailed::Refused("spawn_detached: not supported".into()))
    }

    /// Create a symbolic link at `link` pointing to `target`.
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let _ = (target, link);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Read the target of the symbolic link at `path`.
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Resolve `_path` to its canonical local filesystem path.
    fn canonicalize(&self, _path: &Path) -> io::Result<PathBuf> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Stream `from` into `path` atomically with `mode`, returning the number of bytes written.
    fn write_stream(&self, path: &Path, from: &mut dyn io::Read, mode: u32) -> io::Result<u64> {
        let mut bytes = Vec::new();
        from.read_to_end(&mut bytes)?;
        self.write_file(path, &bytes, mode)?;
        Ok(bytes.len() as u64)
    }

    /// Acquire an exclusive `flock` on `path` within `limit`, held until the returned guard drops.
    fn exclusive(&self, path: &Path, limit: Duration) -> io::Result<Box<dyn std::any::Any>> {
        let _ = (path, limit);
        Ok(Box::new(()))
    }

    /// Listen on a loopback port and spawn `cmd` per connection with stdio bound to the socket.
    fn relay(&self, cmd: &Cmd) -> Result<Box<dyn Relaying + '_>, RunFailed> {
        let _ = cmd;
        Err(RunFailed::Refused("relay: not supported".into()))
    }

    /// Lock the current login session's screen via `login.framework`.
    fn lock_screen(&self) -> Result<(), String> {
        Err("lock_screen: not supported".into())
    }

    /// Catch `SIGINT`, `SIGTERM`, `SIGHUP` and `SIGQUIT` until the returned handle drops; one the
    /// process ignores already stays ignored.
    fn catch_stops(&self) -> Box<dyn std::any::Any> {
        Box::new(())
    }

    /// Catch the same stops as [`System::catch_stops`], and once the returned handle drops (after
    /// every disposition is back) send the one caught again, so the run finishes its step and
    /// its cleanup, then stops as it would have. A run that must not be cut mid-step and must
    /// not outlive a person's stop holds this.
    fn defer_stops(&self) -> Box<dyn std::any::Any> {
        self.catch_stops()
    }

    /// Return the first signal caught since [`System::catch_stops`] or [`System::defer_stops`]
    /// was armed.
    fn stopped(&self) -> Option<i32> {
        None
    }

    /// Ignore `SIGHUP` and `SIGPIPE` for the process.
    fn ignore_hangups(&self) {}

    /// This Mac's own `IOPlatformUUID` on the live system; `None` in tests.
    fn own_uuid(&self) -> Option<String> {
        None
    }

    /// Whether this process holds Full Disk Access, as whoever runs the items measured it, once
    /// and without a prompt, before the run.
    fn full_disk_access(&self) -> Option<bool> {
        None
    }

    /// The volume holding `path`, with the free space the window's disk watch holds it to
    /// ([`aterm_capacity::reading::Volume`]), or `None` where it cannot be read.
    fn volume(&self, _path: &Path) -> Option<aterm_capacity::reading::Volume> {
        None
    }

    /// The app bundle this process runs from (`/Users//bob/Applications/aterm.app`), canonical,
    /// or `None` outside a bundle or when it cannot be read.
    fn running_app(&self) -> Option<PathBuf> {
        None
    }

    /// The awake clock in milliseconds ([`aterm_uds::process::awake_ms`]), which
    /// [`crate::first_exec`] stamps and ages a started program's folder by.
    fn awake_ms(&self) -> Option<u64> {
        aterm_uds::process::awake_ms()
    }

    /// Open the converge journal for the account this system acts as.
    fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + Send + '_>, String> {
        Err("this system keeps no converge journal".into())
    }

    /// Whether this process came in over a remote login: OpenSSH's `SSH_CONNECTION` or
    /// `SSH_TTY` is set ([`crate::console`]). The default: no.
    fn remote_login(&self) -> bool {
        false
    }

    /// Whether this process's audit session reaches the window server and was not
    /// established over the network (`SessionGetInfo`'s `sessionHasGraphicAccess` without
    /// `sessionIsRemote`); `None` when it cannot be read. The default: `None`, so a backend
    /// that reads no session never counts as the console's.
    fn graphic_session(&self) -> Option<bool> {
        None
    }

    /// Whether a person can answer at this process: standard input is a terminal and its
    /// controlling terminal (`/dev/tty`, where [`System::ask`] reads) opens. Claude Code's
    /// Bash tool has neither. The default: no.
    fn terminal(&self) -> bool {
        false
    }

    /// The coding agent among this process's ancestors, by aterm's one name table
    /// (`aterm_agent_names::agent_of`, what `status program=` names an agent by): its product
    /// name. The default: none.
    fn agent_ancestor(&self) -> Option<&'static str> {
        None
    }

    /// Whether this is macOS, whose workstation `aterm init` sets up; elsewhere (Linux) init runs
    /// the base layer, the vault and the repositories, and each workstation row reads
    /// "macOS only". The default: the platform this was built for.
    fn macos(&self) -> bool {
        cfg!(target_os = "macos")
    }

    /// Read `clients`' rows from the macOS system TCC `access` table (`clients = &[]` checks
    /// that the table opens and reads), in this process with no lock or subprocess.
    fn tcc_access(&self, _clients: &[&str]) -> Result<Vec<crate::items::person::TccRow>, String> {
        Err("this system reads no TCC database".into())
    }
}

pub use crate::model::User;

/// What [`System::identity`] reads: the account, as the account database names it, with
/// whether it is an administrator (gid 80, or a member of the `admin` group), and the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The account.
    pub user: User,
    /// The machine word `uname -m` prints (`arm64`).
    pub arch: String,
    /// macOS's product version (`27.0`).
    pub os_version: String,
}

/// The live operating system.
#[derive(Debug, Clone)]
pub struct Live {
    /// The agent executable, which [`System::write_user_file`] re-runs as the user.
    pub agent_exe: PathBuf,
    /// What [`System::full_disk_access`] answers: the runner's own measurement.
    full_disk_access: Option<bool>,
    /// What [`System::person_present`] reads, where the runner gives a reading of its own
    /// ([`Live::with_presence`]); otherwise yes, as its stage decided.
    presence: Option<fn() -> bool>,
}

impl Live {
    /// The live port, with the running executable as the agent, and Full Disk Access not measured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agent_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/nonexistent")),
            full_disk_access: None,
            presence: None,
        }
    }

    /// This port, reading `present` for [`System::person_present`], as the person step asks:
    /// System State's setup run gives the window's reading of a person at the page.
    #[must_use]
    pub const fn with_presence(mut self, present: fn() -> bool) -> Self {
        self.presence = Some(present);
        self
    }

    /// This port, answering `measured` for [`System::full_disk_access`].
    #[must_use]
    pub const fn with_full_disk_access(mut self, measured: Option<bool>) -> Self {
        self.full_disk_access = measured;
        self
    }

    fn argv(&self, cmd: &Cmd) -> Result<(PathBuf, Vec<OsString>), RunFailed> {
        Self::argv_from(self.euid(), cmd)
    }

    /// Return the program, arguments, and environment [`Live`] would spawn for `cmd`.
    #[cfg(test)]
    pub(crate) fn spawned(&self, cmd: &Cmd) -> Result<Spawned, RunFailed> {
        let (program, args) = self.argv(cmd)?;
        Ok((program, args, self.child_env(cmd)))
    }

    fn argv_from(euid: u32, cmd: &Cmd) -> Result<(PathBuf, Vec<OsString>), RunFailed> {
        let Some(user) = &cmd.user else {
            return Ok((cmd.program.clone(), cmd.args.clone()));
        };
        if euid != 0 {
            if euid == user.uid {
                return Ok((cmd.program.clone(), cmd.args.clone()));
            }
            return Err(RunFailed::Refused(format!(
                "running as {} needs root (this is uid {euid})",
                user.name,
            )));
        }
        let mut args: Vec<OsString> = Vec::new();
        let program = if cmd.session {
            args.extend([
                "asuser".into(),
                user.uid.to_string().into(),
                "/usr/bin/sudo".into(),
            ]);
            PathBuf::from("/bin/launchctl")
        } else {
            PathBuf::from("/usr/bin/sudo")
        };
        args.extend(
            [
                "-n",
                "-u",
                user.name.as_str(),
                "-H",
                "--",
                "/usr/bin/env",
                "-i",
            ]
            .map(OsString::from),
        );
        args.extend(
            account_env(user)
                .iter()
                .chain(&cmd.env)
                .map(|(k, v)| OsString::from(format!("{k}={v}"))),
        );
        args.push(cmd.program.clone().into_os_string());
        args.extend(cmd.args.iter().cloned());
        Ok((program, args))
    }

    fn child_env(&self, cmd: &Cmd) -> Vec<(String, String)> {
        let mut env: Vec<(String, String)> = match &cmd.user {
            Some(user) if self.euid() != 0 => account_env(user).to_vec(),
            _ => [
                ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
                ("LANG", "en_US.UTF-8"),
                ("LC_ALL", "en_US.UTF-8"),
            ]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .to_vec(),
        };
        match &cmd.user {
            None if self.euid() == 0 => env.push(("HOME".into(), "/var/root".into())),
            None => {
                if let Ok(home) = std::env::var("HOME") {
                    env.push(("HOME".into(), home));
                }
            }
            Some(_) => {}
        }
        env.extend(cmd.env.iter().cloned());
        env
    }

    fn run_until_deadline(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let failed = |e: io::Error| RunFailed::Spawn(e.to_string());
        let (program, args) = self.argv(cmd)?;
        let out = staging_file(self, b"").map_err(failed)?;
        let err = staging_file(self, b"").map_err(failed)?;
        let open = |staged: &Staged<'_>| {
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(staged.path())
                .map_err(failed)
        };
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
        let stdin = open_stdin_file(cmd.stdin.as_deref())?;
        child
            .process_group(0)
            .stdin(stdin)
            .stdout(open(&out)?)
            .stderr(open(&err)?);
        let mut child = child.spawn().map_err(failed)?;
        let deadline = deadline_after(cmd.budget);
        let mut exit = aterm_uds::deadline::ExitWait::new(&child);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        end_group(&mut child);
                        break None;
                    }
                    exit.wait(remaining);
                }
                Err(e) => {
                    end_group(&mut child);
                    return Err(RunFailed::AfterStart(format!("wait failed: {e}")));
                }
            }
        };
        Ok(Ran {
            status,
            stdout: kept_tail(out.path()).map_err(|error| {
                RunFailed::AfterStart(format!("stdout capture read failed: {error}"))
            })?,
            stderr: kept_tail(err.path()).map_err(|error| {
                RunFailed::AfterStart(format!("stderr capture read failed: {error}"))
            })?,
        })
    }
}

fn account_env(user: &User) -> [(String, String); 6] {
    let shell = if user.shell.is_empty() {
        "/bin/zsh"
    } else {
        user.shell.as_str()
    };
    [
        ("HOME", user.home.as_str()),
        ("USER", user.name.as_str()),
        ("LOGNAME", user.name.as_str()),
        ("SHELL", shell),
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        ("LANG", "en_US.UTF-8"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
}

/// How long [`end_group`] and a relay's close wait to reap a child they killed: one still
/// there (held in the kernel) is left to its handle's drop, which hands it to the reaper,
/// rather than holding the caller for ever.
const REAP_HANG: Duration = Duration::from_secs(1);

fn end_group(child: &mut aterm_uds::spawnfd::Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // `child` is not yet reaped, so its pid and process group id remain valid.
        let _ = aterm_uds::process::kill_group_until_empty(pid);
    }
    aterm_uds::deadline::kill_within(child, REAP_HANG);
}

fn open_stdin_file(path: Option<&Path>) -> Result<aterm_uds::spawnfd::Stdio, RunFailed> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let Some(path) = path else {
        return Ok(aterm_uds::spawnfd::Stdio::null());
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| RunFailed::Spawn(format!("{}: {e}", path.display())))?;
    let meta = file
        .metadata()
        .map_err(|e| RunFailed::Spawn(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(RunFailed::Spawn(format!(
            "{}: not a regular file",
            path.display()
        )));
    }
    Ok(aterm_uds::spawnfd::Stdio::from(file))
}

fn kept_tail(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let keep = aterm_uds::deadline::OUTPUT_KEEP as u64;
    let len = meta.len();
    let start = len.saturating_sub(keep);
    file.seek(io::SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity(
        usize::try_from(len.saturating_sub(start).min(keep))
            .unwrap_or(0)
            .saturating_add(1),
    );
    file.take(keep).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn fixed_command(
    program: &Path,
    args: &[OsString],
    env: &[(String, String)],
    cwd: Option<&Path>,
) -> aterm_uds::spawnfd::Command {
    let mut child = aterm_uds::spawnfd::command(program);
    child
        .args(args)
        .env_clear()
        .current_dir(cwd.unwrap_or(Path::new("/")));
    for (key, value) in env {
        child.env(key, value);
    }
    child
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

/// The terminal, owned by one question or one attached program at a time ([`Live`]'s
/// [`System::ask`] and [`System::run_tty`]): `aterm init`'s lanes print rows while another lane
/// asks or runs a program at the terminal, and [`terminal_free`] holds those rows back until it
/// is done, so no row lands inside a prompt or a program's output.
static TERMINAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Own the terminal for `work` ([`TERMINAL`]).
pub(crate) fn owning_terminal<T>(work: impl FnOnce() -> T) -> T {
    let _owned = TERMINAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    work()
}

/// The terminal while no question or attached program owns it ([`TERMINAL`]), for one write;
/// `None` while one does.
pub(crate) fn terminal_free() -> Option<std::sync::MutexGuard<'static, ()>> {
    match TERMINAL.try_lock() {
        Ok(free) => Some(free),
        Err(std::sync::TryLockError::Poisoned(free)) => Some(free.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// What the aterm window's permission walk answered a run's Accessibility ask
/// ([`System::walk_accessibility`]), which decides whether the run may raise macOS's own consent
/// dialog: never beside a walk's page, and never past the owner's Not now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccessibilityWalk {
    /// No walk takes it: no aterm window that is this run's answered with a walk (there is
    /// none, or it is headless or older than the walk), or it reads the grant held (`OK
    /// granted`, which it answers only with no walk under way). macOS's own dialog may ask.
    #[default]
    NotTaken,
    /// The walk takes it: its panel beside System Settings, in place of macOS's own dialog.
    Taken,
    /// The owner's Not now holds the page back from the walk (`OK declined`, or
    /// `not-now:accessibility` in `OK walking`): nothing asks for it.
    NotNow,
    /// The window's walk shows other pages (an `OK walking` that names neither the page nor a
    /// Not now on it), or walked within the last minute and may still be up (`OK recent`):
    /// macOS's own dialog would stand beside the walk's page.
    OtherPages,
}

/// What the aterm window's answer to `permissions walk now <page>` says of `page`: `OK walking`
/// names the pages the walk has still to show (leaving out one the window reads held), and a
/// page the owner's Not now holds back as `not-now:<page>`.
#[cfg(any(test, target_os = "macos"))]
fn walk_answer(reply: &str, page: &str) -> AccessibilityWalk {
    let reply = reply.trim_end();
    if reply == "OK declined" {
        return AccessibilityWalk::NotNow;
    }
    if reply == "OK recent" {
        return AccessibilityWalk::OtherPages;
    }
    let Some(walking) = reply.strip_prefix("OK walking") else {
        return AccessibilityWalk::NotTaken;
    };
    let mut words = walking.split_whitespace();
    if words.clone().any(|walked| walked == page) {
        AccessibilityWalk::Taken
    } else if words.any(|walked| walked.strip_prefix("not-now:") == Some(page)) {
        AccessibilityWalk::NotNow
    } else {
        AccessibilityWalk::OtherPages
    }
}

/// The control socket of the running aterm window when that window is the app macOS holds
/// responsible for this process (aterm in aterm's tab), by the pid its socket's name carries
/// (`aterm-<pid>.sock`). macOS charges Full Disk Access and Accessibility to that app, and the
/// window's permission walk turns on its own, so only this window walks the grant this run needs.
/// `None` under another terminal (the grant is that terminal's), where no window answers, where
/// either pid cannot be read, and off macOS.
#[must_use]
pub fn responsible_window() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let responsible = i32::try_from(std::process::id())
            .ok()
            .and_then(aterm_containment::responsible_app)?;
        let sock = aterm_ctl::front_door_instance()?;
        let instance = Path::new(&sock)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(aterm_types::control_socket::instance_pid)?;
        (u32::try_from(responsible.pid).ok() == Some(instance)).then_some(sock)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

impl System for Live {
    fn keyboard_service_ids(&self) -> Result<Vec<u64>, String> {
        #[cfg(target_os = "macos")]
        {
            keyboard_maps::service_ids()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("native keyboard services are available only on macOS".into())
        }
    }
    fn github_api(
        &self,
        user: &User,
        token: &Secret,
        endpoint: &str,
    ) -> Result<GithubResponse, GithubError> {
        if self.euid() != user.uid {
            return Err(GithubError::other(
                "GitHub reads must run as their local user",
            ));
        }
        github_api::read(token, endpoint)
    }

    fn github_rest(
        &self,
        token: &Secret,
        method: RestMethod,
        endpoint: &str,
        body: Option<&[u8]>,
    ) -> Result<GithubResponse, String> {
        github_rest::send(
            &github_rest::live_client(),
            github_rest::API,
            token,
            method,
            endpoint,
            body,
        )
    }

    fn github_transport(
        &self,
    ) -> (
        std::sync::Arc<dyn agh::wire::Transport>,
        agh::wire::Endpoints,
    ) {
        (
            std::sync::Arc::new(agh::wire::http::HttpTransport::new(std::sync::Arc::new(
                agh::identity::ProcessEnv,
            ))),
            agh::wire::Endpoints::github_com(),
        )
    }

    fn agh_dirs(&self) -> Option<agh::config::Dirs> {
        agh::config::Dirs::from_env()
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn natural_scrolling(&self) -> Result<bool, String> {
        scroll_direction::get()
    }

    fn set_natural_scrolling(&self, natural: bool) -> Result<(), String> {
        scroll_direction::set(natural)
    }

    fn set_keyboard_maps(
        &self,
        target_uid: u32,
        vendor: u32,
        product: u32,
        changes: &[KeyboardMapChange],
    ) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            keyboard_maps::set(target_uid, vendor, product, changes)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (target_uid, vendor, product, changes);
            Err("native keyboard mappings are available only on macOS".into())
        }
    }

    fn scratch_dir(&self, parent: Option<&Path>, label: &str) -> io::Result<Scratch<'_>> {
        Scratch::live(parent, label)
    }

    fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        if !cmd.program.is_absolute() {
            return Err(RunFailed::Refused(format!(
                "{} is not an absolute path",
                cmd.program.display()
            )));
        }
        if cmd.keep_on_timeout {
            return self.run_until_deadline(cmd);
        }
        let (program, args) = self.argv(cmd)?;
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
        let stdin = open_stdin_file(cmd.stdin.as_deref())?;
        let deadline = deadline_after(cmd.budget);
        match aterm_uds::deadline::output_until_from(&mut child, stdin, deadline) {
            Ok(out) => Ok(Ran {
                status: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
            }),
            Err(error) => Err(RunFailed::from_child(error, cmd.budget)),
        }
    }

    fn run_complete_stdout(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        if cmd.keep_on_timeout {
            return Err(RunFailed::Refused(
                "complete stdout cannot keep timed-out partial output".into(),
            ));
        }
        if !cmd.program.is_absolute() {
            return Err(RunFailed::Refused(format!(
                "{} is not an absolute path",
                cmd.program.display()
            )));
        }
        let (program, args) = self.argv(cmd)?;
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
        let stdin = open_stdin_file(cmd.stdin.as_deref())?;
        let deadline = deadline_after(cmd.budget);
        let out =
            aterm_uds::deadline::output_until_complete_stdout_from(&mut child, stdin, deadline)
                .map_err(|error| RunFailed::from_complete_output(error, cmd.budget))?;
        Ok(Ran {
            status: out.status.code(),
            stdout: out.stdout,
            stderr: out.stderr,
        })
    }

    fn run_complete_stdout_input(&self, cmd: &Cmd, input: &[u8]) -> Result<Ran, RunFailed> {
        complete_input::run(self, cmd, input)
    }

    fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed> {
        if !cmd.program.is_absolute() {
            return Err(RunFailed::Refused(format!(
                "{} is not an absolute path",
                cmd.program.display()
            )));
        }
        let (program, args) = self.argv(cmd)?;
        let mut child = aterm_uds::spawnfd::command(&program);
        for (key, value) in &cmd.env {
            child.env(key, value);
        }
        if let Some(dir) = &cmd.cwd {
            child.current_dir(dir);
        }
        let status = owning_terminal(|| {
            let mut started = child
                .args(&args)
                .stdin(aterm_uds::spawnfd::Stdio::inherit())
                .stdout(aterm_uds::spawnfd::Stdio::inherit())
                .stderr(aterm_uds::spawnfd::Stdio::inherit())
                .spawn()
                .map_err(|error| RunFailed::Spawn(error.to_string()))?;
            tty_wait::wait(&mut started, || self.stopped())
                .map_err(|error| RunFailed::AfterStart(format!("wait failed: {error}")))
        })?;
        Ok(status.code())
    }

    fn run_fed(&self, cmd: &Cmd, input: &Secret) -> Result<Ran, RunFailed> {
        use std::io::Write as _;
        if !cmd.program.is_absolute() || cmd.keep_on_timeout {
            return Err(RunFailed::Refused(format!(
                "{}: a fed run takes an absolute program and its deadline as a failure",
                cmd.program.display()
            )));
        }
        let (program, args) = self.argv(cmd)?;
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
        let (reader, mut writer) = io::pipe().map_err(|e| RunFailed::Spawn(e.to_string()))?;
        writer
            .write_all(input.bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .map_err(|e| RunFailed::Spawn(format!("the input pipe: {e}")))?;
        drop(writer);
        let deadline = deadline_after(cmd.budget);
        match aterm_uds::deadline::output_until_from(
            &mut child,
            aterm_uds::spawnfd::Stdio::from(reader),
            deadline,
        ) {
            Ok(out) => Ok(Ran {
                status: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
            }),
            Err(error) => Err(RunFailed::from_child(error, cmd.budget)),
        }
    }

    fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
        let read = if secret {
            secret::Read::Hidden
        } else {
            secret::Read::Shown
        };
        owning_terminal(|| secret::ask_tty(prompt, read))
    }

    fn ask_kept(&self, prompt: &str) -> io::Result<Option<Secret>> {
        owning_terminal(|| secret::ask_tty(prompt, secret::Read::HiddenKept))
    }

    fn processes(&self) -> Option<Vec<Process>> {
        processes::processes()
    }

    fn process_argv(&self, process: &Process) -> Option<Vec<OsString>> {
        processes::argv(process)
    }

    fn has_tty(&self) -> bool {
        use std::io::IsTerminal as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/dev/tty")
            .is_ok_and(|tty| tty.is_terminal())
    }

    fn person_present(&self) -> bool {
        self.presence.is_none_or(|present| present())
    }

    fn admit_runtime(&self, root: &Path) -> Result<(), String> {
        aterm_toolchain::gnu_runtime::admit_tree(root)
    }

    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        const MAX_READ_FILE_BYTES: u64 = 64 * 1024 * 1024;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        if meta.len() > MAX_READ_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds 64 MiB read limit",
            ));
        }
        let mut bytes =
            Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
        file.take(MAX_READ_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_READ_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file exceeds 64 MiB read limit",
            ));
        }
        Ok(bytes)
    }

    fn clt_mark(&self) -> &dyn aterm_types::clt_mark::Files {
        &aterm_types::clt_mark::Live
    }

    fn file_version(&self, path: &Path) -> io::Result<[u8; 32]> {
        use aterm_toolchain::stat_digest::{StatDigest, StatKey};
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut digest = StatDigest::new(b"repository helper file");
        digest.entry(b"", &StatKey::of(&metadata));
        Ok(digest.finish().0)
    }

    /// Opened by [`aterm_uds::nofollow::open_reader_with_identity`] (`O_NOFOLLOW_ANY | O_NONBLOCK`,
    /// then `fstat`), so a link on `path` answers `ELOOP` and a FIFO is refused without waiting;
    /// `Unsupported` off macOS.
    fn read_head(&self, path: &Path, max: usize) -> io::Result<Vec<u8>> {
        let (fd, id) = aterm_uds::nofollow::open_reader_with_identity(path)?;
        let cap = usize::try_from(id.size)
            .unwrap_or(max)
            .min(max)
            .saturating_add(1);
        let mut bytes = Vec::with_capacity(cap);
        let mut reader = std::io::Read::take(std::fs::File::from(fd), max as u64);
        std::io::Read::read_to_end(&mut reader, &mut bytes)?;
        Ok(bytes)
    }

    fn read_owned_file(
        &self,
        path: &Path,
        uid: u32,
        max: usize,
        private: bool,
    ) -> io::Result<Vec<u8>> {
        read_owned_file::read(path, uid, max, private)
    }

    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        write_atomic(path, bytes, mode)
    }

    fn write_user_file(&self, user: &User, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        user_file_target(user, path, mode)?;
        if self.euid() != 0 {
            return write_atomic(path, bytes, mode);
        }
        let staged = staging_file(self, bytes)?;
        let cmd = Cmd::new(&self.agent_exe)
            .args(["machines", "agent", "put-file"])
            .arg(path.as_os_str())
            .arg(format!("{mode:o}"))
            .as_user(user)
            .stdin_file(staged.path());
        let ran = self
            .run(&cmd)
            .map_err(|e| io::Error::other(format!("put-file as {}: {e}", user.name)))?;
        if ran.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "put-file as {}: {}",
                user.name,
                ran.describe()
            )))
        }
    }

    fn durable_dir(&self, path: &Path) -> io::Result<()> {
        aterm_durable::durable_dir(path)
    }

    fn ordered_dir(&self, path: &Path) -> io::Result<()> {
        aterm_durable::ordered_dir(path)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
            Ok(m) if m.is_dir() => std::fs::remove_dir(path),
            Ok(_) => std::fs::remove_file(path),
        }
    }

    fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
        remove_empty_dir::remove(path)
    }

    fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(path)
    }

    fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()> {
        std::os::unix::fs::lchown(path, Some(uid), Some(gid))
    }

    fn set_owner_and_mode(&self, path: &Path, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.file_type().is_file() || meta.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is not a regular file with one link ({} links)",
                    path.display(),
                    meta.nlink()
                ),
            ));
        }
        std::os::unix::fs::fchown(&file, Some(uid), Some(gid))?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))
    }

    fn stat(&self, path: &Path) -> io::Result<Stat> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::symlink_metadata(path)?;
        Ok(Stat {
            uid: m.uid(),
            gid: m.gid(),
            mode: m.mode() & 0o7777,
            dir: m.is_dir(),
            symlink: m.file_type().is_symlink(),
            len: m.len(),
        })
    }

    fn list_dir(&self, path: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(path)? {
            names.push(entry?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    }

    fn sha256(&self, path: &Path) -> io::Result<String> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)?;
        if !f.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        let mut hasher = aterm_digest::Sha256::new();
        let mut buf = vec![0_u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
    }

    fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let parent = to
            .parent()
            .ok_or_else(|| io::Error::other(format!("{} has no parent", to.display())))?;
        if to.file_name().is_none() {
            return Err(io::Error::other(format!(
                "{} has no file name",
                to.display()
            )));
        }
        let scratch = Scratch::new_in(self, parent, "copy")?;
        let tmp = scratch.path().join("payload");
        let mut src = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(from)?;
        if !src.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", from.display()),
            ));
        }
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&tmp)?;
        std::io::copy(&mut src, &mut dst)?;
        dst.set_permissions(std::fs::Permissions::from_mode(mode))?;
        aterm_durable::durable(&dst)?;
        drop(dst);
        std::fs::rename(&tmp, to)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_noreplace::rename(from, to)
    }

    fn euid(&self) -> u32 {
        aterm_uds::peer::our_uid()
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }

    fn create_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        create_new::create(path, bytes, mode, aterm_durable::ordered)
    }

    fn pid_alive(&self, pid: u32) -> bool {
        aterm_uds::process::pid_alive(pid)
    }

    fn identity(&self) -> Result<Identity, String> {
        identity::read(self.euid())
    }

    fn service(&self, restart: &aterm_vitals::services::Restart) -> aterm_uds::services::Look {
        aterm_uds::services::look(restart)
    }

    fn hold_service_restart(&self, path: &Path) -> io::Result<Box<dyn ServiceRestartHold>> {
        aterm_update_core::FileLock::acquire_within(path, Duration::ZERO)
            .map(|lock| Box::new(lock) as Box<dyn ServiceRestartHold>)
    }

    fn administrator(&self) -> Option<bool> {
        aterm_uds::account::administrator()
    }

    fn low_data_mode(&self) -> Option<bool> {
        #[cfg(target_os = "macos")]
        {
            low_data::read(low_data::PATIENCE)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn install_network(&self) -> Option<Box<dyn InstallNetworkWatch + '_>> {
        #[cfg(target_os = "macos")]
        {
            low_data::watch().map(|watch| Box::new(watch) as Box<dyn InstallNetworkWatch>)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn accessibility_trusted(&self) -> Option<bool> {
        #[cfg(target_os = "macos")]
        {
            accessibility::trusted()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn request_accessibility(&self) -> Option<bool> {
        #[cfg(target_os = "macos")]
        {
            accessibility::request()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn responsible_app(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            i32::try_from(std::process::id())
                .ok()
                .and_then(aterm_containment::responsible_app)
                .map(|app| app.display_name)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn walk_accessibility(&self) -> AccessibilityWalk {
        #[cfg(target_os = "macos")]
        {
            let Some(sock) = responsible_window() else {
                return AccessibilityWalk::NotTaken;
            };
            aterm_ctl::front_door_send(&sock, "permissions walk now accessibility\n")
                .map_or(AccessibilityWalk::NotTaken, |reply| {
                    walk_answer(&reply, "accessibility")
                })
        }
        #[cfg(not(target_os = "macos"))]
        {
            AccessibilityWalk::NotTaken
        }
    }

    fn process_started(&self, pid: u32) -> Option<u64> {
        aterm_sysprobe::kproc::record(pid).map(|r| r.start_s)
    }

    fn stage_detached(
        &self,
        cmd: &Cmd,
        log: &Path,
    ) -> Result<Box<dyn StagedChild + '_>, RunFailed> {
        staged_child::spawn(self, cmd, log)
    }

    fn await_start(&self, limit: Duration) -> Result<(), RunFailed> {
        staged_child::await_pipe_start(limit)
    }

    fn spawn_detached(&self, cmd: &Cmd, log: &Path) -> Result<u32, RunFailed> {
        use std::os::unix::fs::OpenOptionsExt as _;
        if !cmd.program.is_absolute() {
            return Err(RunFailed::Refused(format!(
                "{} is not an absolute path",
                cmd.program.display()
            )));
        }
        let (program, args) = self.argv(cmd)?;
        let open_log = || {
            let f = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(log)
                .map_err(|e| RunFailed::Spawn(format!("{}: {e}", log.display())))?;
            if !f.metadata().is_ok_and(|m| m.is_file()) {
                return Err(RunFailed::Spawn(format!(
                    "{}: not a regular file",
                    log.display()
                )));
            }
            Ok(f)
        };
        let mut child = aterm_uds::spawnfd::command(&program);
        child
            .args(&args)
            .env_clear()
            .current_dir(cmd.cwd.as_deref().unwrap_or(Path::new("/")))
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::from(open_log()?))
            .stderr(aterm_uds::spawnfd::Stdio::from(open_log()?));
        for (key, value) in self.child_env(cmd) {
            child.env(key, value);
        }
        let started = child.spawn().map_err(|e| RunFailed::Spawn(e.to_string()))?;
        Ok(started.id())
    }

    fn relay(&self, cmd: &Cmd) -> Result<Box<dyn Relaying + '_>, RunFailed> {
        if !cmd.program.is_absolute() {
            return Err(RunFailed::Refused(format!(
                "{} is not an absolute path",
                cmd.program.display()
            )));
        }
        Ok(Box::new(relay::listen(cmd, self.child_env(cmd))?))
    }

    fn lock_screen(&self) -> Result<(), String> {
        screen_lock::lock_screen_immediate()
    }

    fn catch_stops(&self) -> Box<dyn std::any::Any> {
        stops::catch()
    }

    fn defer_stops(&self) -> Box<dyn std::any::Any> {
        stops::defer()
    }

    fn stopped(&self) -> Option<i32> {
        stops::caught()
    }

    fn ignore_hangups(&self) {
        stops::ignore_hangups();
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::read_link(path)
    }

    fn write_stream(&self, path: &Path, from: &mut dyn io::Read, mode: u32) -> io::Result<u64> {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other(format!("{} has no parent", path.display())))?;
        if path.file_name().is_none() {
            return Err(io::Error::other(format!(
                "{} has no file name",
                path.display()
            )));
        }
        let scratch = Scratch::new_in(self, parent, "stream")?;
        let tmp = scratch.path().join("payload");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&tmp)?;
        let copied = std::io::copy(from, &mut f)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        aterm_durable::durable(&f)?;
        drop(f);
        std::fs::rename(&tmp, path)?;
        Ok(copied)
    }

    fn exclusive(&self, path: &Path, limit: Duration) -> io::Result<Box<dyn std::any::Any>> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
        })?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        Ok(Box::new(Exclusive::within(file, limit, path)?))
    }

    fn full_disk_access(&self) -> Option<bool> {
        self.full_disk_access
    }

    fn tcc_access(&self, clients: &[&str]) -> Result<Vec<crate::items::person::TccRow>, String> {
        crate::items::person::read_tcc_file(Path::new(crate::items::person::TCC_DB), clients)
    }

    fn volume(&self, path: &Path) -> Option<aterm_capacity::reading::Volume> {
        aterm_capacity::reading::Reading::now(path).volume
    }

    fn running_app(&self) -> Option<PathBuf> {
        let exe = self.canonicalize(&self.agent_exe).ok()?;
        exe.ancestors()
            .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
            .map(Path::to_path_buf)
    }

    fn own_uuid(&self) -> Option<String> {
        let ran = self
            .run(&Cmd::new("/usr/sbin/ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]))
            .ok()?;
        crate::hardware::ioreg_value(&ran.out(), "IOPlatformUUID")
    }

    fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + Send + '_>, String> {
        aterm_types::dirs::account_state_dir()
            .map(|root| {
                Box::new(aterm_converge::FileJournal::under(&root))
                    as Box<dyn aterm_converge::Journal + Send>
            })
            .ok_or_else(|| "this account has no state directory to journal changes in".into())
    }

    fn remote_login(&self) -> bool {
        console_session::remote_login()
    }

    fn graphic_session(&self) -> Option<bool> {
        console_session::graphic_session()
    }

    fn terminal(&self) -> bool {
        console_session::terminal()
    }

    fn agent_ancestor(&self) -> Option<&'static str> {
        console_session::agent_ancestor()
    }
}

struct Exclusive(std::fs::File);

impl Exclusive {
    fn within(file: std::fs::File, limit: Duration, what: &Path) -> io::Result<Self> {
        let file: std::fs::File = file;
        aterm_uds::keep_out_of_children(&file)?;
        let deadline = deadline_after(limit);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "{} was held for more than {}s",
                            what.display(),
                            limit.as_secs()
                        ),
                    ));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
    }
}

impl Drop for Exclusive {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A temporary file whose exclusive parent is held until the command has finished.
pub struct Staged<'s> {
    path: PathBuf,
    _scratch: Scratch<'s>,
}

impl Staged<'_> {
    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Refuse a [`System::write_user_file`] of `path` as `user` with `mode` unless `path` is an
/// absolute path with no `..` strictly inside `user`'s home (itself absolute, with no `..`, and
/// not `/`) and `mode` holds permission bits alone (no setuid, setgid or sticky bit). A
/// symbolic link along the way is the user's own: a dotfile manager links `~/.config`.
fn user_file_target(user: &User, path: &Path, mode: u32) -> io::Result<()> {
    let plain = |p: &Path| {
        p.is_absolute()
            && p.components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
    };
    let home = Path::new(&user.home);
    if !plain(home)
        || home.parent().is_none()
        || !plain(path)
        || !path.starts_with(home)
        || path == home
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is not a plain path inside {}'s home",
                path.display(),
                user.name
            ),
        ));
    }
    if mode & !0o777 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("mode {mode:o} sets more than the permission bits (0777)"),
        ));
    }
    Ok(())
}

/// Write `bytes` to a fresh `0600` file inside an exclusively owned temporary directory.
pub fn staging_file<'s>(sys: &'s dyn System, bytes: &[u8]) -> io::Result<Staged<'s>> {
    let scratch = Scratch::new(sys, "stdin")?;
    let path = scratch.path().join("payload");
    sys.create_new(&path, bytes, 0o600)?;
    Ok(Staged {
        path,
        _scratch: scratch,
    })
}

/// Atomically write `bytes` to `path` with `mode` via a sibling temporary file and durable rename.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} has no parent", path.display())))?;
    if path.file_name().is_none() {
        return Err(io::Error::other(format!(
            "{} has no file name",
            path.display()
        )));
    }
    let scratch = Scratch::live(Some(parent), "write")?;
    let tmp = scratch.path().join("payload");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    aterm_durable::durable(&f)?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    aterm_durable::durable_dir(parent)
}

/// The GitHub a test double that names none sends to: a `ScriptedTransport` with nothing
/// queued, which opens no socket and answers every round trip as not sent, beside endpoints
/// on 127.0.0.1's port 1 (`System::github_transport`'s default, and `Fake`'s without
/// `Fake::with_github`).
#[cfg(any(test, debug_assertions))]
fn fail_closed_github() -> (
    std::sync::Arc<dyn agh::wire::Transport>,
    agh::wire::Endpoints,
) {
    (
        std::sync::Arc::new(agh::wire::scripted::ScriptedTransport::new()),
        agh::wire::Endpoints::loopback(1, 1, 1, 1),
    )
}

#[cfg(any(test, debug_assertions))]
pub use fake::Fake;

#[cfg(any(test, debug_assertions))]
mod fake {
    use super::scratch::{OwnedDirectory, prefix};
    use super::{
        Cmd, GithubError, GithubFailure, GithubResponse, Ran, Relaying, RunFailed, Scratch, Secret,
        StagedChild, Stat, System, User,
    };
    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    type Model = Arc<dyn Fn(&Fake, &Cmd) -> Option<Ran> + Send + Sync>;

    /// Scripted [`System`] implementation for unit tests.
    #[derive(Default)]
    pub struct Fake {
        /// The tools a test models instead of scripting ([`Fake::simulate`]).
        model: Mutex<Option<Model>>,
        scroll_natural: Mutex<Option<bool>>,
        scroll_set_error: Mutex<Option<String>>,
        scroll_set_readback: Mutex<Option<bool>>,
        replies: Mutex<BTreeMap<String, Vec<Ran>>>,
        github_replies: Mutex<BTreeMap<String, Vec<Result<GithubResponse, GithubError>>>>,
        outputs: Mutex<BTreeMap<String, Vec<Output>>>,
        strict_refusals: Mutex<BTreeMap<String, String>>,
        /// The commands whose [`System::run`] gives no answer within its budget
        /// ([`Fake::time_out`]).
        timeouts: Mutex<std::collections::BTreeSet<String>>,
        stage_commit_refused: Mutex<bool>,
        stage_abort_refused: Mutex<bool>,
        start_answers: Mutex<std::collections::VecDeque<Result<(), RunFailed>>>,
        scratch_sequence: Mutex<u64>,
        scratch_close_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        /// Shared with [`System::catch_stops`]'s handle, which records its end.
        calls: Arc<Mutex<Vec<String>>>,
        /// The [`System::rename_noreplace`] destinations renamed onto while a clone's publish
        /// held the update boundary ([`Fake::guarded_renames`]).
        guarded_renames: Mutex<Vec<PathBuf>>,
        files: Mutex<BTreeMap<PathBuf, (Vec<u8>, u32)>>,
        runtime_refusals: Mutex<BTreeMap<PathBuf, String>>,
        stats: Mutex<BTreeMap<PathBuf, Stat>>,
        stat_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        stat_errors_once: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        rename_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        list_errors_once: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        directory_flush_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        links: Mutex<BTreeMap<PathBuf, PathBuf>>,
        alive: Mutex<Vec<u32>>,
        /// Each pid's start second ([`Fake::born`]).
        born: Mutex<BTreeMap<u32, u64>>,
        /// The effective uid the fake reports (0 = root, the agent's case).
        pub euid: u32,
        /// The clock.
        pub now: u64,
        /// The pid [`System::spawn_detached`] answers.
        pub next_pid: u32,
        /// The port [`System::relay`] answers; 0 fails it.
        pub relay_port: u16,
        /// Simulated [`System::lock_screen`] error (`None` succeeds).
        pub lock_refused: Option<String>,
        /// Number of `None` readings from [`System::stopped`] before returning `SIGINT`.
        pub stop_after: Option<usize>,
        /// The [`System::stopped`] readings so far (a deferral's end reads once more).
        stop_readings: Arc<Mutex<usize>>,
        requested_stop: Arc<Mutex<Option<i32>>>,
        /// What [`System::ask`] answers, in order ([`Fake::answer`]).
        answers: Mutex<std::collections::VecDeque<Option<String>>>,
        /// Each [`System::run_fed`] run's key and the bytes it was fed ([`Fake::fed`]).
        fed: Mutex<Vec<(String, Vec<u8>)>>,
        /// What [`System::has_tty`] answers: a person's terminal is there.
        pub tty: bool,
        /// Nobody is at this Mac: what [`System::person_present`] answers is its opposite, read
        /// as the step asks, so a test can send the person away while a step waits.
        pub away: std::sync::atomic::AtomicBool,
        /// This fake's own registry of update-boundary guards ([`System::update_boundary`]),
        /// so a test that latches a handoff holds up no other test's items.
        pub boundary: aterm_types::update_boundary::Boundary,
        /// What [`System::full_disk_access`] answers: `None`, not measured, unless a test
        /// grants it.
        pub full_disk_access: Option<bool>,
        /// What [`System::running_app`] answers: `None` unless a test names a bundle.
        pub running_app: Option<PathBuf>,
        /// What [`System::volume`] answers for every path: `None`, unread, unless a test sets
        /// it.
        pub volume: Option<aterm_capacity::reading::Volume>,
        /// What [`System::awake_ms`] answers ([`Fake::awake_at`]): `None`, no awake clock,
        /// unless a test sets it.
        awake: Mutex<Option<u64>>,
        /// What [`System::remote_login`] answers ([`Fake::root`]: no).
        pub remote_login: bool,
        /// What [`System::graphic_session`] answers ([`Fake::root`]: the console's).
        pub graphic_session: Option<bool>,
        /// What [`System::terminal`] answers ([`Fake::root`]: a person at it).
        pub terminal: bool,
        /// What [`System::agent_ancestor`] answers ([`Fake::root`]: none).
        pub agent: Option<&'static str>,
        /// What [`System::identity`] answers ([`Fake::logged_in`]): none unless a test names
        /// the account.
        identity: Mutex<Option<super::Identity>>,
        /// What a [`System::sleep`] does besides recording itself ([`Fake::on_sleep`]).
        on_sleep: Mutex<Option<SleepHook>>,
        on_owned_read: Mutex<Option<OwnedReadHook>>,
        /// What [`System::low_data_mode`] answers: unknown unless a test sets it.
        pub low_data: Option<bool>,
        install_path: Mutex<Option<super::InstallNetworkReading>>,
        /// What [`System::service`] answers, one look each, the last repeating
        /// ([`Fake::services`]): none running when none is scripted.
        services: Mutex<Vec<aterm_uds::services::Look>>,
        /// A real scratch rendezvous for competing restart tests; inert unless set.
        pub service_restart_lock: Option<PathBuf>,
        /// A hook at the terminal-command seam, without executing the command.
        tty_hook: Mutex<Option<Model>>,
        /// What [`System::administrator`] answers: unread unless a test sets it.
        pub admin: Option<bool>,
        /// What [`System::processes`] (and so [`System::process_paths`]) answers
        /// ([`Fake::running`], [`Fake::process`]): unread unless a test lists them.
        processes: Mutex<Option<Vec<super::Process>>>,
        /// What [`System::process_argv`] answers, by pid ([`Fake::process`]): unread when none.
        argvs: Mutex<BTreeMap<u32, Vec<std::ffi::OsString>>>,
        /// What [`System::accessibility_trusted`] and [`System::request_accessibility`]
        /// answer, one reading each, the last repeating ([`Fake::trusts`]): unknown when none.
        trust: Mutex<Vec<bool>>,
        /// What [`System::responsible_app`] answers.
        pub responsible: Option<String>,
        /// What [`System::walk_accessibility`] answers: no walk takes it unless a test says.
        pub walks: super::AccessibilityWalk,
        /// [`System::macos`] answers no: a Linux machine ([`Fake::root`]: a Mac).
        pub linux: bool,
        /// Scripted [`System::tcc_access`] replies keyed by `tcc open` or `tcc <clients>`.
        tcc_replies: Mutex<TccReplies>,
        /// What [`System::github_transport`] answers ([`Fake::with_github`]): unset, the
        /// trait's fail-closed default.
        github: Option<(Arc<dyn agh::wire::Transport>, agh::wire::Endpoints)>,
        /// What [`System::agh_dirs`] answers ([`Fake::with_agh_dirs`]): `None` unless set.
        agh_dirs: Option<agh::config::Dirs>,
        /// What [`System::github_fx`] answers, on every call.
        github_fx: GithubFx,
        /// Exact native keyboard writes requested by a test; no HID services are touched.
        keyboard_changes: Mutex<Vec<super::KeyboardMapChange>>,
    }

    /// The one simulated effect seam a [`Fake`] hands its agh client, made with the `Fake`, so
    /// a test reads every sleep in `Fx::slept` and no `Fake` sleeps for real.
    struct GithubFx(Arc<agh::Fx>);

    impl Default for GithubFx {
        fn default() -> Self {
            Self(Arc::new(agh::Fx::simulated(1_760_000_000)))
        }
    }

    type TccReplies =
        std::collections::BTreeMap<String, Vec<Result<Vec<crate::items::person::TccRow>, String>>>;

    type SleepHook = Arc<dyn Fn(&Fake, std::time::Duration) + Send + Sync>;
    type OwnedReadHook = Arc<dyn Fn(&Fake, &Path) + Send + Sync>;

    #[derive(Clone)]
    enum Output {
        File(PathBuf, Vec<u8>),
        Stat(PathBuf, Stat),
        Running(Vec<String>),
    }

    struct Journaled<'a>(&'a Fake);

    impl aterm_converge::Journal for Journaled<'_> {
        fn before_change(
            &mut self,
            id: &str,
            from: &str,
            to: &str,
            revert: Option<&str>,
        ) -> Result<(), String> {
            let line = aterm_json::json!({
                "time": self.0.now, "id": id, "from": from, "to": to, "revert": revert,
            });
            let mut line = aterm_json::to_string(&line).map_err(|e| e.to_string())?;
            line.push('\n');
            let mut files = self.0.files.lock().unwrap();
            let (bytes, _) = files
                .entry(PathBuf::from(Fake::JOURNAL))
                .or_insert_with(|| (Vec::new(), 0o600));
            bytes.extend_from_slice(line.as_bytes());
            Ok(())
        }
    }

    struct FakeDirectory<'s> {
        sys: &'s Fake,
        path: Option<PathBuf>,
    }

    impl OwnedDirectory for FakeDirectory<'_> {
        fn path(&self) -> &Path {
            self.path.as_deref().unwrap_or_else(|| Path::new(""))
        }
        fn close(&mut self) -> io::Result<()> {
            let Some(path) = &self.path else {
                return Ok(());
            };
            if let Some(kind) = self.sys.scratch_close_errors.lock().unwrap().get(path) {
                return Err(io::Error::from(*kind));
            }
            self.sys
                .files
                .lock()
                .unwrap()
                .retain(|p, _| !p.starts_with(path));
            self.sys
                .stats
                .lock()
                .unwrap()
                .retain(|p, _| !p.starts_with(path));
            self.sys
                .links
                .lock()
                .unwrap()
                .retain(|p, _| !p.starts_with(path));
            self.sys
                .calls
                .lock()
                .unwrap()
                .push(format!("scratch closed {}", path.display()));
            self.path = None;
            Ok(())
        }
        fn keep(mut self: Box<Self>) -> PathBuf {
            let path = self.path.take().unwrap_or_default();
            self.sys
                .calls
                .lock()
                .unwrap()
                .push(format!("scratch kept {}", path.display()));
            path
        }
    }

    impl Drop for FakeDirectory<'_> {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }

    struct FakeStaged<'s> {
        sys: &'s Fake,
        pid: u32,
        committed: bool,
        aborted: bool,
        abort_attempted: bool,
    }

    impl StagedChild for FakeStaged<'_> {
        fn id(&self) -> u32 {
            self.pid
        }
        fn commit(&mut self) -> Result<(), RunFailed> {
            if self.committed {
                return Ok(());
            }
            if self.abort_attempted {
                return Err(RunFailed::Refused(
                    "staged child cancellation has begun".into(),
                ));
            }
            if *self.sys.stage_commit_refused.lock().unwrap() {
                return Err(RunFailed::Refused(
                    "synthetic start-byte write refused".into(),
                ));
            }
            self.committed = true;
            self.sys
                .calls
                .lock()
                .unwrap()
                .push(format!("stage committed {}", self.pid));
            Ok(())
        }
        fn abort(&mut self) -> Result<(), RunFailed> {
            if self.committed {
                return Err(RunFailed::Refused(
                    "committed child is no longer cancellation-owned".into(),
                ));
            }
            if self.aborted {
                return Ok(());
            }
            self.abort_attempted = true;
            if *self.sys.stage_abort_refused.lock().unwrap() {
                self.sys
                    .calls
                    .lock()
                    .unwrap()
                    .push(format!("stage abort unproven {}", self.pid));
                return Err(RunFailed::Refused(
                    "synthetic child reap is unproven; keep its lock".into(),
                ));
            }
            self.aborted = true;
            self.sys
                .calls
                .lock()
                .unwrap()
                .push(format!("stage aborted {}", self.pid));
            Ok(())
        }
    }

    impl Drop for FakeStaged<'_> {
        fn drop(&mut self) {
            if self.committed || self.aborted {
                return;
            }
            if !self.abort_attempted {
                let _ = self.abort();
            }
            if !self.aborted {
                self.sys
                    .calls
                    .lock()
                    .unwrap()
                    .push(format!("stage reaper custody {}", self.pid));
            }
        }
    }

    impl Fake {
        /// Keep a managed build's native runtime unavailable, without starting a child.
        pub fn runtime_unready(&self, root: &Path, why: &str) -> &Self {
            self.runtime_refusals
                .lock()
                .unwrap()
                .insert(root.to_owned(), why.into());
            self
        }

        /// A newly compatible tree has no remaining native runtime blocker.
        pub fn runtime_ready(&self, root: &Path) -> &Self {
            self.runtime_refusals.lock().unwrap().remove(root);
            self
        }

        /// Interleave a filesystem change after an admitted read snapshot was copied.
        pub fn on_owned_read(&self, hook: impl Fn(&Fake, &Path) + Send + Sync + 'static) -> &Self {
            *self.on_owned_read.lock().unwrap() = Some(Arc::new(hook));
            self
        }

        /// Model an explicit Stop at an event boundary, without counting internal reads.
        pub fn stop_now(&self) -> &Self {
            *self.requested_stop.lock().unwrap() = Some(libc::SIGINT);
            self
        }

        /// Publish a network update to a retained install's observer.
        pub fn install_path(&self, reading: super::InstallNetworkReading) -> &Self {
            *self.install_path.lock().unwrap() = Some(reading);
            self
        }

        /// Script the native session's current scroll direction without touching HID.
        pub fn scroll_state(&self, natural: bool) -> &Self {
            *self.scroll_natural.lock().unwrap() = Some(natural);
            self
        }

        /// Make the native setter refuse before changing the scripted state.
        pub fn refuse_scroll_set(&self, why: &str) -> &Self {
            *self.scroll_set_error.lock().unwrap() = Some(why.into());
            self
        }

        /// The state a synthetic setter leaves, to exercise failed read-back.
        pub fn scroll_readback_after_set(&self, natural: bool) -> &Self {
            *self.scroll_set_readback.lock().unwrap() = Some(natural);
            self
        }

        /// Path of the fake converge journal in [`Fake`]'s virtual filesystem.
        pub const JOURNAL: &'static str =
            "/var/root/Library/Application Support/aterm/converge/journal.ndjson";

        /// A fake running as root at a fixed time, in a local graphic session at a terminal
        /// ([`crate::console::presence`] reads a person there once the console is its uid's).
        #[must_use]
        pub fn root() -> Self {
            Self {
                now: 1_800_000_000,
                graphic_session: Some(true),
                terminal: true,
                ..Self::default()
            }
        }

        /// Return the next deterministic scratch path without allocating it.
        #[must_use]
        pub fn scratch_path(parent: Option<&Path>, label: &str, sequence: u64) -> PathBuf {
            parent.unwrap_or_else(|| Path::new("/tmp")).join(format!(
                "{}{sequence}",
                prefix(label).expect("fixture scratch label")
            ))
        }

        /// Script a file write side effect when `key` runs.
        pub fn on_write(&self, key: &str, path: &Path, bytes: &[u8]) -> &Self {
            self.outputs
                .lock()
                .unwrap()
                .entry(key.to_owned())
                .or_default()
                .push(Output::File(path.to_owned(), bytes.to_vec()));
            self
        }

        /// A directory/file metadata observation created by a scripted tool.
        pub fn on_stat(&self, key: &str, path: &Path, stat: Stat) -> &Self {
            self.outputs
                .lock()
                .unwrap()
                .entry(key.to_owned())
                .or_default()
                .push(Output::Stat(path.to_owned(), stat));
            self
        }

        /// The processes running once `key` ran, as [`Fake::running`] lists them: a program
        /// that started (or ended) while the scripted tool ran.
        pub fn on_running(&self, key: &str, exes: &[&str]) -> &Self {
            self.outputs
                .lock()
                .unwrap()
                .entry(key.to_owned())
                .or_default()
                .push(Output::Running(
                    exes.iter().map(|exe| (*exe).to_owned()).collect(),
                ));
            self
        }

        /// Control a cleanup failure without touching any real filesystem.
        pub fn scratch_close_fails(&self, path: &Path, kind: Option<io::ErrorKind>) -> &Self {
            let mut errors = self.scratch_close_errors.lock().unwrap();
            if let Some(kind) = kind {
                errors.insert(path.to_owned(), kind);
            } else {
                errors.remove(path);
            }
            self
        }

        /// Refuse only the complete-record lane, leaving diagnostic replies unchanged.
        pub fn refuse_complete_stdout(&self, key: &str, why: &str) -> &Self {
            self.strict_refusals
                .lock()
                .unwrap()
                .insert(key.to_owned(), why.to_owned());
            self
        }

        /// Refuse the one-byte parent commit without delivering it.
        pub fn stage_commit_refuses(&self, refused: bool) -> &Self {
            *self.stage_commit_refused.lock().unwrap() = refused;
            self
        }

        /// Leave abort settlement unproven; the caller must retain its persistent lock.
        pub fn stage_abort_refuses(&self, refused: bool) -> &Self {
            *self.stage_abort_refused.lock().unwrap() = refused;
            self
        }

        /// Queue the result returned by [`System::await_start`].
        pub fn start_answer(&self, answer: Result<(), RunFailed>) -> &Self {
            self.start_answers.lock().unwrap().push_back(answer);
            self
        }

        /// Queue the next reply for [`System::ask`].
        pub fn answer(&self, line: Option<&str>) -> &Self {
            self.answers
                .lock()
                .unwrap()
                .push_back(line.map(str::to_owned));
            self
        }

        /// The processes running now, by executable path ([`System::processes`],
        /// [`System::process_paths`]), in place of any listed before: each under a pid from
        /// [`Fake::PIDS`] on, started by launchd (so with no controlling terminal) and with its
        /// file's [`super::Process::short_name`], its arguments unread.
        pub fn running(&self, exes: &[&str]) -> &Self {
            let rows = (Self::PIDS..)
                .zip(exes)
                .map(|(pid, exe)| {
                    let exe = PathBuf::from(exe);
                    let name = exe.file_name().unwrap_or_default().to_string_lossy();
                    super::Process {
                        pid,
                        ppid: super::Process::LAUNCHD,
                        terminal: false,
                        comm: super::Process::short_name(&name),
                        exe: Some(exe),
                    }
                })
                .collect();
            *self.processes.lock().unwrap() = Some(rows);
            self
        }

        /// The first pid [`Fake::running`] gives: above every pid the kernel hands out (macOS's
        /// top is 99 999, Linux's 4 194 304), so never the test process's own.
        pub const PIDS: u32 = 1 << 23;

        /// One more process running now ([`System::processes`]), and the argument words
        /// [`System::process_argv`] reads of it (`None`: unread, as of a program macOS's program
        /// check holds).
        pub fn process(&self, row: super::Process, argv: Option<&[&str]>) -> &Self {
            if let Some(words) = argv {
                self.argvs
                    .lock()
                    .unwrap()
                    .insert(row.pid, words.iter().map(Into::into).collect());
            }
            self.processes
                .lock()
                .unwrap()
                .get_or_insert_with(Vec::new)
                .push(row);
            self
        }

        /// The next scripted answer ([`Fake::answer`]) to `prompt`.
        fn next_answer(&self, prompt: &str) -> io::Result<Option<Secret>> {
            match self.answers.lock().unwrap().pop_front() {
                Some(Some(line)) => Secret::new(line.as_bytes()).map(Some),
                Some(None) => Ok(None),
                None => Err(io::Error::other(format!(
                    "fake: no answer scripted for `{prompt}`"
                ))),
            }
        }

        /// Recorded `(key, input)` pairs from [`System::run_fed`].
        #[must_use]
        pub fn fed(&self) -> Vec<(String, Vec<u8>)> {
            self.fed.lock().unwrap().clone()
        }

        /// The destinations [`System::rename_noreplace`] renamed onto while a clone's publish
        /// held the update boundary (init-on-by-default design §6.7).
        #[must_use]
        pub fn guarded_renames(&self) -> Vec<PathBuf> {
            self.guarded_renames.lock().unwrap().clone()
        }

        /// Install a dynamic command handler consulted before scripted replies.
        pub fn simulate(
            &self,
            model: impl Fn(&Self, &Cmd) -> Option<Ran> + Send + Sync + 'static,
        ) -> &Self {
            *self.model.lock().unwrap() = Some(Arc::new(model));
            self
        }

        /// Answer [`System::identity`] as `user`'s login on an arm64 Mac running macOS 27.0.
        pub fn logged_in(&self, user: &User) -> &Self {
            *self.identity.lock().unwrap() = Some(super::Identity {
                user: user.clone(),
                arch: "arm64".into(),
                os_version: "27.0".into(),
            });
            self
        }

        /// Answer the Accessibility trust readings in turn, the last one for every later
        /// reading.
        pub fn trusts(&self, readings: &[bool]) -> &Self {
            *self.trust.lock().unwrap() = readings.to_vec();
            self
        }

        /// Script what [`System::service`] answers, one look each, the last repeating.
        pub fn services(&self, looks: Vec<aterm_uds::services::Look>) -> &Self {
            *self.services.lock().unwrap() = looks;
            self
        }

        /// Run a model at the terminal-command seam; its optional result overrides the
        /// scripted reply. No terminal or child process is opened.
        pub fn tty_model(
            &self,
            model: impl Fn(&Self, &Cmd) -> Option<Ran> + Send + Sync + 'static,
        ) -> &Self {
            *self.tty_hook.lock().unwrap() = Some(Arc::new(model));
            self
        }

        /// The next trust reading ([`Fake::trusts`]).
        fn next_trust(&self) -> Option<bool> {
            let mut trust = self.trust.lock().unwrap();
            if trust.len() > 1 {
                Some(trust.remove(0))
            } else {
                trust.first().copied()
            }
        }

        /// Run `hook` at each [`System::sleep`], after it is recorded: a model whose time a
        /// sleep moves on.
        pub fn on_sleep(
            &self,
            hook: impl Fn(&Self, std::time::Duration) + Send + Sync + 'static,
        ) -> &Self {
            *self.on_sleep.lock().unwrap() = Some(Arc::new(hook));
            self
        }

        /// Mark `pid` as a process that has exited.
        pub fn exited(&self, pid: u32) -> &Self {
            self.alive.lock().unwrap().retain(|alive| *alive != pid);
            self
        }

        /// Make [`System::run`] of the command whose display is `key` give no answer within its
        /// budget: [`RunFailed::TimedOut`] with that budget.
        pub fn time_out(&self, key: &str) -> &Self {
            self.timeouts.lock().unwrap().insert(key.to_owned());
            self
        }

        /// Script `reply` for the command whose display is `key`.
        pub fn on(&self, key: &str, reply: Ran) -> &Self {
            self.replies
                .lock()
                .unwrap()
                .entry(key.to_owned())
                .or_default()
                .push(reply);
            self
        }

        /// Exact HID service changes requested through [`System::set_keyboard_maps`].
        pub fn keyboard_changes(&self) -> Vec<super::KeyboardMapChange> {
            self.keyboard_changes.lock().unwrap().clone()
        }

        /// Script `reply` for [`System::tcc_access`] of `clients` (`&[]` for the open check).
        pub fn on_tcc(
            &self,
            clients: &[&str],
            reply: Result<Vec<crate::items::person::TccRow>, String>,
        ) -> &Self {
            let key = if clients.is_empty() {
                "tcc open".to_owned()
            } else {
                format!("tcc {}", clients.join(" "))
            };
            self.tcc_replies
                .lock()
                .unwrap()
                .entry(key)
                .or_default()
                .push(reply);
            self
        }

        /// Script a GitHub read without retaining its credential or opening a socket. A
        /// failure is of no particular kind ([`Fake::fail_github_api`] names one).
        pub fn on_github_api(
            &self,
            user: &User,
            endpoint: &str,
            reply: Result<GithubResponse, String>,
        ) -> &Self {
            self.github_replies
                .lock()
                .unwrap()
                .entry(format!("{}$ agh GET {endpoint}", user.name))
                .or_default()
                .push(reply.map_err(GithubError::other));
            self
        }

        /// Script a GitHub read that fails as `failure`.
        pub fn fail_github_api(
            &self,
            user: &User,
            endpoint: &str,
            failure: GithubFailure,
        ) -> &Self {
            self.github_replies
                .lock()
                .unwrap()
                .entry(format!("{}$ agh GET {endpoint}", user.name))
                .or_default()
                .push(Err(GithubError {
                    failure,
                    why: format!("fake: GitHub read failed ({failure:?})"),
                }));
            self
        }

        /// Send the agh client to `transport` and `endpoints` (agh-fake's loopback): what
        /// [`System::github_transport`] answers. The client's effect seam stays this `Fake`'s
        /// one simulated [`agh::Fx`].
        #[must_use]
        pub fn with_github(
            mut self,
            transport: Arc<dyn agh::wire::Transport>,
            endpoints: agh::wire::Endpoints,
        ) -> Self {
            self.github = Some((transport, endpoints));
            self
        }

        /// Read agh.toml under `dirs` (a scratch `Dirs::under(root)`): what
        /// [`System::agh_dirs`] answers.
        #[must_use]
        pub fn with_agh_dirs(mut self, dirs: agh::config::Dirs) -> Self {
            self.agh_dirs = Some(dirs);
            self
        }

        /// Script a [`System::github_rest`] response.
        pub fn on_github_rest(
            &self,
            method: super::RestMethod,
            endpoint: &str,
            reply: Result<GithubResponse, String>,
        ) -> &Self {
            self.github_replies
                .lock()
                .unwrap()
                .entry(format!("github {method} {endpoint}"))
                .or_default()
                .push(reply.map_err(GithubError::other));
            self
        }

        /// Seed a file.
        pub fn file(&self, path: &str, bytes: &[u8]) -> &Self {
            self.files
                .lock()
                .unwrap()
                .insert(PathBuf::from(path), (bytes.to_vec(), 0o644));
            self
        }

        /// Seed a `stat` answer.
        pub fn stat_of(&self, path: &str, stat: Stat) -> &Self {
            self.stats.lock().unwrap().insert(PathBuf::from(path), stat);
            self
        }

        /// Make `stat` of `path` fail with `kind` (a directory the caller may not search).
        pub fn stat_fails(&self, path: &str, kind: io::ErrorKind) -> &Self {
            self.stat_errors
                .lock()
                .unwrap()
                .insert(PathBuf::from(path), kind);
            self
        }

        /// Fail the next `stat` of `path`, then return its seeded answer again.
        pub fn stat_fails_once(&self, path: &str, kind: io::ErrorKind) -> &Self {
            self.stat_errors_once
                .lock()
                .unwrap()
                .insert(PathBuf::from(path), kind);
            self
        }

        /// Make each [`System::rename`] onto `to` fail with `kind`, moving nothing.
        pub fn rename_fails(&self, to: &str, kind: io::ErrorKind) -> &Self {
            self.rename_errors
                .lock()
                .unwrap()
                .insert(PathBuf::from(to), kind);
            self
        }

        /// Fail the next listing of `path`, then list its seeded entries again.
        pub fn list_fails_once(&self, path: &str, kind: io::ErrorKind) -> &Self {
            self.list_errors_once
                .lock()
                .unwrap()
                .insert(PathBuf::from(path), kind);
            self
        }

        /// Refuse a directory flush without opening a host path; `None` permits it again.
        pub fn durable_dir_fails(&self, path: &Path, kind: Option<io::ErrorKind>) -> &Self {
            let mut errors = self.directory_flush_errors.lock().unwrap();
            if let Some(kind) = kind {
                errors.insert(path.to_owned(), kind);
            } else {
                errors.remove(path);
            }
            self
        }

        fn produce(&self, key: &str) {
            let outputs = self
                .outputs
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .unwrap_or_default();
            for output in outputs {
                match output {
                    Output::File(path, bytes) => {
                        self.files.lock().unwrap().insert(path, (bytes, 0o600));
                    }
                    Output::Stat(path, stat) => {
                        self.stats.lock().unwrap().insert(path, stat);
                    }
                    Output::Running(exes) => {
                        self.running(&exes.iter().map(String::as_str).collect::<Vec<_>>());
                    }
                }
            }
        }

        /// Every command run so far, by display, in order.
        #[must_use]
        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        /// The bytes written to `path`, if any.
        #[must_use]
        pub fn written(&self, path: &str) -> Option<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .get(Path::new(path))
                .map(|(b, _)| b.clone())
        }

        /// The mode `path` was written with, if it was.
        #[must_use]
        pub fn mode_of(&self, path: &str) -> Option<u32> {
            self.files
                .lock()
                .unwrap()
                .get(Path::new(path))
                .map(|(_, m)| *m)
        }

        /// Mark `pid` as a live process.
        pub fn alive(&self, pid: u32) -> &Self {
            self.alive.lock().unwrap().push(pid);
            self
        }

        /// Record that `pid` started at `unix` seconds ([`System::process_started`]).
        pub fn born(&self, pid: u32, unix: u64) -> &Self {
            self.born.lock().unwrap().insert(pid, unix);
            self
        }

        /// Set the awake clock [`System::awake_ms`] reads to `ms`.
        pub fn awake_at(&self, ms: u64) -> &Self {
            *self.awake.lock().unwrap() = Some(ms);
            self
        }

        /// Seed a symbolic link.
        pub fn link(&self, path: &str, target: &str) -> &Self {
            self.links
                .lock()
                .unwrap()
                .insert(PathBuf::from(path), PathBuf::from(target));
            self
        }
    }

    /// The record of aterm's own Command Line Tools ask in this world's files
    /// ([`System::clt_mark`]): its writes and removals are this world's, and so its calls.
    impl aterm_types::clt_mark::Files for Fake {
        fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>> {
            if self.links.lock().unwrap().contains_key(path) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let bytes = match System::read_file(self, path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            if bytes.len() > max {
                return Err(io::ErrorKind::InvalidData.into());
            }
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| io::ErrorKind::InvalidData.into())
        }

        fn write(&self, path: &Path, text: &str) -> io::Result<()> {
            System::write_file(self, path, text.as_bytes(), 0o600)
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            System::remove(self, path)
        }

        /// One world, whose record changes one call at a time: the hold is its own.
        fn lock(&self, _path: &Path) -> io::Result<aterm_types::clt_mark::Held> {
            Ok(aterm_types::clt_mark::Held::new(()))
        }
    }

    impl System for Fake {
        fn clt_mark(&self) -> &dyn aterm_types::clt_mark::Files {
            self
        }

        fn github_api(
            &self,
            user: &User,
            _token: &Secret,
            endpoint: &str,
        ) -> Result<GithubResponse, GithubError> {
            let key = format!("{}$ agh GET {endpoint}", user.name);
            self.calls.lock().unwrap().push(key.clone());
            let mut replies = self.github_replies.lock().unwrap();
            match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => list.remove(0),
                Some(list) if list.len() == 1 => list[0].clone(),
                _ => Err(GithubError::other("fake: no GitHub response was scripted")),
            }
        }

        fn github_rest(
            &self,
            _token: &Secret,
            method: super::RestMethod,
            endpoint: &str,
            body: Option<&[u8]>,
        ) -> Result<GithubResponse, String> {
            let key = format!("github {method} {endpoint}");
            let call = match body {
                Some(body) => format!("{key} {}", String::from_utf8_lossy(body)),
                None => key.clone(),
            };
            self.calls.lock().unwrap().push(call);
            let mut replies = self.github_replies.lock().unwrap();
            match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => list.remove(0).map_err(String::from),
                Some(list) if list.len() == 1 => list[0].clone().map_err(String::from),
                _ => Err(format!("fake: no GitHub response was scripted for `{key}`")),
            }
        }

        fn github_transport(&self) -> (Arc<dyn agh::wire::Transport>, agh::wire::Endpoints) {
            self.github
                .clone()
                .unwrap_or_else(super::fail_closed_github)
        }

        fn agh_dirs(&self) -> Option<agh::config::Dirs> {
            self.agh_dirs.clone()
        }

        fn github_fx(&self) -> Arc<agh::Fx> {
            Arc::clone(&self.github_fx.0)
        }

        fn natural_scrolling(&self) -> Result<bool, String> {
            self.calls
                .lock()
                .unwrap()
                .push("scroll-direction get".into());
            self.scroll_natural
                .lock()
                .unwrap()
                .ok_or_else(|| "fake: native scroll direction was not scripted".into())
        }

        fn set_natural_scrolling(&self, natural: bool) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("scroll-direction set {natural}"));
            if let Some(why) = self.scroll_set_error.lock().unwrap().clone() {
                return Err(why);
            }
            let readback = self.scroll_set_readback.lock().unwrap().unwrap_or(natural);
            *self.scroll_natural.lock().unwrap() = Some(readback);
            if readback != natural {
                return Err(
                    "native scroll direction did not match after applying the setting".into(),
                );
            }
            Ok(())
        }

        fn set_keyboard_maps(
            &self,
            target_uid: u32,
            _vendor: u32,
            _product: u32,
            changes: &[super::KeyboardMapChange],
        ) -> Result<(), String> {
            if self.euid != target_uid {
                return Err("keyboard activation actor differs from the target user".into());
            }
            self.keyboard_changes
                .lock()
                .unwrap()
                .extend_from_slice(changes);
            Ok(())
        }

        fn scratch_dir(&self, parent: Option<&Path>, label: &str) -> io::Result<Scratch<'_>> {
            prefix(label)?;
            let mut sequence = self.scratch_sequence.lock().unwrap();
            for _ in 0..5 {
                let path = Self::scratch_path(parent, label, *sequence);
                *sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("fake scratch sequence exhausted"))?;
                let files = self.files.lock().unwrap();
                let mut stats = self.stats.lock().unwrap();
                let links = self.links.lock().unwrap();
                if files
                    .keys()
                    .chain(stats.keys())
                    .chain(links.keys())
                    .any(|p| p.starts_with(&path))
                {
                    continue;
                }
                stats.insert(
                    path.clone(),
                    Stat {
                        uid: self.euid,
                        gid: 0,
                        mode: 0o700,
                        dir: true,
                        symlink: false,
                        len: 0,
                    },
                );
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("scratch allocated {}", path.display()));
                return Ok(Scratch::from_owner(Box::new(FakeDirectory {
                    sys: self,
                    path: Some(path),
                })));
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "no free fake scratch candidate",
            ))
        }

        fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
            let key = cmd.display();
            self.calls.lock().unwrap().push(key.clone());
            if self.timeouts.lock().unwrap().contains(&key) {
                return Err(RunFailed::TimedOut(cmd.budget));
            }
            let model = self.model.lock().unwrap().clone();
            if let Some(model) = model
                && let Some(ran) = model(self, cmd)
            {
                self.produce(&key);
                return Ok(ran);
            }
            let mut replies = self.replies.lock().unwrap();
            let reply = match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => Ok(list.remove(0)),
                Some(list) if list.len() == 1 => Ok(list[0].clone()),
                _ => Err(RunFailed::Spawn(format!(
                    "fake: no reply scripted for `{key}`"
                ))),
            };
            drop(replies);
            if reply.is_ok() {
                self.produce(&key);
            }
            reply
        }

        fn run_complete_stdout(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
            if cmd.keep_on_timeout {
                return Err(RunFailed::Refused(
                    "complete stdout cannot keep timed-out partial output".into(),
                ));
            }
            let reply = self.run(cmd)?;
            if let Some(why) = self.strict_refusals.lock().unwrap().get(&cmd.display()) {
                return Err(RunFailed::Refused(why.clone()));
            }
            if reply.stdout.len() > aterm_uds::deadline::OUTPUT_KEEP {
                return Err(RunFailed::Refused(format!(
                    "complete stdout exceeds {} bytes",
                    aterm_uds::deadline::OUTPUT_KEEP
                )));
            }
            Ok(reply)
        }

        fn run_complete_stdout_input(&self, cmd: &Cmd, _input: &[u8]) -> Result<Ran, RunFailed> {
            super::complete_input::admit(cmd)?;
            self.run_complete_stdout(cmd)
        }

        fn run_fed(&self, cmd: &Cmd, input: &Secret) -> Result<Ran, RunFailed> {
            let key = format!("fed {}", cmd.display());
            self.calls.lock().unwrap().push(key.clone());
            self.fed
                .lock()
                .unwrap()
                .push((key.clone(), input.bytes().to_vec()));
            let mut replies = self.replies.lock().unwrap();
            let reply = match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => Ok(list.remove(0)),
                Some(list) if list.len() == 1 => Ok(list[0].clone()),
                _ => Err(RunFailed::Spawn(format!(
                    "fake: no reply scripted for `{key}`"
                ))),
            };
            drop(replies);
            if reply.is_ok() {
                self.produce(&key);
            }
            reply
        }

        fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
            let kind = if secret { "ask secret" } else { "ask" };
            self.calls.lock().unwrap().push(format!("{kind} {prompt}"));
            self.next_answer(prompt)
        }

        /// Recorded as `ask kept <prompt>`; answered as [`System::ask`] is.
        fn ask_kept(&self, prompt: &str) -> io::Result<Option<Secret>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("ask kept {prompt}"));
            self.next_answer(prompt)
        }

        /// Answered as [`Fake::running`] and [`Fake::process`] say, and not recorded: a look
        /// changes nothing.
        fn processes(&self) -> Option<Vec<super::Process>> {
            self.processes.lock().unwrap().clone()
        }

        /// Answered as [`Fake::process`] says, and not recorded.
        fn process_argv(&self, process: &super::Process) -> Option<Vec<std::ffi::OsString>> {
            self.argvs.lock().unwrap().get(&process.pid).cloned()
        }

        fn has_tty(&self) -> bool {
            self.tty
        }

        fn person_present(&self) -> bool {
            !self.away.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn update_boundary(&self) -> &aterm_types::update_boundary::Boundary {
            &self.boundary
        }

        fn full_disk_access(&self) -> Option<bool> {
            self.full_disk_access
        }

        fn tcc_access(
            &self,
            clients: &[&str],
        ) -> Result<Vec<crate::items::person::TccRow>, String> {
            let key = if clients.is_empty() {
                "tcc open".to_owned()
            } else {
                format!("tcc {}", clients.join(" "))
            };
            self.calls.lock().unwrap().push(key.clone());
            let mut replies = self.tcc_replies.lock().unwrap();
            match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => list.remove(0),
                Some(list) if list.len() == 1 => list[0].clone(),
                _ => Err(format!(
                    "the TCC database did not answer: fake: no TCC reply scripted for `{key}`"
                )),
            }
        }

        fn volume(&self, _path: &Path) -> Option<aterm_capacity::reading::Volume> {
            self.volume.clone()
        }

        fn running_app(&self) -> Option<PathBuf> {
            self.running_app.clone()
        }

        fn awake_ms(&self) -> Option<u64> {
            *self.awake.lock().unwrap()
        }

        fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed> {
            let key = format!("tty {}", cmd.display());
            self.calls.lock().unwrap().push(key.clone());
            let model = self.tty_hook.lock().unwrap().clone();
            if let Some(model) = model
                && let Some(output) = model(self, cmd)
            {
                return Ok(output.status);
            }
            let mut replies = self.replies.lock().unwrap();
            let status = match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => Ok(list.remove(0).status),
                Some(list) if list.len() == 1 => Ok(list[0].status),
                _ => Err(RunFailed::Spawn(format!(
                    "fake: no reply scripted for `{key}`"
                ))),
            };
            drop(replies);
            if status.is_ok() {
                self.produce(&key);
            }
            status
        }

        fn admit_runtime(&self, root: &Path) -> Result<(), String> {
            match self.runtime_refusals.lock().unwrap().get(root) {
                Some(why) => Err(why.clone()),
                None => Ok(()),
            }
        }

        fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .map(|(b, _)| b.clone())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn file_version(&self, path: &Path) -> io::Result<[u8; 32]> {
            let stat = self.stat(path)?;
            if stat.dir || stat.symlink {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let mut digest = aterm_digest::Sha256::new();
            digest.update(self.read_file(path)?);
            digest.update(stat.uid.to_le_bytes());
            digest.update(stat.gid.to_le_bytes());
            digest.update(stat.mode.to_le_bytes());
            Ok(digest.finalize())
        }

        fn read_owned_file(
            &self,
            path: &Path,
            uid: u32,
            max: usize,
            private: bool,
        ) -> io::Result<Vec<u8>> {
            if path.as_os_str().as_encoded_bytes().contains(&0) || max.checked_add(1).is_none() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let stat = self.stat(path)?;
            super::read_owned_file::admit(
                stat.uid,
                uid,
                stat.mode,
                !stat.dir && !stat.symlink,
                private,
            )?;
            if stat.len > max as u64 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let bytes = {
                let files = self.files.lock().unwrap();
                let (bytes, _) = files.get(path).ok_or(io::ErrorKind::NotFound)?;
                if bytes.len() > max {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                bytes.clone()
            };
            let hook = self.on_owned_read.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(self, path);
            }
            Ok(bytes)
        }

        fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("write {} {mode:o}", path.display()));
            self.files
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), (bytes.to_vec(), mode));
            Ok(())
        }

        fn write_user_file(
            &self,
            user: &User,
            path: &Path,
            bytes: &[u8],
            mode: u32,
        ) -> io::Result<()> {
            super::user_file_target(user, path, mode)?;
            self.calls.lock().unwrap().push(format!(
                "{}$ write {} {mode:o}",
                user.name,
                path.display()
            ));
            self.files
                .lock()
                .unwrap()
                .insert(path.to_path_buf(), (bytes.to_vec(), mode));
            self.stats.lock().unwrap().insert(
                path.to_path_buf(),
                Stat {
                    uid: user.uid,
                    gid: user.gid,
                    mode,
                    dir: false,
                    symlink: false,
                    len: bytes.len() as u64,
                },
            );
            Ok(())
        }

        fn durable_dir(&self, path: &Path) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("durable-dir {}", path.display()));
            match self.directory_flush_errors.lock().unwrap().get(path) {
                Some(kind) => Err(io::Error::from(*kind)),
                None => Ok(()),
            }
        }

        fn ordered_dir(&self, path: &Path) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("ordered-dir {}", path.display()));
            match self.directory_flush_errors.lock().unwrap().get(path) {
                Some(kind) => Err(io::Error::from(*kind)),
                None => Ok(()),
            }
        }

        fn remove(&self, path: &Path) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("remove {}", path.display()));
            self.files.lock().unwrap().remove(path);
            self.links.lock().unwrap().remove(path);
            self.stats.lock().unwrap().remove(path);
            Ok(())
        }

        fn remove_empty_dir(&self, path: &Path) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("rmdir {}", path.display()));
            let files = self.files.lock().unwrap();
            let links = self.links.lock().unwrap();
            let mut stats = self.stats.lock().unwrap();
            if files.contains_key(path)
                || links.contains_key(path)
                || stats
                    .get(path)
                    .is_some_and(|stat| !stat.dir || stat.symlink)
            {
                return Err(io::ErrorKind::NotADirectory.into());
            }
            if files
                .keys()
                .chain(links.keys())
                .chain(stats.keys())
                .any(|child| child != path && child.starts_with(path))
            {
                return Err(io::ErrorKind::DirectoryNotEmpty.into());
            }
            stats.remove(path);
            Ok(())
        }

        fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("mkdir {} {mode:o}", path.display()));
            let parents: Vec<_> = path.ancestors().collect();
            let mut gid = 0;
            for parent in parents
                .into_iter()
                .rev()
                .filter(|p| !p.as_os_str().is_empty())
            {
                match self.stat(parent) {
                    Ok(stat) if stat.dir && !stat.symlink => gid = stat.gid,
                    Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let root = parent.is_absolute() && parent.parent().is_none();
                        self.stats.lock().unwrap().insert(
                            parent.to_path_buf(),
                            Stat {
                                uid: if root { 0 } else { self.euid },
                                gid,
                                mode: if root { 0o755 } else { mode },
                                dir: true,
                                symlink: false,
                                len: 0,
                            },
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }

        fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("chown {uid}:{gid} {}", path.display()));
            Ok(())
        }

        fn set_owner_and_mode(&self, path: &Path, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
            let known = self.files.lock().unwrap().contains_key(path)
                || self.stats.lock().unwrap().contains_key(path);
            if !known {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("own {uid}:{gid} {mode:o} {}", path.display()));
            Ok(())
        }

        fn stat(&self, path: &Path) -> io::Result<Stat> {
            if let Some(kind) = self.stat_errors_once.lock().unwrap().remove(path) {
                return Err(io::Error::from(kind));
            }
            if let Some(kind) = self.stat_errors.lock().unwrap().get(path) {
                return Err(io::Error::from(*kind));
            }
            if let Some(s) = self.stats.lock().unwrap().get(path) {
                return Ok(*s);
            }
            if self.links.lock().unwrap().contains_key(path) {
                return Ok(Stat {
                    uid: 0,
                    gid: 0,
                    mode: 0o755,
                    dir: false,
                    symlink: true,
                    len: 0,
                });
            }
            self.files
                .lock()
                .unwrap()
                .get(path)
                .map(|(b, mode)| Stat {
                    uid: 0,
                    gid: 0,
                    mode: *mode,
                    dir: false,
                    symlink: false,
                    len: b.len() as u64,
                })
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn list_dir(&self, path: &Path) -> io::Result<Vec<String>> {
            if let Some(kind) = self.list_errors_once.lock().unwrap().remove(path) {
                return Err(io::Error::from(kind));
            }
            let files = self.files.lock().unwrap();
            let links = self.links.lock().unwrap();
            let mut names: Vec<String> = files
                .keys()
                .chain(links.keys())
                .filter(|p| p.parent() == Some(path))
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .collect();
            names.sort();
            Ok(names)
        }

        fn sha256(&self, path: &Path) -> io::Result<String> {
            let bytes = self.read_file(path)?;
            let mut hasher = aterm_digest::Sha256::new();
            hasher.update(&bytes);
            Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
        }

        fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()> {
            let bytes = self.read_file(from)?;
            self.calls.lock().unwrap().push(format!(
                "copy {} {} {mode:o}",
                from.display(),
                to.display()
            ));
            self.files
                .lock()
                .unwrap()
                .insert(to.to_path_buf(), (bytes, mode));
            Ok(())
        }

        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("rename {} {}", from.display(), to.display()));
            if let Some(kind) = self.rename_errors.lock().unwrap().get(to) {
                return Err(io::Error::from(*kind));
            }
            {
                let mut links = self.links.lock().unwrap();
                if let Some(target) = links.remove(from) {
                    links.insert(to.to_path_buf(), target);
                }
            }
            let mut files = self.files.lock().unwrap();
            if let Some(v) = files.remove(from) {
                files.insert(to.to_path_buf(), v);
            }
            let below: Vec<PathBuf> = files
                .keys()
                .filter(|p| p.starts_with(from) && *p != from)
                .cloned()
                .collect();
            for p in below {
                if let (Some(v), Ok(rest)) = (files.remove(&p), p.strip_prefix(from)) {
                    files.insert(to.join(rest), v);
                }
            }
            Ok(())
        }

        fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "rename-noreplace {} {}",
                from.display(),
                to.display(),
            ));
            if self
                .boundary
                .is_held(aterm_types::update_boundary::GuardedItem::ClonePublishRename)
            {
                self.guarded_renames.lock().unwrap().push(to.to_path_buf());
            }
            if [from, to]
                .iter()
                .any(|p| p.as_os_str().as_encoded_bytes().contains(&0))
            {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let mut files = self.files.lock().unwrap();
            let mut stats = self.stats.lock().unwrap();
            let mut links = self.links.lock().unwrap();
            if files
                .keys()
                .chain(stats.keys())
                .chain(links.keys())
                .any(|p| p.starts_with(to))
            {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            if !files.contains_key(from) && !stats.contains_key(from) && !links.contains_key(from) {
                return Err(io::ErrorKind::NotFound.into());
            }
            if to.starts_with(from) {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            fn move_tree<V>(map: &mut BTreeMap<PathBuf, V>, from: &Path, to: &Path) {
                let paths: Vec<_> = map
                    .keys()
                    .filter(|p| p.starts_with(from))
                    .cloned()
                    .collect();
                for path in paths {
                    if let Some(value) = map.remove(&path) {
                        let relative = path
                            .strip_prefix(from)
                            .expect("the selected path is under from");
                        map.insert(to.join(relative), value);
                    }
                }
            }
            move_tree(&mut files, from, to);
            move_tree(&mut stats, from, to);
            move_tree(&mut links, from, to);
            Ok(())
        }

        fn euid(&self) -> u32 {
            self.euid
        }

        fn now(&self) -> u64 {
            self.now
        }

        fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + Send + '_>, String> {
            Ok(Box::new(Journaled(self)))
        }

        fn create_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
            let mut files = self.files.lock().unwrap();
            if files.contains_key(path) || self.links.lock().unwrap().contains_key(path) {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("create {} {mode:o}", path.display()));
            files.insert(path.to_path_buf(), (bytes.to_vec(), mode));
            Ok(())
        }

        fn pid_alive(&self, pid: u32) -> bool {
            self.alive.lock().unwrap().contains(&pid)
        }

        /// Recorded as `sleep <ms>ms`; no time passes unless [`Fake::on_sleep`] moves a model.
        fn sleep(&self, period: std::time::Duration) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("sleep {}ms", period.as_millis()));
            let hook = self.on_sleep.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(self, period);
            }
        }

        /// Recorded as `ax-trusted`; answered as [`Fake::trusts`] scripts.
        fn accessibility_trusted(&self) -> Option<bool> {
            self.calls.lock().unwrap().push("ax-trusted".into());
            self.next_trust()
        }

        /// Recorded as `ax-request`; answered as [`Fake::trusts`] scripts.
        fn request_accessibility(&self) -> Option<bool> {
            self.calls.lock().unwrap().push("ax-request".into());
            self.next_trust()
        }

        fn responsible_app(&self) -> Option<String> {
            self.responsible.clone()
        }

        /// Recorded as `ax-walk`; answered as [`Fake::walks`] says.
        fn walk_accessibility(&self) -> super::AccessibilityWalk {
            self.calls.lock().unwrap().push("ax-walk".into());
            self.walks
        }

        fn administrator(&self) -> Option<bool> {
            self.admin
        }

        /// Answered as [`Fake::services`] scripts, and not recorded: a look changes nothing.
        fn service(&self, _restart: &aterm_vitals::services::Restart) -> aterm_uds::services::Look {
            let mut looks = self.services.lock().unwrap();
            if looks.len() > 1 {
                looks.remove(0)
            } else {
                looks
                    .first()
                    .cloned()
                    .unwrap_or(aterm_uds::services::Look::NoneRunning)
            }
        }

        fn hold_service_restart(
            &self,
            _path: &Path,
        ) -> io::Result<Box<dyn super::ServiceRestartHold>> {
            struct Inert;
            impl super::ServiceRestartHold for Inert {}
            match self.service_restart_lock.as_deref() {
                Some(path) => {
                    aterm_update_core::FileLock::acquire_within(path, std::time::Duration::ZERO)
                        .map(|lock| Box::new(lock) as Box<dyn super::ServiceRestartHold>)
                }
                None => Ok(Box::new(Inert)),
            }
        }

        /// Recorded as `low-data-mode`; answered as [`Fake::low_data`] says.
        fn low_data_mode(&self) -> Option<bool> {
            self.calls.lock().unwrap().push("low-data-mode".into());
            self.low_data
        }

        fn install_network(&self) -> Option<Box<dyn super::InstallNetworkWatch + '_>> {
            Some(Box::new(InstallWatch(self)))
        }

        /// Recorded as `identity`, the in-process reading; answered as [`Fake::logged_in`] set.
        fn identity(&self) -> Result<super::Identity, String> {
            self.calls.lock().unwrap().push("identity".into());
            self.identity
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| "no account scripted (Fake::logged_in)".into())
        }

        fn account_home(&self) -> Option<PathBuf> {
            self.identity
                .lock()
                .unwrap()
                .as_ref()
                .map(|id| PathBuf::from(&id.user.home))
        }

        fn process_started(&self, pid: u32) -> Option<u64> {
            self.born.lock().unwrap().get(&pid).copied()
        }

        fn stage_detached(
            &self,
            cmd: &Cmd,
            log: &Path,
        ) -> Result<Box<dyn StagedChild + '_>, RunFailed> {
            super::staged_child::admit_request(cmd)?;
            if self.next_pid == 0 {
                return Err(RunFailed::Refused("fake stage pid is not scripted".into()));
            }
            self.calls.lock().unwrap().push(format!(
                "stage {} >> {}",
                cmd.display(),
                log.display()
            ));
            Ok(Box::new(FakeStaged {
                sys: self,
                pid: self.next_pid,
                committed: false,
                aborted: false,
                abort_attempted: false,
            }))
        }

        fn await_start(&self, _limit: std::time::Duration) -> Result<(), RunFailed> {
            self.calls.lock().unwrap().push("await start".into());
            self.start_answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Err(RunFailed::Refused(
                        "no synthetic start byte was delivered".into(),
                    ))
                })
        }

        fn spawn_detached(&self, cmd: &Cmd, log: &Path) -> Result<u32, RunFailed> {
            self.calls.lock().unwrap().push(format!(
                "detach {} >> {}",
                cmd.display(),
                log.display()
            ));
            Ok(self.next_pid)
        }

        fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "symlink {} -> {}",
                link.display(),
                target.display()
            ));
            self.links
                .lock()
                .unwrap()
                .insert(link.to_path_buf(), target.to_path_buf());
            Ok(())
        }

        fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
            self.links
                .lock()
                .unwrap()
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))
        }

        /// `path` resolved one component at a time, as `realpath` resolves it: a seeded link
        /// ([`Fake::link`]) is replaced by its target, read relative to the link's folder, and
        /// `..` steps out of the folder reached so far, so no `..` is left in the answer.
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            use std::path::Component;
            let links = self.links.lock().unwrap();
            let parts = |path: &Path| -> Vec<std::ffi::OsString> {
                path.components()
                    .rev()
                    .map(|part| part.as_os_str().to_owned())
                    .collect()
            };
            let mut todo = parts(path);
            let mut real = PathBuf::new();
            let mut hops = 0;
            while let Some(part) = todo.pop() {
                match Path::new(&part).components().next() {
                    Some(Component::RootDir) => real = PathBuf::from("/"),
                    Some(Component::ParentDir) => {
                        real.pop();
                    }
                    Some(Component::Normal(_)) => {
                        real.push(&part);
                        if let Some(target) = links.get(&real) {
                            hops += 1;
                            if hops > 32 {
                                return Err(io::Error::other("too many levels of symbolic links"));
                            }
                            real.pop();
                            todo.extend(parts(target));
                        }
                    }
                    _ => {}
                }
            }
            Ok(real)
        }

        fn relay(&self, cmd: &Cmd) -> Result<Box<dyn Relaying + '_>, RunFailed> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("relay {}", cmd.display()));
            if self.relay_port == 0 {
                return Err(RunFailed::Spawn("fake: no relay port scripted".into()));
            }
            Ok(Box::new(Relayed {
                calls: &self.calls,
                port: self.relay_port,
                open: true,
            }))
        }

        fn lock_screen(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("lock-screen".into());
            self.lock_refused.clone().map_or(Ok(()), Err)
        }

        fn catch_stops(&self) -> Box<dyn std::any::Any> {
            self.calls.lock().unwrap().push("catch stops".into());
            Box::new(Caught(Arc::clone(&self.calls)))
        }

        fn defer_stops(&self) -> Box<dyn std::any::Any> {
            self.calls.lock().unwrap().push("defer stops".into());
            Box::new(Deferred {
                calls: Arc::clone(&self.calls),
                readings: Arc::clone(&self.stop_readings),
                requested: Arc::clone(&self.requested_stop),
                stop_after: self.stop_after,
            })
        }

        fn stopped(&self) -> Option<i32> {
            let requested = *self.requested_stop.lock().unwrap();
            requested.or_else(|| read_stop(&self.stop_readings, self.stop_after))
        }

        fn ignore_hangups(&self) {
            self.calls.lock().unwrap().push("ignore hangups".into());
        }

        fn remote_login(&self) -> bool {
            self.remote_login
        }

        fn graphic_session(&self) -> Option<bool> {
            self.graphic_session
        }

        fn terminal(&self) -> bool {
            self.terminal
        }

        fn agent_ancestor(&self) -> Option<&'static str> {
            self.agent
        }

        fn macos(&self) -> bool {
            !self.linux
        }
    }

    struct InstallWatch<'a>(&'a Fake);

    impl super::InstallNetworkWatch for InstallWatch<'_> {
        fn reading(&self) -> Option<super::InstallNetworkReading> {
            *self.0.install_path.lock().unwrap()
        }

        fn wait(
            &mut self,
            _revision: Option<u64>,
            budget: std::time::Duration,
        ) -> std::time::Duration {
            self.0.sleep(budget);
            budget
        }
    }

    /// One [`System::stopped`] reading: `SIGINT` once more than `stop_after` were taken.
    fn read_stop(readings: &Mutex<usize>, stop_after: Option<usize>) -> Option<i32> {
        let mut readings = readings.lock().unwrap();
        *readings += 1;
        stop_after
            .is_some_and(|after| *readings > after)
            .then_some(libc::SIGINT)
    }

    /// [`Fake::defer_stops`]'s handle: its drop restores, then reads once more and records the
    /// stop it would send again.
    struct Deferred {
        calls: Arc<Mutex<Vec<String>>>,
        readings: Arc<Mutex<usize>>,
        requested: Arc<Mutex<Option<i32>>>,
        stop_after: Option<usize>,
    }

    impl Drop for Deferred {
        fn drop(&mut self) {
            self.calls.lock().unwrap().push("stops restored".into());
            let requested = *self.requested.lock().unwrap();
            if let Some(sig) = requested.or_else(|| read_stop(&self.readings, self.stop_after)) {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("stop sent again: signal {sig}"));
            }
        }
    }

    struct Caught(Arc<Mutex<Vec<String>>>);

    impl Drop for Caught {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("stops restored".into());
        }
    }

    struct Relayed<'a> {
        calls: &'a Mutex<Vec<String>>,
        port: u16,
        open: bool,
    }

    impl Relaying for Relayed<'_> {
        fn port(&self) -> u16 {
            self.port
        }

        fn close(&mut self) -> Result<(), String> {
            if std::mem::take(&mut self.open) {
                self.calls.lock().unwrap().push("relay closed".into());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user file is written only at a plain path strictly inside the user's home with
    /// permission bits alone: `..` out of the home, the home itself, a `/` home and a setuid
    /// mode are refused before anything is written. NEGATIVE CONTROL: a file under a
    /// symbolically linked `~/.config` (a dotfile manager's) is written with its mode.
    #[test]
    fn a_user_file_outside_the_home_or_with_more_than_permission_bits_is_refused() {
        let fake = Fake::root();
        let alice = User {
            name: "alice".into(),
            uid: 502,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let root_home = User {
            home: "/".into(),
            ..alice.clone()
        };
        for (user, path, mode, kind) in [
            (
                &alice,
                "/Users//alice/../bob/x",
                0o600,
                io::ErrorKind::PermissionDenied,
            ),
            (
                &alice,
                "/Users//alice",
                0o600,
                io::ErrorKind::PermissionDenied,
            ),
            (
                &alice,
                "Users/alice/x",
                0o600,
                io::ErrorKind::PermissionDenied,
            ),
            (
                &alice,
                "/Users//alice2/x",
                0o600,
                io::ErrorKind::PermissionDenied,
            ),
            (
                &root_home,
                "/etc/sudoers.d/x",
                0o600,
                io::ErrorKind::PermissionDenied,
            ),
            (
                &alice,
                "/Users//alice/x",
                0o4755,
                io::ErrorKind::InvalidInput,
            ),
            (
                &alice,
                "/Users//alice/x",
                0o1777,
                io::ErrorKind::InvalidInput,
            ),
        ] {
            let refused = fake
                .write_user_file(user, Path::new(path), b"x", mode)
                .unwrap_err();
            assert_eq!(refused.kind(), kind, "{path} {mode:o}: {refused}");
        }
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
        fake.link("/Users//alice/.config", "/Users//alice/dotfiles/config");
        fake.write_user_file(&alice, Path::new("/Users//alice/.config/x"), b"x", 0o644)
            .unwrap();
        assert_eq!(fake.calls(), ["alice$ write /Users//alice/.config/x 644"]);
        assert_eq!(fake.mode_of("/Users//alice/.config/x"), Some(0o644));
    }

    #[test]
    fn child_failures_preserve_spawn_post_start_and_timeout_phases() {
        use aterm_uds::deadline::{CompleteOutputError, RunError};
        let budget = Duration::from_secs(60);
        assert!(matches!(
            RunFailed::from_child(RunError::Spawn(io::ErrorKind::NotFound.into()), budget),
            RunFailed::Spawn(_)
        ));
        for error in [
            RunError::Capture(io::ErrorKind::NotFound.into()),
            RunError::Wait(io::ErrorKind::NotFound.into()),
        ] {
            let failed = RunFailed::from_child(error, budget);
            assert!(matches!(failed, RunFailed::AfterStart(_)), "{failed:?}");
            assert!(failed.to_string().starts_with("failed after start:"));
        }
        assert_eq!(
            RunFailed::from_child(RunError::TimedOut, budget),
            RunFailed::TimedOut(budget)
        );
        for error in [
            CompleteOutputError::StdoutRead(io::ErrorKind::UnexpectedEof.into()),
            CompleteOutputError::StdoutTooLarge { limit: 3 },
            CompleteOutputError::StdoutIncomplete,
        ] {
            assert!(matches!(
                RunFailed::from_complete_output(error, budget),
                RunFailed::AfterStart(_)
            ));
        }
        assert!(matches!(
            RunFailed::from_complete_output(
                CompleteOutputError::Run(RunError::Spawn(io::ErrorKind::NotFound.into())),
                budget
            ),
            RunFailed::Spawn(_)
        ));
        assert_eq!(
            RunFailed::from_complete_output(CompleteOutputError::Run(RunError::TimedOut), budget),
            RunFailed::TimedOut(budget)
        );
    }

    /// The window's walk takes a sign-out's Accessibility ask only when its answer names that
    /// page; `OK declined` or a `not-now:accessibility` word is the owner's Not now on it; a walk
    /// under way over other pages (or one within the last minute, `OK recent`) has its page up,
    /// beside which macOS's own dialog would be a second question. NEGATIVE CONTROL: `OK
    /// granted` and an answer from a window older than the walk leave the ask to macOS.
    #[test]
    fn the_windows_answer_tells_a_walk_that_takes_the_ask_from_a_not_now_and_other_pages() {
        use AccessibilityWalk::{NotNow, NotTaken, OtherPages, Taken};
        for (reply, want) in [
            ("OK walking accessibility\n", Taken),
            ("OK walking fda accessibility", Taken),
            ("OK walking fda not-now:accessibility\n", NotNow),
            ("OK declined\n", NotNow),
            ("OK walking fda devtools\n", OtherPages),
            ("OK walking devtools not-now:fda\n", OtherPages),
            ("OK recent\n", OtherPages),
            ("OK granted\n", NotTaken),
            ("ERR unknown verb (try: help)\n", NotTaken),
            (
                "ERR headless: a headless instance brings no System Settings page up\n",
                NotTaken,
            ),
        ] {
            assert_eq!(walk_answer(reply, "accessibility"), want, "{reply}");
        }
    }

    #[test]
    fn complete_stdout_admits_scripted_whole_records_and_refuses_unproven_capture() {
        let sys = Fake::root();
        let cmd = Cmd::new("/synthetic-record");
        let record = "header\nfirst row\nlast row\n";
        sys.on(&cmd.display(), Ran::ok(record));
        assert_eq!(
            sys.run_complete_stdout(&cmd).unwrap().stdout,
            record.as_bytes()
        );
        sys.refuse_complete_stdout(&cmd.display(), "synthetic stdout did not reach EOF");
        let why = sys.run_complete_stdout(&cmd).unwrap_err().to_string();
        assert!(why.contains("did not reach EOF"), "{why}");
        assert_eq!(
            sys.run(&cmd).unwrap().stdout,
            record.as_bytes(),
            "diagnostic lane is unchanged"
        );
        let kept = cmd.clone().until_deadline();
        let before = sys.calls().len();
        assert!(
            sys.run_complete_stdout(&kept)
                .unwrap_err()
                .to_string()
                .contains("partial output")
        );
        assert_eq!(
            sys.calls().len(),
            before,
            "strict refusal precedes execution"
        );
    }

    #[test]
    fn complete_stdout_accepts_its_exact_bound_and_refuses_one_byte_more() {
        let sys = Fake::root();
        let cmd = Cmd::new("/synthetic-bound");
        let mut ran = Ran {
            status: Some(0),
            stdout: vec![b'x'; aterm_uds::deadline::OUTPUT_KEEP],
            stderr: b"diagnostic".to_vec(),
        };
        sys.on(&cmd.display(), ran.clone());
        ran.stdout.push(b'y');
        sys.on(&cmd.display(), ran);
        let exact = sys.run_complete_stdout(&cmd).unwrap();
        assert_eq!(exact.stdout.len(), aterm_uds::deadline::OUTPUT_KEEP);
        assert_eq!(exact.stderr, b"diagnostic");
        let why = sys.run_complete_stdout(&cmd).unwrap_err().to_string();
        assert!(why.contains("complete stdout exceeds"), "{why}");
    }

    #[test]
    fn a_word_is_quoted_only_when_a_shell_would_read_it_specially() {
        assert_eq!(shell_word("/usr/bin/defaults"), "/usr/bin/defaults");
        assert_eq!(shell_word("com.apple.dock"), "com.apple.dock");
        assert_eq!(shell_word("Assistant Enabled"), "'Assistant Enabled'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
        assert_eq!(shell_word(""), "''");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
    }

    fn alice() -> User {
        User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        }
    }

    #[test]
    fn fake_recursive_creation_recreates_removed_directories_and_preserves_user_ownership() {
        let who = alice();
        let mut sys = Fake::root();
        sys.euid = who.uid;
        let home = Path::new(&who.home);
        let home_stat = Stat {
            uid: who.uid,
            gid: who.gid,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        };
        sys.stat_of(home.to_str().unwrap(), home_stat);
        let state = home.join("Library/Application Support/aterm/init-activation");
        sys.create_dir_all(&state, 0o700).unwrap();
        assert_eq!(
            sys.stat(home).unwrap(),
            home_stat,
            "mkdir must preserve existing directory metadata"
        );
        let directory = sys.stat(&state).unwrap();
        assert!(directory.dir && !directory.symlink);
        assert_eq!(
            (directory.uid, directory.gid, directory.mode),
            (who.uid, who.gid, 0o700)
        );
        sys.remove(&state).unwrap();
        assert_eq!(
            sys.stat(&state).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        sys.create_dir_all(&state, 0o700).unwrap();
        assert_eq!(sys.stat(&state).unwrap(), directory);
        let marker = state.join("dock.pending");
        sys.write_user_file(&who, &marker, b"pending", 0o600)
            .unwrap();
        let stat = sys.stat(&marker).unwrap();
        assert_eq!(
            (stat.uid, stat.gid, stat.mode, stat.len),
            (who.uid, who.gid, 0o600, 7)
        );
        sys.remove(&marker).unwrap();
        assert_eq!(
            sys.stat(&marker).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        let conflict = home.join("conflict");
        sys.file(
            conflict.to_str().unwrap(),
            b"a file cannot contain a child directory",
        );
        assert!(sys.create_dir_all(&conflict.join("child"), 0o700).is_err());
        assert!(sys.stat(&conflict.join("child")).is_err());
    }

    #[test]
    fn display_names_the_user_and_quotes_each_word() {
        let cmd = Cmd::new("/usr/bin/defaults")
            .args([
                "write",
                "com.apple.assistant.support",
                "Assistant Enabled",
                "-bool",
                "false",
            ])
            .as_user(&alice());
        assert_eq!(
            cmd.display(),
            "alice$ /usr/bin/defaults write com.apple.assistant.support 'Assistant Enabled' -bool false"
        );
    }

    #[test]
    fn from_root_a_user_run_scrubs_the_environment_and_a_session_run_goes_through_launchctl() {
        let (program, args) = Live::argv_from(
            0,
            &Cmd::new("/usr/bin/open")
                .arg("u")
                .as_user(&alice())
                .in_session(),
        )
        .unwrap();
        assert_eq!(program, PathBuf::from("/bin/launchctl"));
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "asuser",
                "501",
                "/usr/bin/sudo",
                "-n",
                "-u",
                "alice",
                "-H",
                "--",
                "/usr/bin/env",
                "-i",
                "HOME=/Users//alice",
                "USER=alice",
                "LOGNAME=alice",
                "SHELL=/bin/zsh",
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "LANG=en_US.UTF-8",
                "/usr/bin/open",
                "u"
            ]
        );
    }

    #[test]
    fn without_root_a_run_as_another_user_is_refused_before_anything_starts() {
        let euid = 502;
        let mut other = alice();
        other.uid = euid + 1;
        let err = Live::argv_from(euid, &Cmd::new("/usr/bin/true").as_user(&other)).unwrap_err();
        assert!(matches!(err, RunFailed::Refused(_)), "{err:?}");
        let (program, args) = Live::argv_from(
            euid,
            &Cmd::new("/usr/bin/true").as_user(&User {
                uid: euid,
                ..alice()
            }),
        )
        .unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/true"));
        assert!(args.is_empty(), "{args:?}");
    }

    #[test]
    fn a_run_as_this_processs_own_account_gets_that_accounts_home_and_names() {
        let live = Live::new();
        if live.euid() == 0 {
            aterm_uds::host::could_not_run(
                "as root a run as this account is wrapped; the unwrapped run needs a non-root uid",
            );
        }
        let me = User {
            name: "lab-person".into(),
            uid: live.euid(),
            gid: 20,
            home: "/nonexistent/lab-person".into(),
            shell: String::new(),
            admin: false,
        };
        let cmd = Cmd::new("/usr/bin/env").as_user(&me).env("GIT_X", "1");
        let (program, args) = live.argv(&cmd).unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/env"));
        assert!(args.is_empty(), "{args:?}");
        for cmd in [cmd.clone(), cmd.until_deadline()] {
            let ran = live.run(&cmd).unwrap();
            assert!(ran.success(), "{ran:?}");
            let out = ran.out();
            let mut lines: Vec<&str> = out.lines().collect();
            lines.sort_unstable();
            assert_eq!(
                lines,
                [
                    "GIT_X=1",
                    "HOME=/nonexistent/lab-person",
                    "LANG=en_US.UTF-8",
                    "LOGNAME=lab-person",
                    "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                    "SHELL=/bin/zsh",
                    "USER=lab-person",
                ],
                "{}",
                cmd.display()
            );
        }
    }

    #[test]
    fn display_names_each_added_variable_and_masks_its_value() {
        let cmd = Cmd::new("/usr/bin/ssh-add")
            .secret_env("SSH_AUTH_SOCK", "/private/agent-secret.sock")
            .args(["-l"]);
        assert_eq!(cmd.display(), "SSH_AUTH_SOCK=\u{2026} /usr/bin/ssh-add -l");
        assert!(!cmd.display().contains("agent-secret"));
        assert!(!format!("{cmd:?}").contains("agent-secret"), "{cmd:?}");
    }

    #[test]
    fn a_new_file_is_ordered_and_a_copy_durable() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = aterm_tempfile::tempdir().unwrap();
        let live = Live::new();
        let recording = Recording::start();
        live.create_new(&dir.path().join("run.lock"), b"1 2\n", 0o600)
            .unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "a new file");
        live.copy_file(
            &dir.path().join("run.lock"),
            &dir.path().join("copy"),
            0o600,
        )
        .unwrap();
        assert_eq!(recording.take(), [Flush::Durable], "a copy");
    }

    #[test]
    fn a_relative_program_is_refused() {
        let live = Live::new();
        let err = live.run(&Cmd::new("defaults")).unwrap_err();
        assert!(matches!(err, RunFailed::Refused(_)), "{err:?}");
    }

    /// A `System` that never says where GitHub is reaches none of it: its agh client's transport
    /// sends nothing, its endpoints are 127.0.0.1's port 1, and it finds no agh.toml. The
    /// defaults are read through `Spawns`, a double that overrides neither method; a `Fake`
    /// never given `with_github` or `with_agh_dirs` answers the same, and is checked beside it.
    /// NEGATIVE CONTROL: `Live` sends to github.com.
    #[test]
    fn a_system_that_names_no_github_sends_nowhere_and_reads_no_agh_toml() {
        // A double that names no GitHub: its transport answers a round trip as not sent.
        fn sends_nowhere(sys: &dyn System) {
            let (transport, endpoints) = sys.github_transport();
            assert_eq!(endpoints, agh::wire::Endpoints::loopback(1, 1, 1, 1));
            let sent = transport.round_trip(agh::wire::WireRequest::new(
                agh::wire::Method::Get,
                endpoints.url(agh::wire::OriginRole::Api, "/user"),
                Vec::new(),
                1024,
                agh::wire::Deadlines::JSON,
            ));
            assert!(
                matches!(&sent, Err(e) if e.phase == agh::wire::Phase::NotSent),
                "{:?}",
                sent.err()
            );
            assert!(sys.agh_dirs().is_none());
        }
        let live = Live::new();
        sends_nowhere(&super::spawns::Spawns::new(&live));
        sends_nowhere(&Fake::default());
        assert_eq!(
            Live::new().github_transport().1,
            agh::wire::Endpoints::github_com()
        );
    }

    #[test]
    fn a_run_kept_to_its_deadline_ends_what_its_child_started_too() {
        let live = Live::new();
        let ran = live
            .run(
                &Cmd::new("/bin/sh")
                    .args(["-c", "/bin/sleep 120 & echo $!; wait"])
                    .budget(Duration::from_secs(2))
                    .until_deadline(),
            )
            .unwrap();
        assert_eq!(ran.status, None, "{ran:?}");
        let pid: u32 = ran
            .out()
            .trim()
            .parse()
            .expect("the shell printed its child's pid");
        let since = std::time::Instant::now();
        while aterm_uds::process::pid_alive(pid) {
            assert!(
                since.elapsed() < Duration::from_secs(60),
                "hang detector: {pid} outlived its run's deadline"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn owner_and_mode_are_set_through_the_file_and_never_through_a_link_or_a_second_name() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let live = Live::new();
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = dir.path().join("prefs.plist");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let meta = std::fs::metadata(&file).unwrap();
        live.set_owner_and_mode(&file, meta.uid(), meta.gid(), 0o644)
            .unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o7777, 0o644);

        let link = dir.path().join("link.plist");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(
            live.set_owner_and_mode(&link, meta.uid(), meta.gid(), 0o600)
                .is_err()
        );

        let second = dir.path().join("second.plist");
        std::fs::hard_link(&file, &second).unwrap();
        let err = live
            .set_owner_and_mode(&file, meta.uid(), meta.gid(), 0o600)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o7777, 0o644);
    }

    #[test]
    fn a_variable_set_on_a_run_shows_in_its_display_and_reaches_the_tool() {
        let cmd = Cmd::new("/usr/bin/env").env("HOMEBREW_NO_ANALYTICS", "1");
        assert_eq!(cmd.display(), "HOMEBREW_NO_ANALYTICS=1 /usr/bin/env");
        let ran = Live::new().run(&cmd).unwrap();
        assert!(ran.out().lines().any(|l| l == "HOMEBREW_NO_ANALYTICS=1"));
    }

    #[test]
    fn write_atomic_replaces_a_link_rather_than_following_it() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"untouched").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_atomic(&link, b"new", 0o600).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        assert_eq!(std::fs::read(&link).unwrap(), b"new");
        assert!(
            !std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn the_running_app_is_the_bundle_the_resolved_executable_sits_in() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let app = root.join("Applications/aterm.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("aterm"), b"").unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::os::unix::fs::symlink(macos.join("aterm"), root.join("bin/aterm")).unwrap();
        std::fs::write(root.join("bin/built"), b"").unwrap();
        let live = |exe: &str| Live {
            agent_exe: root.join(exe),
            ..Live::new()
        };
        assert_eq!(live("bin/aterm").running_app(), Some(app.clone()));
        assert_eq!(
            live("Applications/aterm.app/Contents/MacOS/aterm").running_app(),
            Some(app)
        );
        assert_eq!(live("bin/built").running_app(), None, "a build is no app");
        assert_eq!(live("bin/gone").running_app(), None);
    }

    /// [`Live::read_head`] reads at most `max` bytes of a regular file, and refuses a link at the
    /// end of the path or on its way (a program linked out of its bundle, a bundle under a linked
    /// folder), so it reads nothing outside the folders the path names, and refuses a FIFO at
    /// once, never waiting for a writer. NEGATIVE CONTROLS (scratch): a plain
    /// `std::fs::File::open` in place of `open_reader` reads through both links and waits on the
    /// FIFO until the one-minute detector fails; `O_NONBLOCK` alone in its place reads the FIFO
    /// as an empty file; reading with no `take` fails the 512-byte assertion.
    #[cfg(target_os = "macos")]
    #[test]
    fn read_head_reads_a_bounded_head_of_a_regular_file_alone() {
        use std::os::unix::ffi::OsStrExt as _;
        let temp = aterm_tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let live = Live::new();
        let program = root.join("program");
        std::fs::write(&program, [b'#'; 2000]).unwrap();
        assert_eq!(live.read_head(&program, 512).unwrap(), [b'#'; 512]);
        std::fs::write(root.join("outside"), b"#!/bin/zsh\n").unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("linked")).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::fs::write(root.join("real/program"), b"#!/bin/zsh\n").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("folder")).unwrap();
        for linked in ["linked", "folder/program"] {
            let read = live.read_head(&root.join(linked), 512);
            assert!(
                read.as_ref().is_err_and(aterm_uds::nofollow::is_link),
                "{linked}: {read:?}"
            );
        }
        let fifo = root.join("fifo");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a live owned C string names an absent file in this private fixture.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let read = Live::new().read_head(&fifo, 512);
            let _ = send.send(read.map_err(|e| aterm_uds::nofollow::not_regular(&e)));
        });
        assert_eq!(
            receive
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("a FIFO's read must not wait for a writer"),
            Err(true)
        );
    }
}
