// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one port between this crate and the operating system.
//!
//! Every tool an item runs, every file it reads or writes, and the converge journal its
//! changes are recorded in go through [`System`]. The live arm ([`Live`]) runs absolute
//! programs built by `aterm_uds::spawnfd::command`, each bounded by a deadline, with a fixed
//! environment. The test arm (`Fake`, test builds only) answers from a script and records
//! the argv, so no test touches the real `defaults`, `pmset`, `scutil` or `launchctl` of
//! the machine running it, nor its account's journal.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod complete_input;
mod github_api;
pub(crate) mod github_rest;
mod read_owned_file;
mod relay;
mod rename_noreplace;
mod scratch;
mod screen_lock;
mod scroll_direction;
mod secret;
mod staged_child;
mod stops;

pub use github_rest::RestMethod;
pub use scratch::Scratch;
pub use secret::{SECRET_MAX, Secret, no_core_dumps};
pub use staged_child::{START_BYTE, START_LIMIT, StagedChild};
pub use stops::name as stop_name;

/// The default bound on one tool run. Generous: a hung tool is what it catches, not a
/// slow one.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(60);

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

/// One tool run.
#[derive(Clone, PartialEq, Eq)]
pub struct Cmd {
    /// Absolute path of the program. Never resolved through `PATH`.
    pub program: PathBuf,
    /// Arguments after the program.
    pub args: Vec<OsString>,
    /// Run as this account (design §3.5): from root, `sudo -n -u <u> -H -- /usr/bin/env -i
    /// HOME=… USER=… LOGNAME=… SHELL=… PATH=… <program> …`.
    pub user: Option<User>,
    /// Run inside that user's login session (`launchctl asuser <uid>`), for tools that
    /// talk to the user's GUI session (`open`, `killall Dock`). The caller checks that the
    /// session exists (`launchctl print gui/<uid>`).
    pub session: bool,
    /// A file to read standard input from; standard input is closed otherwise.
    pub stdin: Option<PathBuf>,
    /// The deadline, counted from the start of the run.
    pub budget: Duration,
    /// Variables added to the fixed environment, in order (`HOMEBREW_NO_ANALYTICS=1`, or
    /// `SSH_AUTH_SOCK` for the signing agent). A run with `user` passes them through `env -i`,
    /// where they are visible in that process's argv, so a secret belongs only on a run
    /// without `user`, and a credential never in a variable: it goes on standard input
    /// ([`System::run_complete_stdout_input`]).
    pub env: Vec<(String, String)>,
    /// The names in `env` whose values [`Cmd::display`] masks ([`Cmd::secret_env`]).
    /// `Debug` masks every value.
    pub masked: Vec<String>,
    /// The deadline ends the run rather than failing it: for a tool that never exits on its
    /// own (`dns-sd -B` browses until it is killed). At the deadline the child's process
    /// group is killed (`SIGKILL`) and the run answers `Ok` with `status: None` and what it
    /// printed by then; a run that ends sooner answers as any other.
    pub keep_on_timeout: bool,
}

impl std::fmt::Debug for Cmd {
    /// Every field, with each `env` value masked as [`Cmd::display`] masks it.
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
        }
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

    /// The command as one shell-quoted line: the form revert lines and logs print, and the
    /// key the test arm's script is matched on (`<user>$ ` prefixed when `user` is set,
    /// each [`Cmd::env`] variable as a `NAME=value` word before the program).
    #[must_use]
    pub fn display(&self) -> String {
        let mut out = String::new();
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

/// `word` quoted for a POSIX shell: bare when it is made only of characters no shell
/// treats specially, else in single quotes with each `'` written `'\''`.
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
    /// The deadline passed; the process group was killed.
    TimedOut(Duration),
    /// The port refused the run before starting it (e.g. `as_user` without root).
    Refused(String),
}

impl std::fmt::Display for RunFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(why) => write!(f, "could not start: {why}"),
            Self::TimedOut(d) => write!(f, "no answer within {}s", d.as_secs()),
            Self::Refused(why) => write!(f, "refused: {why}"),
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

/// A listening [`System::relay`].
pub trait Relaying {
    /// The port it listens on, on `127.0.0.1`.
    fn port(&self) -> u16;

    /// Stop listening, then end (`SIGKILL`) and reap every run still carrying a connection.
    /// A second close does nothing.
    ///
    /// # Errors
    /// One line when the relay failed on its own before the close: it stopped accepting, or
    /// a connection's run did not start.
    fn close(&mut self) -> Result<(), String>;
}

/// The operating system, as items see it.
pub trait System: Sync {
    /// Read one GitHub API endpoint through agh as the already-admitted development
    /// credential. No credential is written to process environment, files or logs.
    /// The live arm runs only as `user`; unsupported and test backends never network.
    ///
    /// # Errors
    /// Unsupported backend, user mismatch, invalid credential, or incomplete/failed read.
    fn github_api(
        &self,
        _user: &User,
        _token: &Secret,
        _endpoint: &str,
    ) -> Result<GithubResponse, String> {
        Err("GitHub reads are not implemented by this backend".into())
    }

    /// One GitHub REST request as this process's own account (the machines controller):
    /// `method endpoint` to the API origin over aterm-http, `token` in an `Authorization`
    /// header built in memory, `body` as JSON. A redirect is refused, never followed. The
    /// live arm networks; the test arm answers from a script.
    ///
    /// # Errors
    /// Unsupported backend, an endpoint that is not one path, a failed or cut-short request,
    /// an answer over its bound, or a redirect. No error carries the token.
    fn github_rest(
        &self,
        _token: &Secret,
        _method: RestMethod,
        _endpoint: &str,
        _body: Option<&[u8]>,
    ) -> Result<GithubResponse, String> {
        Err("GitHub REST requests are not implemented by this backend".into())
    }

    /// Natural scrolling through the same native getter used by System Settings.
    /// Apple's getter synchronizes and reads preferences; it does not independently
    /// query WindowServer's active input state. No preference write or UI prompt.
    ///
    /// # Errors
    /// The native framework or getter is unavailable, or returned an unknown value.
    fn natural_scrolling(&self) -> Result<bool, String> {
        Err("native scroll direction is not implemented by this backend".into())
    }

    /// Apply Natural scrolling in this process's current macOS login session with
    /// the native System Settings setter, including its normal change notification.
    /// The caller also verifies the persistent global preference.
    ///
    /// # Errors
    /// The native setter is unavailable or the native preference read-back does not match.
    fn set_natural_scrolling(&self, _natural: bool) -> Result<(), String> {
        Err("native scroll direction is not implemented by this backend".into())
    }

    /// Allocate an exclusive owned directory. `None` selects the backend's temporary
    /// parent; `Some` keeps staging on the caller's filesystem. Prefer [`Scratch`]'s
    /// constructors; an implementation that cannot prove exclusive creation refuses.
    fn scratch_dir(&self, _parent: Option<&Path>, _label: &str) -> io::Result<Scratch<'_>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned scratch is not implemented",
        ))
    }

    /// Run `cmd` to completion or its deadline.
    ///
    /// # Errors
    /// [`RunFailed`] when nothing ran to completion.
    fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed>;

    /// Capture stdout as a complete bounded record. Success proves EOF, no read error
    /// and no discarded stdout bytes; stderr remains a diagnostic tail. This is not the
    /// diagnostic [`Self::run`] lane and cannot return a kept timed-out partial result.
    ///
    /// # Errors
    /// Unsupported backend, timeout, failed capture or stdout beyond the capture bound.
    fn run_complete_stdout(&self, _cmd: &Cmd) -> Result<Ran, RunFailed> {
        Err(RunFailed::Refused(
            "complete stdout is not implemented by this backend".into(),
        ))
    }

    /// Send exactly `input` through a private stdin pipe while capturing complete,
    /// bounded stdout. Input is never a file, argument, environment variable or log
    /// record. The concurrent writer shares the child's deadline and is joined before
    /// return, including when spawn or capture fails. `cmd.stdin` must be absent.
    ///
    /// # Errors
    /// Unsupported backend, incomplete input, timeout or unproven stdout capture.
    fn run_complete_stdout_input(&self, _cmd: &Cmd, _input: &[u8]) -> Result<Ran, RunFailed> {
        Err(RunFailed::Refused(
            "complete stdout with piped input is not implemented by this backend".into(),
        ))
    }

    /// Run `cmd` on this process's own terminal (standard input, output and error
    /// inherited, no deadline): for the two prompts a person answers at enrollment,
    /// OpenSSH's password and Apple's sudo. Returns the exit status.
    ///
    /// # Errors
    /// [`RunFailed`] when the program could not be started.
    fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed>;

    /// Whether this process has a controlling terminal a person can answer a prompt on
    /// ([`Self::run_tty`]'s `sudo`): `/dev/tty` opens for reading and writing. `false` for
    /// a backend with none.
    fn has_tty(&self) -> bool {
        false
    }

    /// Run `cmd` to completion or its deadline, as [`System::run`] does, with `input` and a
    /// newline on its standard input: a pipe written and closed before the child starts,
    /// never a file, an argument or a variable (`enroll` feeds the admin's password to `ssh`
    /// and `sudo` so). `cmd.stdin` is not used.
    ///
    /// # Errors
    /// [`RunFailed`] when nothing ran to completion.
    fn run_fed(&self, cmd: &Cmd, input: &Secret) -> Result<Ran, RunFailed> {
        let _ = (cmd, input);
        Err(RunFailed::Refused("run_fed: not supported".into()))
    }

    /// Ask the person at this process's controlling terminal (`/dev/tty`, never standard
    /// input): write `prompt`, read one line, with the terminal's echo off when `secret`.
    /// `None` when they typed nothing (an empty line, or the end of input).
    ///
    /// # Errors
    /// No terminal, or the read failed; [`io::ErrorKind::Interrupted`] when they stopped
    /// the prompt (Ctrl-C).
    fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
        let _ = (prompt, secret);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// The bytes of `path`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// Read one bounded regular file through the descriptor whose metadata was
    /// admitted. The final path must not be a link; ownership must match `uid`,
    /// set-id/group/other write bits are refused, and `private` also refuses every
    /// group/other permission. File contents never belong in admission errors.
    ///
    /// # Errors
    /// Unavailable safe reads, unsafe metadata, an I/O failure, or more than `max`
    /// bytes. A backend without this operation refuses rather than using read_file.
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

    /// Write `bytes` to `path` atomically (a sibling temporary, then a rename), with
    /// `mode`, as this process's own user. Never follows a link at `path`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn write_file(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;

    /// Write `bytes` to `path` as `user` (the agent re-runs itself as that user, so a link
    /// the user planted can never redirect a root write). `path` must be inside the user's
    /// home.
    ///
    /// # Errors
    /// The underlying I/O error, or a refusal as [`io::ErrorKind::PermissionDenied`].
    fn write_user_file(&self, user: &User, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()>;

    /// Make directory-entry changes in `path` durable before later writes.
    ///
    /// # Errors
    /// The directory cannot be opened or flushed. An unsupported backend refuses.
    fn durable_dir(&self, path: &Path) -> io::Result<()> {
        let _ = path;
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Remove `path` (a file or an empty directory); absent is success.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Create `path` and any missing parents with `mode`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn create_dir_all(&self, path: &Path, mode: u32) -> io::Result<()>;

    /// `chown(path, uid, gid)`, never following a link.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn chown(&self, path: &Path, uid: u32, gid: u32) -> io::Result<()>;

    /// Give the regular file at `path` the owner `uid:gid` and the permission bits `mode`,
    /// through one descriptor opened without following a link. Anything but a regular file
    /// with exactly one link is refused: in a directory others may write (stock macOS's
    /// `/Library/Application Support/CrashReporter` is `root:admin 0775`), a name swapped
    /// for a link between a check and a path-based `chown` would hand that owner and mode
    /// to another file.
    ///
    /// # Errors
    /// The underlying I/O error, or [`io::ErrorKind::InvalidInput`] for a refused file.
    fn set_owner_and_mode(&self, path: &Path, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
        let _ = (path, uid, gid, mode);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// `lstat(path)`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn stat(&self, path: &Path) -> io::Result<Stat>;

    /// The names in directory `path`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn list_dir(&self, path: &Path) -> io::Result<Vec<String>>;

    /// The SHA-256 of the file at `path`, lowercase hex, read in a stream (artifacts run to
    /// hundreds of megabytes). Never follows a link at `path`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn sha256(&self, path: &Path) -> io::Result<String>;

    /// Copy the file `from` to `to` (through a sibling temporary and a rename) with `mode`.
    /// Never follows a link at either end.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn copy_file(&self, from: &Path, to: &Path, mode: u32) -> io::Result<()>;

    /// `rename(from, to)`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Atomically rename `from` to an absent `to`. Any existing destination,
    /// including an empty directory or a dangling link, is preserved.
    /// Backends must provide an exclusive OS primitive, never a prior existence
    /// check followed by an ordinary rename.
    ///
    /// # Errors
    /// The underlying I/O error, [`io::ErrorKind::AlreadyExists`] on collision,
    /// or [`io::ErrorKind::Unsupported`] when atomic exclusion is unavailable.
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let _ = (from, to);
        Err(io::ErrorKind::Unsupported.into())
    }

    /// This process's effective uid.
    fn euid(&self) -> u32;

    /// Unix seconds now.
    fn now(&self) -> u64;

    /// Create `path` holding `bytes` with `mode`, failing with
    /// [`io::ErrorKind::AlreadyExists`] when anything (a link included) is there: the
    /// exclusive create a lock file needs.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn create_new(&self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        let _ = (path, bytes, mode);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Whether `pid` names a live process (a stale lock's holder is dead).
    fn pid_alive(&self, pid: u32) -> bool {
        let _ = pid;
        false
    }

    /// Retain a child and offer a stdin start gate. The executable must cooperatively
    /// await that gate before effects; this does not suspend arbitrary child code.
    ///
    /// # Errors
    /// Unsupported backend, invalid request or spawn failure. No child is returned by PID alone.
    fn stage_detached(
        &self,
        _cmd: &Cmd,
        _log: &Path,
    ) -> Result<Box<dyn StagedChild + '_>, RunFailed> {
        Err(RunFailed::Refused(
            "staged child is not implemented by this backend".into(),
        ))
    }

    /// Await the parent's one-byte start gate before admitting a run-job's actions.
    /// This is lifecycle gating, not a replacement for envelope/holder authority.
    ///
    /// # Errors
    /// Missing/invalid commit, timeout, I/O failure or unsupported backend.
    fn await_start(&self, _limit: Duration) -> Result<(), RunFailed> {
        Err(RunFailed::Refused(
            "start gate is not implemented by this backend".into(),
        ))
    }

    /// Start `cmd` detached from this process: a session of its own (`setsid`), standard
    /// input closed, standard output and error appended to `log`, no deadline. Returns
    /// its pid. The agent's run job (design §4.4) outlives the SSH session this way.
    ///
    /// # Errors
    /// [`RunFailed`] when it could not be started.
    fn spawn_detached(&self, cmd: &Cmd, log: &Path) -> Result<u32, RunFailed> {
        let _ = (cmd, log);
        Err(RunFailed::Refused("spawn_detached: not supported".into()))
    }

    /// Make `link` a symbolic link to `target`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let _ = (target, link);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// The target of the symbolic link at `path`.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Resolve a local filesystem target, including app-bundle aliases. Test
    /// backends may leave this unsupported and compare their injected paths.
    ///
    /// # Errors
    /// The path cannot be resolved or this backend cannot inspect the filesystem.
    fn canonicalize(&self, _path: &Path) -> io::Result<PathBuf> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Write everything `from` yields to `path` atomically with `mode`, like
    /// [`System::write_file`], without holding it all in memory (an artifact runs to
    /// hundreds of megabytes). Returns the byte count.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn write_stream(&self, path: &Path, from: &mut dyn io::Read, mode: u32) -> io::Result<u64> {
        let mut bytes = Vec::new();
        from.read_to_end(&mut bytes)?;
        self.write_file(path, &bytes, mode)?;
        Ok(bytes.len() as u64)
    }

    /// Hold an exclusive advisory lock (`flock`) on `path` (created 0600 when missing, never
    /// through a link) until the returned guard drops, waiting at most `limit` for another
    /// holder: the critical section in which the agent's run lock is taken, handed and
    /// released, so two of them never interleave. The default holds nothing (a test arm
    /// runs one thread).
    ///
    /// # Errors
    /// The underlying I/O error, or [`io::ErrorKind::TimedOut`] past `limit`.
    fn exclusive(&self, path: &Path, limit: Duration) -> io::Result<Box<dyn std::any::Any>> {
        let _ = (path, limit);
        Ok(Box::new(()))
    }

    /// `screen`'s local end (design §6): listen on a free port on `127.0.0.1` and run `cmd`
    /// once for each connection made to it, with that connection as the run's standard input
    /// and output and standard error discarded. A run lasts as long as its connection (no
    /// deadline: `cmd`'s budget is not used), and the handle's close or drop ends them all.
    ///
    /// # Errors
    /// [`RunFailed::Refused`] for a `cmd` a connection cannot carry (a relative program, a
    /// run as another user or in a session, an input file, a run kept to its deadline);
    /// [`RunFailed::Spawn`] when the listener could not be made.
    fn relay(&self, cmd: &Cmd) -> Result<Box<dyn Relaying + '_>, RunFailed> {
        let _ = cmd;
        Err(RunFailed::Refused("relay: not supported".into()))
    }

    /// Lock the screen of the login session this process runs in, as the Apple menu's Lock
    /// Screen does, whatever the screen-lock setting (design §6): [`Live`] asks loginwindow
    /// through login.framework (`port/screen_lock.rs`). Only a process inside that session
    /// (`launchctl asuser`) reaches its loginwindow. The default locks nothing; the test arm
    /// records the call.
    ///
    /// # Errors
    /// One line when loginwindow did not take the lock.
    fn lock_screen(&self) -> Result<(), String> {
        Err("lock_screen: not supported".into())
    }

    /// Until the handle drops, note SIGINT, SIGTERM and SIGHUP ([`System::stopped`]) instead
    /// of ending this process, so `screen` can close its relay and lock the session before it
    /// exits (design §6); the drop puts back the dispositions it replaced. [`Live`] sets them
    /// for the whole process (`port/stops.rs`), so one flow holds them at a time. The default
    /// catches nothing; the test arm records the catch and its end.
    fn catch_stops(&self) -> Box<dyn std::any::Any> {
        Box::new(())
    }

    /// The first signal [`System::catch_stops`] noted while its handle lives. The default:
    /// none.
    fn stopped(&self) -> Option<i32> {
        None
    }

    /// Ignore SIGHUP and SIGPIPE from here on, for the whole process: the host's screen
    /// watcher (`crate::agent::watch`) outlives the run that started it, and neither a hang-up
    /// nor a log it cannot write may end it before it locks. [`Live`] sets both to `SIG_IGN`
    /// (`port/stops.rs`); the default ignores nothing; the test arm records the call.
    fn ignore_hangups(&self) {}

    /// This Mac's own `IOPlatformUUID` on the live system; `None` in tests.
    fn own_uuid(&self) -> Option<String> {
        None
    }

    /// The converge journal of the account this system acts as, which records each change
    /// before it is made (docs/DESIGN-init-system-2026-10-20.md §3.3): an item's journal
    /// goes where its account is, so it comes from the same port the items act through.
    /// [`Live`] keeps it under the account's state root; the test arm keeps it in its own
    /// files, so a run over it can never write the journal of the account running the
    /// test. The default keeps none, and every change is refused on it.
    ///
    /// # Errors
    /// No journal can be kept (the account has no state root; this system keeps none).
    fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + '_>, String> {
        Err("this system keeps no converge journal".into())
    }
}

pub use crate::model::User;

/// The live operating system.
#[derive(Debug, Clone)]
pub struct Live {
    /// The agent executable, which [`System::write_user_file`] re-runs as the user.
    pub agent_exe: PathBuf,
}

impl Live {
    /// The live port, with the running executable as the agent.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agent_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/nonexistent")),
        }
    }

    /// The argv `cmd` really runs: wrapped in `sudo -u … env -i …` and `launchctl asuser`
    /// as asked (design §3.5).
    fn argv(&self, cmd: &Cmd) -> Result<(PathBuf, Vec<OsString>), RunFailed> {
        Self::argv_from(self.euid(), cmd)
    }

    /// What this port spawns for `cmd`: the program, its arguments and its whole
    /// environment, for a test's spawn record.
    ///
    /// # Errors
    /// As [`Live::argv`].
    #[cfg(test)]
    pub(crate) fn spawned(&self, cmd: &Cmd) -> Result<Spawned, RunFailed> {
        let (program, args) = self.argv(cmd)?;
        Ok((program, args, self.child_env(cmd)))
    }

    /// [`Live::argv`] for a process whose effective uid is `euid`: a function of the
    /// two alone, so both the root wrapping and the refusal without root are checked
    /// on whatever account runs the tests.
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

    /// The whole environment a child of `cmd` starts with (it is cleared first), then
    /// [`Cmd::env`].
    ///
    /// A root process never passes on the HOME it was given (design §3.4): Apple's sudoers
    /// keeps the caller's (`env_keep += "HOME MAIL"`), so under a password sudo root would
    /// hand the admin's home to every tool. Root's children get root's own. A run as
    /// another account from root carries [`account_env`] in the `env -i` list
    /// [`Live::argv`] builds (sudo's env_reset drops what is set here). A run as this
    /// process's own account, which `argv` leaves unwrapped, gets that same list here, so
    /// it sees what it would see wrapped (`git config --global` needs HOME).
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

    /// [`System::run`] for a [`Cmd::keep_on_timeout`] run. Standard output and error go to
    /// private files rather than pipes, so what the tool printed outlives the kill at the
    /// deadline (`aterm_uds::deadline` answers a timed-out run with nothing). The child leads
    /// its own process group, as there, and at the deadline the whole group is killed: run as
    /// an account from root, the child is `sudo` (or `launchctl asuser`) and the tool its
    /// descendant, which killing the child alone would leave running.
    fn run_until_deadline(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let failed = |e: io::Error| RunFailed::Spawn(e.to_string());
        let (program, args) = self.argv(cmd)?;
        let out = staging_file(self, b"").map_err(failed)?;
        let err = staging_file(self, b"").map_err(failed)?;
        let open = |staged: &Staged<'_>| {
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(staged.path())
                .map_err(failed)
        };
        let mut child = fixed_command(&program, &args, &self.child_env(cmd));
        let stdin = match &cmd.stdin {
            Some(path) => aterm_uds::spawnfd::Stdio::from(
                std::fs::File::open(path)
                    .map_err(|e| RunFailed::Spawn(format!("{}: {e}", path.display())))?,
            ),
            None => aterm_uds::spawnfd::Stdio::null(),
        };
        child
            .process_group(0)
            .stdin(stdin)
            .stdout(open(&out)?)
            .stderr(open(&err)?);
        let mut child = child.spawn().map_err(failed)?;
        let deadline = std::time::Instant::now() + cmd.budget;
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
                    return Err(RunFailed::Spawn(e.to_string()));
                }
            }
        };
        Ok(Ran {
            status,
            stdout: kept_tail(out.path()).map_err(failed)?,
            stderr: kept_tail(err.path()).map_err(failed)?,
        })
    }
}

/// The environment a run as `user` starts with, wrapped or not: the account's variables,
/// then the fixed `PATH` and locale. Its login shell is `/bin/zsh` (macOS's default) when
/// none was read.
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

/// `SIGKILL` the process group `child` leads (it was spawned with `process_group(0)`) and
/// the child itself, then reap it: what `aterm_uds::deadline` does at a deadline.
fn end_group(child: &mut aterm_uds::spawnfd::Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: a plain syscall. The child is not yet reaped, so its pid, and with it the
        // group id it leads, cannot have been reused.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The last [`aterm_uds::deadline::OUTPUT_KEEP`] bytes of the file at `path`, as a piped run
/// keeps them; only those are read, whatever the tool wrote before them.
fn kept_tail(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _};
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let keep = aterm_uds::deadline::OUTPUT_KEEP as u64;
    let len = file.metadata()?.len();
    file.seek(io::SeekFrom::Start(len.saturating_sub(keep)))?;
    let mut bytes = Vec::new();
    file.take(keep).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `program` with `args` in a cleared environment holding only `env` ([`Live::child_env`]),
/// with `/` as the working directory. [`Live::run`] and [`System::relay`]'s connections
/// share it.
fn fixed_command(
    program: &Path,
    args: &[OsString],
    env: &[(String, String)],
) -> aterm_uds::spawnfd::Command {
    let mut child = aterm_uds::spawnfd::command(program);
    child.args(args).env_clear().current_dir("/");
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

impl System for Live {
    fn github_api(
        &self,
        user: &User,
        token: &Secret,
        endpoint: &str,
    ) -> Result<GithubResponse, String> {
        if self.euid() != user.uid {
            return Err("GitHub reads must run as their local user".into());
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

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
    fn natural_scrolling(&self) -> Result<bool, String> {
        scroll_direction::get()
    }

    fn set_natural_scrolling(&self, natural: bool) -> Result<(), String> {
        scroll_direction::set(natural)
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
        let mut child = fixed_command(&program, &args, &self.child_env(cmd));
        let stdin = match &cmd.stdin {
            Some(path) => aterm_uds::spawnfd::Stdio::from(
                std::fs::File::open(path)
                    .map_err(|e| RunFailed::Spawn(format!("{}: {e}", path.display())))?,
            ),
            None => aterm_uds::spawnfd::Stdio::null(),
        };
        let deadline = std::time::Instant::now() + cmd.budget;
        match aterm_uds::deadline::output_until_from(&mut child, stdin, deadline) {
            Ok(out) => Ok(Ran {
                status: out.status.code(),
                stdout: out.stdout,
                stderr: out.stderr,
            }),
            Err(aterm_uds::deadline::RunError::TimedOut) => Err(RunFailed::TimedOut(cmd.budget)),
            Err(
                aterm_uds::deadline::RunError::Spawn(e) | aterm_uds::deadline::RunError::Wait(e),
            ) => Err(RunFailed::Spawn(e.to_string())),
        }
    }

    fn run_complete_stdout(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        use aterm_uds::deadline::{CompleteOutputError, RunError};
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
        let mut child = fixed_command(&program, &args, &self.child_env(cmd));
        // Existing stdin admission policy is unchanged: opening a caller-supplied
        // pathname precedes the child deadline. Complete capture is not an I/O deadline.
        let stdin = match &cmd.stdin {
            Some(path) => aterm_uds::spawnfd::Stdio::from(
                std::fs::File::open(path)
                    .map_err(|e| RunFailed::Spawn(format!("{}: {e}", path.display())))?,
            ),
            None => aterm_uds::spawnfd::Stdio::null(),
        };
        let deadline = std::time::Instant::now() + cmd.budget;
        let out =
            aterm_uds::deadline::output_until_complete_stdout_from(&mut child, stdin, deadline)
                .map_err(|error| match error {
                    CompleteOutputError::Run(RunError::TimedOut) => RunFailed::TimedOut(cmd.budget),
                    CompleteOutputError::Run(RunError::Spawn(e) | RunError::Wait(e)) => {
                        RunFailed::Spawn(e.to_string())
                    }
                    CompleteOutputError::StdoutRead(e) => {
                        RunFailed::Refused(format!("complete stdout read failed: {e}"))
                    }
                    CompleteOutputError::StdoutTooLarge { limit } => {
                        RunFailed::Refused(format!("complete stdout exceeds {limit} bytes"))
                    }
                    CompleteOutputError::StdoutIncomplete => {
                        RunFailed::Refused("complete stdout did not reach EOF".into())
                    }
                })?;
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
        let status = child
            .args(&args)
            .stdin(aterm_uds::spawnfd::Stdio::inherit())
            .stdout(aterm_uds::spawnfd::Stdio::inherit())
            .stderr(aterm_uds::spawnfd::Stdio::inherit())
            .status()
            .map_err(|e| RunFailed::Spawn(e.to_string()))?;
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
        let mut child = fixed_command(&program, &args, &self.child_env(cmd));
        // Written whole before the child starts, so no thread is needed: a pipe holds 16 KiB
        // at least and a secret is at most SECRET_MAX. The writer's close is the child's end
        // of input.
        let (reader, mut writer) = io::pipe().map_err(|e| RunFailed::Spawn(e.to_string()))?;
        writer
            .write_all(input.bytes())
            .and_then(|()| writer.write_all(b"\n"))
            .map_err(|e| RunFailed::Spawn(format!("the input pipe: {e}")))?;
        drop(writer);
        let deadline = std::time::Instant::now() + cmd.budget;
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
            Err(aterm_uds::deadline::RunError::TimedOut) => Err(RunFailed::TimedOut(cmd.budget)),
            Err(
                aterm_uds::deadline::RunError::Spawn(e) | aterm_uds::deadline::RunError::Wait(e),
            ) => Err(RunFailed::Spawn(e.to_string())),
        }
    }

    fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
        secret::ask_tty(prompt, secret)
    }

    fn has_tty(&self) -> bool {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .is_ok()
    }

    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
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
        if !path.starts_with(&user.home) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is outside {}'s home", path.display(), user.name),
            ));
        }
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

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
            Ok(m) if m.is_dir() => std::fs::remove_dir(path),
            Ok(_) => std::fs::remove_file(path),
        }
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
        // O_NONBLOCK: a FIFO swapped in opens at once, and is refused below, instead of
        // waiting for a writer.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
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
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
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
            .custom_flags(libc::O_NOFOLLOW)
            .open(from)?;
        let mut dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
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
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        f.write_all(bytes)?;
        f.set_permissions(std::fs::Permissions::from_mode(mode))?;
        // Ordered: the run lock is taken over once its holder is dead, which no reboot
        // leaves alive; a staging file is consumed at once; a placeholder is empty.
        aterm_durable::ordered(&f)
    }

    fn pid_alive(&self, pid: u32) -> bool {
        aterm_uds::process::pid_alive(pid)
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
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(log)
                .map_err(|e| RunFailed::Spawn(format!("{}: {e}", log.display())))
        };
        let mut child = aterm_uds::spawnfd::command(&program);
        child
            .args(&args)
            .env_clear()
            .current_dir("/")
            .setsid(true)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::from(open_log()?))
            .stderr(aterm_uds::spawnfd::Stdio::from(open_log()?));
        for (key, value) in self.child_env(cmd) {
            child.env(key, value);
        }
        let started = child.spawn().map_err(|e| RunFailed::Spawn(e.to_string()))?;
        // The handle is dropped unreaped on purpose: the job outlives this process, and
        // aterm-uds's reaper collects it should it end first.
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
            .custom_flags(libc::O_NOFOLLOW)
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
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        Ok(Box::new(Exclusive::within(file, limit, path)?))
    }

    fn own_uuid(&self) -> Option<String> {
        let ran = self
            .run(&Cmd::new("/usr/sbin/ioreg").args(["-rd1", "-c", "IOPlatformExpertDevice"]))
            .ok()?;
        crate::agent::platform::ioreg_value(&ran.out(), "IOPlatformUUID")
    }

    /// `<state_dir>/converge/journal.ndjson` under [`aterm_types::dirs::account_state_dir`]:
    /// the state root the process's items act on (a private state root as it is, else
    /// `$HOME`'s), and for root its own account record's home, since `sudo aterm machines
    /// here apply` may keep the invoking person's `$HOME`.
    fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + '_>, String> {
        aterm_types::dirs::account_state_dir()
            .map(|root| {
                Box::new(aterm_converge::FileJournal::under(&root))
                    as Box<dyn aterm_converge::Journal>
            })
            .ok_or_else(|| "this account has no state directory to journal changes in".into())
    }
}

/// [`Live::exclusive`]'s guard: released by `LOCK_UN` when dropped, before the close
/// (clippy.toml: the flock release rule).
struct Exclusive(std::fs::File);

impl Exclusive {
    /// Take `file`'s exclusive lock, re-trying every 20 ms for up to `limit`.
    fn within(file: std::fs::File, limit: Duration, what: &Path) -> io::Result<Self> {
        // The lock-order census's File evidence (as atpkg's `lock::Flock`).
        let file: std::fs::File = file;
        let deadline = std::time::Instant::now() + limit;
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

/// Write `bytes` to a fresh 0600 file in an exclusively owned temporary directory.
///
/// # Errors
/// The underlying I/O error. Only this call's allocation is cleaned on failure.
pub fn staging_file<'s>(sys: &'s dyn System, bytes: &[u8]) -> io::Result<Staged<'s>> {
    let scratch = Scratch::new(sys, "stdin")?;
    let path = scratch.path().join("payload");
    sys.create_new(&path, bytes, 0o600)?;
    Ok(Staged {
        path,
        _scratch: scratch,
    })
}

/// Write `bytes` to `path` through a sibling temporary and a rename, with `mode`. A link
/// at `path` is replaced, never followed; a link at the temporary's name fails the open.
/// Flush the payload before rename and the destination directory after it. Callers
/// that create ancestors must also publish those directory entries before relying on
/// the new name surviving a power interruption. A flush error can leave `path` replaced.
///
/// # Errors
/// The underlying I/O error.
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
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    aterm_durable::durable(&f)?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    aterm_durable::durable_dir(parent)
}

#[cfg(test)]
pub use fake::Fake;

#[cfg(test)]
mod fake {
    use super::scratch::{OwnedDirectory, prefix};
    use super::{
        Cmd, GithubResponse, Ran, Relaying, RunFailed, Scratch, Secret, StagedChild, Stat, System,
        User,
    };
    use std::collections::BTreeMap;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    /// A tool a test models from its own state ([`Fake::simulate`]).
    type Model = Arc<dyn Fn(&Fake, &Cmd) -> Option<Ran> + Send + Sync>;

    /// The scripted operating system tests run items against.
    ///
    /// A reply is keyed by [`Cmd::display`] (`<user>$ ` prefixed for `as_user`). Replies
    /// for one key are consumed in order and the last one repeats. A run with no scripted
    /// reply fails as [`RunFailed::Spawn`], so a test notices a command it did not expect.
    #[derive(Default)]
    pub struct Fake {
        /// The tools a test models instead of scripting ([`Fake::simulate`]).
        model: Mutex<Option<Model>>,
        scroll_natural: Mutex<Option<bool>>,
        scroll_set_error: Mutex<Option<String>>,
        scroll_set_readback: Mutex<Option<bool>>,
        replies: Mutex<BTreeMap<String, Vec<Ran>>>,
        github_replies: Mutex<BTreeMap<String, Vec<Result<GithubResponse, String>>>>,
        outputs: Mutex<BTreeMap<String, Vec<Output>>>,
        strict_refusals: Mutex<BTreeMap<String, String>>,
        stage_commit_refused: Mutex<bool>,
        stage_abort_refused: Mutex<bool>,
        start_answers: Mutex<std::collections::VecDeque<Result<(), RunFailed>>>,
        scratch_sequence: Mutex<u64>,
        scratch_close_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        /// Shared with [`System::catch_stops`]'s handle, which records its end.
        calls: Arc<Mutex<Vec<String>>>,
        files: Mutex<BTreeMap<PathBuf, (Vec<u8>, u32)>>,
        stats: Mutex<BTreeMap<PathBuf, Stat>>,
        stat_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        directory_flush_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        links: Mutex<BTreeMap<PathBuf, PathBuf>>,
        alive: Mutex<Vec<u32>>,
        /// The effective uid the fake reports (0 = root, the agent's case).
        pub euid: u32,
        /// The clock.
        pub now: u64,
        /// The pid [`System::spawn_detached`] answers.
        pub next_pid: u32,
        /// The port [`System::relay`] answers; 0 fails it.
        pub relay_port: u16,
        /// What [`System::lock_screen`] answers: `None` takes the lock, `Some(why)` refuses
        /// it. Either way the call is recorded as `lock-screen`, and nothing is locked.
        pub lock_refused: Option<String>,
        /// How many [`System::stopped`] readings answer none before every later one answers
        /// SIGINT, as if the person pressed Ctrl-C there; `None`: never.
        pub stop_after: Option<usize>,
        /// The [`System::stopped`] readings so far.
        stop_readings: Mutex<usize>,
        /// What [`System::ask`] answers, in order ([`Fake::answer`]).
        answers: Mutex<std::collections::VecDeque<Option<String>>>,
        /// Each [`System::run_fed`] run's key and the bytes it was fed ([`Fake::fed`]).
        fed: Mutex<Vec<(String, Vec<u8>)>>,
        /// What [`System::has_tty`] answers: a person's terminal is there.
        pub tty: bool,
    }

    #[derive(Clone)]
    enum Output {
        File(PathBuf, Vec<u8>),
        Stat(PathBuf, Stat),
    }

    /// [`Fake`]'s converge journal: each line appended to [`Fake::JOURNAL`] in its files.
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

        /// The fake account's converge journal ([`System::journal`]), a file of the fake's
        /// own: each change's line is appended there, read back with [`Fake::written`].
        pub const JOURNAL: &'static str =
            "/var/root/Library/Application Support/aterm/converge/journal.ndjson";

        /// A fake running as root at a fixed time.
        #[must_use]
        pub fn root() -> Self {
            Self {
                now: 1_800_000_000,
                ..Self::default()
            }
        }

        /// The deterministic candidate used only by this in-memory backend. Fixtures
        /// script external-tool output after allocation, never pre-adopt a real path.
        #[must_use]
        pub fn scratch_path(parent: Option<&Path>, label: &str, sequence: u64) -> PathBuf {
            parent.unwrap_or_else(|| Path::new("/tmp")).join(format!(
                "{}{sequence}",
                prefix(label).expect("fixture scratch label")
            ))
        }

        /// Have the scripted command create its output when it runs, after the caller
        /// has allocated its scratch. No host filesystem or command is involved.
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

        /// Queue an explicit child-side gate observation. No answer means refusal,
        /// never a real stdin read or a grant inferred from a fake spawned PID.
        pub fn start_answer(&self, answer: Result<(), RunFailed>) -> &Self {
            self.start_answers.lock().unwrap().push_back(answer);
            self
        }

        /// Queue what the next [`System::ask`] answers: `Some(line)` as typed, `None` for
        /// nothing typed. An ask with nothing queued fails, so a test notices a prompt it
        /// did not expect.
        pub fn answer(&self, line: Option<&str>) -> &Self {
            self.answers
                .lock()
                .unwrap()
                .push_back(line.map(str::to_owned));
            self
        }

        /// Every [`System::run_fed`] run so far: its key (`fed <display>`, the key its reply
        /// is scripted under) and what it was fed. Only here does a fed secret show: the
        /// run's display, which [`Fake::calls`] records, never holds it.
        #[must_use]
        pub fn fed(&self) -> Vec<(String, Vec<u8>)> {
            self.fed.lock().unwrap().clone()
        }

        /// Answer every command `model` answers (`Some`) from the test's own state, before
        /// any scripted reply: a desktop whose `defaults`, `pgrep`, `killall` and `sleep` act
        /// on one another. It is handed this fake, so it can start and end processes
        /// ([`Fake::alive`], [`Fake::exited`]). Each run is still recorded in
        /// [`Fake::calls`]; one it leaves (`None`) gets its scripted reply.
        pub fn simulate(
            &self,
            model: impl Fn(&Self, &Cmd) -> Option<Ran> + Send + Sync + 'static,
        ) -> &Self {
            *self.model.lock().unwrap() = Some(Arc::new(model));
            self
        }

        /// Mark `pid` as a process that has exited.
        pub fn exited(&self, pid: u32) -> &Self {
            self.alive.lock().unwrap().retain(|alive| *alive != pid);
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

        /// Script a GitHub read without retaining its credential or opening a socket.
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
                .push(reply);
            self
        }

        /// Script a controller REST request (`github <METHOD> <endpoint>`), without
        /// retaining its credential or opening a socket. [`Fake::calls`] records it with its
        /// JSON body, when it has one, after the endpoint.
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
                .push(reply);
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

        /// What the commands scripted under `key` leave behind ([`Fake::on_write`],
        /// [`Fake::on_stat`]), once one of them ran.
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

        /// Seed a symbolic link.
        pub fn link(&self, path: &str, target: &str) -> &Self {
            self.links
                .lock()
                .unwrap()
                .insert(PathBuf::from(path), PathBuf::from(target));
            self
        }
    }

    impl System for Fake {
        fn github_api(
            &self,
            user: &User,
            _token: &Secret,
            endpoint: &str,
        ) -> Result<GithubResponse, String> {
            let key = format!("{}$ agh GET {endpoint}", user.name);
            self.calls.lock().unwrap().push(key.clone());
            let mut replies = self.github_replies.lock().unwrap();
            match replies.get_mut(&key) {
                Some(list) if list.len() > 1 => list.remove(0),
                Some(list) if list.len() == 1 => list[0].clone(),
                _ => Err("fake: no GitHub response was scripted".into()),
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
                Some(list) if list.len() > 1 => list.remove(0),
                Some(list) if list.len() == 1 => list[0].clone(),
                _ => Err(format!("fake: no GitHub response was scripted for `{key}`")),
            }
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

        fn scratch_dir(&self, parent: Option<&Path>, label: &str) -> io::Result<Scratch<'_>> {
            prefix(label)?;
            let mut sequence = self.scratch_sequence.lock().unwrap();
            for _ in 0..5 {
                let path = Self::scratch_path(parent, label, *sequence);
                *sequence = sequence
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("fake scratch sequence exhausted"))?;
                // A subtree is occupancy too: Fake fixtures must not seed unowned
                // contents and then pretend their absent directory was just allocated.
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
            // A scripted Fake vector models the entire stream, not a captured tail.
            // Only Live's strict helper establishes real pipe EOF/no loss.
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
            // No input copy is retained, even by the injected backend. The ordinary
            // command key scripts the complete reply; calls records only that key.
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
            match self.answers.lock().unwrap().pop_front() {
                Some(Some(line)) => Secret::new(line.as_bytes()).map(Some),
                Some(None) => Ok(None),
                None => Err(io::Error::other(format!(
                    "fake: no answer scripted for `{prompt}`"
                ))),
            }
        }

        fn has_tty(&self) -> bool {
            self.tty
        }

        fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed> {
            let key = format!("tty {}", cmd.display());
            self.calls.lock().unwrap().push(key.clone());
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

        fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .map(|(b, _)| b.clone())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
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
            let files = self.files.lock().unwrap();
            let (bytes, _) = files.get(path).ok_or(io::ErrorKind::NotFound)?;
            if bytes.len() > max {
                return Err(io::ErrorKind::InvalidData.into());
            }
            Ok(bytes.clone())
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
            if !path.starts_with(&user.home) {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
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
            let files = self.files.lock().unwrap();
            let mut names: Vec<String> = files
                .keys()
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
            // A directory's rename carries the files below it.
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
            if [from, to]
                .iter()
                .any(|p| p.as_os_str().as_encoded_bytes().contains(&0))
            {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            // Hold all modeled filesystem maps across both admission and movement.
            // A competing exclusive publisher cannot observe a check/move gap.
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

        fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + '_>, String> {
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

        fn stopped(&self) -> Option<i32> {
            let mut readings = self.stop_readings.lock().unwrap();
            *readings += 1;
            self.stop_after
                .is_some_and(|after| *readings > after)
                .then_some(libc::SIGINT)
        }

        fn ignore_hangups(&self) {
            self.calls.lock().unwrap().push("ignore hangups".into());
        }
    }

    /// [`Fake::catch_stops`]'s handle: catches nothing, records its end as `stops restored`.
    struct Caught(Arc<Mutex<Vec<String>>>);

    impl Drop for Caught {
        fn drop(&mut self) {
            self.0.lock().unwrap().push("stops restored".into());
        }
    }

    /// [`Fake::relay`]'s handle: listens on nothing, records its close as `relay closed`.
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
        // Only the argv shape is checked here, for a root euid on any account; nothing runs.
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
        // Its own account it runs unwrapped.
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
            // From root the run is wrapped in sudo to an account that does not exist; the
            // argv tests above check that wrapping on any account.
            aterm_uds::host::could_not_run(
                "as root a run as this account is wrapped; the unwrapped run needs a non-root uid",
            );
        }
        // Our own uid under a name and home of the test's making: what the child sees must
        // come from the account, not from this process's environment, and be the list a
        // run wrapped from root carries (the argv test above).
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

    /// A new run lock, staging file or placeholder is only ordered (aterm-durable's
    /// counting seam); a copied artifact stays durable.
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

    #[test]
    fn the_live_port_runs_an_absolute_tool_and_keeps_its_output() {
        let live = Live::new();
        let ran = live.run(&Cmd::new("/bin/echo").arg("hello")).unwrap();
        assert!(ran.success());
        assert_eq!(ran.out(), "hello\n");
    }

    #[test]
    fn a_run_kept_to_its_deadline_is_ended_there_and_answers_rather_than_failing() {
        let live = Live::new();
        let ran = live
            .run(
                &Cmd::new("/bin/sleep")
                    .arg("60")
                    .budget(Duration::from_millis(200))
                    .until_deadline(),
            )
            .unwrap();
        assert_eq!(ran.status, None, "{ran:?}");
    }

    #[test]
    fn a_run_kept_to_its_deadline_that_ends_sooner_keeps_its_status_and_output() {
        let live = Live::new();
        let ran = live
            .run(&Cmd::new("/bin/echo").arg("hello").until_deadline())
            .unwrap();
        assert_eq!(ran.status, Some(0));
        assert_eq!(ran.out(), "hello\n");
    }

    #[test]
    fn a_run_kept_to_its_deadline_ends_what_its_child_started_too() {
        // Run as an account from root, the child is `sudo` (or `launchctl asuser`) and the
        // tool is its descendant: the deadline must end the tool, not orphan it.
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
        // Our own uid and gid: the one owner a process that is not root may give.
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
}
