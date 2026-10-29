// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Operating-system port ([`System`]), with [`Live`] and test (`Fake`) implementations.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "macos")]
mod accessibility;
mod complete_input;
mod console_session;
mod create_new;
mod github_api;
pub(crate) mod github_rest;
mod identity;
#[cfg(target_os = "macos")]
mod low_data;
mod read_owned_file;
mod relay;
mod rename_noreplace;
mod scratch;
mod screen_lock;
mod scroll_direction;
mod secret;
#[cfg(test)]
pub(crate) mod spawns;
mod staged_child;
mod stops;

pub use github_rest::RestMethod;
pub use scratch::Scratch;
pub use secret::{SECRET_MAX, Secret, no_core_dumps};
pub use staged_child::{START_BYTE, START_LIMIT, StagedChild};
pub use stops::name as stop_name;

/// Default deadline for a single tool run.
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
    /// The deadline passed; the process group was killed.
    TimedOut(Duration),
    /// The port refused the run before starting it (such as `as_user` without root).
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

    /// Stop listening, then `SIGKILL` and reap every active connection run.
    fn close(&mut self) -> Result<(), String>;
}

/// An account's service-restart lock, released when the guard drops.
pub trait ServiceRestartHold {}

impl ServiceRestartHold for aterm_update_core::FileLock {}

/// The operating system, as items see it.
pub trait System: Sync {
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

    /// Read the active session's natural scrolling preference via the native getter.
    fn natural_scrolling(&self) -> Result<bool, String> {
        Err("native scroll direction is not implemented by this backend".into())
    }

    /// Set natural scrolling in the current macOS login session via the native setter.
    fn set_natural_scrolling(&self, _natural: bool) -> Result<(), String> {
        Err("native scroll direction is not implemented by this backend".into())
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

    /// Run `cmd` with `input` and a trailing newline written to a private stdin pipe.
    fn run_fed(&self, cmd: &Cmd, input: &Secret) -> Result<Ran, RunFailed> {
        let _ = (cmd, input);
        Err(RunFailed::Refused("run_fed: not supported".into()))
    }

    /// Prompt on `/dev/tty` and read one line, disabling echo when `secret` is true.
    fn ask(&self, prompt: &str, secret: bool) -> io::Result<Option<Secret>> {
        let _ = (prompt, secret);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    /// Read the bytes of `path`.
    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>>;

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
}

impl Live {
    /// The live port, with the running executable as the agent, and Full Disk Access not measured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            agent_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/nonexistent")),
            full_disk_access: None,
        }
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
                .custom_flags(libc::O_NOFOLLOW)
                .open(staged.path())
                .map_err(failed)
        };
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
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

fn end_group(child: &mut aterm_uds::spawnfd::Child) {
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // `child` is not yet reaped, so its pid and process group id remain valid.
        let _ = aterm_uds::process::kill_group_until_empty(pid);
    }
    let _ = child.kill();
    let _ = child.wait();
}

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
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
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
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
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
        if let Some(dir) = &cmd.cwd {
            child.current_dir(dir);
        }
        let status = owning_terminal(|| {
            child
                .args(&args)
                .stdin(aterm_uds::spawnfd::Stdio::inherit())
                .stdout(aterm_uds::spawnfd::Stdio::inherit())
                .stderr(aterm_uds::spawnfd::Stdio::inherit())
                .status()
        })
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
        let mut child = fixed_command(&program, &args, &self.child_env(cmd), cmd.cwd.as_deref());
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
        owning_terminal(|| secret::ask_tty(prompt, secret))
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
        crate::agent::platform::ioreg_value(&ran.out(), "IOPlatformUUID")
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
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    aterm_durable::durable(&f)?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    aterm_durable::durable_dir(parent)
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
        files: Mutex<BTreeMap<PathBuf, (Vec<u8>, u32)>>,
        stats: Mutex<BTreeMap<PathBuf, Stat>>,
        stat_errors: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        stat_errors_once: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
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
        /// What [`System::ask`] answers, in order ([`Fake::answer`]).
        answers: Mutex<std::collections::VecDeque<Option<String>>>,
        /// Each [`System::run_fed`] run's key and the bytes it was fed ([`Fake::fed`]).
        fed: Mutex<Vec<(String, Vec<u8>)>>,
        /// What [`System::has_tty`] answers: a person's terminal is there.
        pub tty: bool,
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
        /// What [`System::low_data_mode`] answers: unknown unless a test sets it.
        pub low_data: Option<bool>,
        /// What [`System::service`] answers, one look each, the last repeating
        /// ([`Fake::services`]): none running when none is scripted.
        services: Mutex<Vec<aterm_uds::services::Look>>,
        /// A real scratch rendezvous for competing restart tests; inert unless set.
        pub service_restart_lock: Option<PathBuf>,
        /// A hook at the terminal-command seam, without executing the command.
        tty_hook: Mutex<Option<Model>>,
        /// What [`System::administrator`] answers: unread unless a test sets it.
        pub admin: Option<bool>,
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
    }

    type TccReplies =
        std::collections::BTreeMap<String, Vec<Result<Vec<crate::items::person::TccRow>, String>>>;

    type SleepHook = Arc<dyn Fn(&Fake, std::time::Duration) + Send + Sync>;

    #[derive(Clone)]
    enum Output {
        File(PathBuf, Vec<u8>),
        Stat(PathBuf, Stat),
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

        /// Recorded `(key, input)` pairs from [`System::run_fed`].
        #[must_use]
        pub fn fed(&self) -> Vec<(String, Vec<u8>)> {
            self.fed.lock().unwrap().clone()
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

    impl System for Fake {
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

        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            let links = self.links.lock().unwrap();
            let mut path = path.to_path_buf();
            for _ in 0..32 {
                let Some((link, target)) = links.iter().find(|(link, _)| path.starts_with(link))
                else {
                    return Ok(path);
                };
                let target = link.parent().unwrap_or(Path::new("/")).join(target);
                let rest = path.strip_prefix(link).unwrap_or(Path::new(""));
                path = if rest.as_os_str().is_empty() {
                    target
                } else {
                    target.join(rest)
                };
            }
            Err(io::Error::other("too many levels of symbolic links"))
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
                stop_after: self.stop_after,
            })
        }

        fn stopped(&self) -> Option<i32> {
            read_stop(&self.stop_readings, self.stop_after)
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
        stop_after: Option<usize>,
    }

    impl Drop for Deferred {
        fn drop(&mut self) {
            self.calls.lock().unwrap().push("stops restored".into());
            if let Some(sig) = read_stop(&self.readings, self.stop_after) {
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
            full_disk_access: None,
        };
        assert_eq!(live("bin/aterm").running_app(), Some(app.clone()));
        assert_eq!(
            live("Applications/aterm.app/Contents/MacOS/aterm").running_app(),
            Some(app)
        );
        assert_eq!(live("bin/built").running_app(), None, "a build is no app");
        assert_eq!(live("bin/gone").running_app(), None);
    }
}
