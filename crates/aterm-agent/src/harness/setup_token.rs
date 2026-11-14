// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The setup-token capture (`docs/DESIGN-claude-setup-token-2026-11-05.md` §4.5): Claude Code's
//! `setup-token` run for the person on the default route, on a private terminal, with its token
//! taken from what it shows and nothing else of it kept.
//!
//! - **The process.** The program runs as a session leader on a private 25 by 512 pty
//!   ([`ROWS`], [`COLS`]: no token line wraps) whose slave is its stdin, stdout, stderr and
//!   controlling terminal, so nothing it writes reaches the person's tab. Its environment is
//!   [`child_env`]: [`super::login_drive::passed_unrouted`], a fixed `PATH`, `TERM=dumb`, both
//!   Claude homes on a fresh 0700 scratch folder ([`Scratch`]) and the telemetry opt-outs; no
//!   credential, no route and no aterm session variable. Its page opens in the person's
//!   default browser (no `BROWSER`), or, where this Mac makes the token by itself, in aterm's
//!   own sign-in window ([`WindowRoute`], [`window_env`]).
//! - **The default route only.** A verb whose own environment names a route
//!   ([`super::netwatch::names_a_route`]) is refused before anything is made ([`route_in`]).
//! - **The authorize URL is never relayed.** The child opens it (by itself, or through
//!   aterm's opener, which it starts with the URL); the capture checks each
//!   authorization request the child shows ([`admit_url`]: claude.com's or claude.ai's authorize
//!   path, the `user:inference` scope alone, the setup-token lifetime where the URL names one)
//!   and ends the child at any other.
//! - **Reading.** Every byte the child writes is cleaned as it is read ([`Cleaner`]: escapes and
//!   control bytes skipped, an OSC 8 link's address kept as a line of its own) into one buffer
//!   of [`BUFFER`] bytes, made at that size and wiped when it drops; no `String` of child bytes
//!   is made, and past [`BUFFER`] without a verdict the capture stops.
//! - **The grammar is closed** ([`Judge`]): the paste prompt becomes the verb's own hidden prompt
//!   ([`Person::paste`]), or, in the sign-in window, the code the window's page ended on
//!   ([`WindowWaiter`]); success is Claude Code's success marker, then a token, then its export
//!   line, with every token-shaped value the same one; failure is one of [`Failure`]'s
//!   phrases; any other end is [`Error::NoToken`].
//! - **Errors carry no child bytes**: each is a fixed sentence ([`Error`]).

use std::ffi::OsString;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_secrets::SecretBytes;
use aterm_secrets::claude_token::{ExpiryFrom, value_grammar};
use zeroize::{Zeroize as _, Zeroizing};

use super::wake_deadline::WakeDeadline;

/// The rows of the capture's private terminal.
pub const ROWS: u16 = 25;

/// Its columns: wide enough that a token line, or the authorize URL, never wraps.
pub const COLS: u16 = 512;

/// The most of the child's output the capture holds, cleaned ([`Cleaner`]); past it, with no
/// verdict, the capture stops ([`Error::Output`]).
pub const BUFFER: usize = 256 * 1024;

/// A setup-token's lifetime as Claude Code asks for it (365 days), and the rule for a
/// token whose lifetime it did not print: mint time plus this.
pub const LIFETIME: u64 = 31_536_000;

/// The scratch folders' name before `<pid>-<birth>` of the process that made each ([`Scratch`]).
pub const SCRATCH_PREFIX: &str = "setup-token-";

/// The child's whole `PATH`: the system's own folders, where macOS's `open` (which Claude Code
/// opens the browser with) is.
pub const CHILD_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// What the child is told so it reports and phones home nothing it need not.
pub const OPT_OUTS: [(&str, &str); 3] = [
    ("DISABLE_ERROR_REPORTING", "1"),
    ("DISABLE_TELEMETRY", "1"),
    ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
];

/// How long the child is given to exit once its success frame was read (it exits by itself
/// 500 ms after it); then its group is killed. Never past the capture's deadline.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// How long one wait for the child's output lasts at most, so a stop, a paste and both clocks
/// are looked at this often.
const READ_TICK: Duration = Duration::from_millis(100);

/// What the verb asks the person while Claude Code waits for a pasted code.
pub const PASTE_PROMPT: &str =
    "if the browser shows a code to paste, paste it here (it is not shown as you type): ";

/// Why a capture made no token. Each is a fixed sentence: none holds a byte the child wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The verb's environment names a route: the variable's name (never its value).
    Route(String),
    /// The scratch folder could not be made, or this process could not be read.
    Scratch,
    /// The child could not be started, read, written or ended.
    Process,
    /// The child asked for something other than an inference token, and was ended.
    Request,
    /// The hang detector ([`super::login_drive::HANG`]) ran out, and the child was ended.
    Hang,
    /// The child said more than [`BUFFER`] without a verdict, and was ended.
    Output,
    /// The person stopped it (Ctrl-C), and the child was ended.
    Stopped,
    /// The child ended, or stopped, without showing a token.
    NoToken,
    /// The child showed two different token-shaped values.
    TwoTokens,
    /// The child showed a token without first showing the sign-in it opened, so nothing checked
    /// what it asked for.
    Unchecked,
    /// The success frame named an account other than the one asked for: that one's email.
    WrongAccount(String),
    /// One of Claude Code's own failures.
    Failed(Failure),
    /// The sign-in in aterm's own window ended without authorizing: what its opener met, in
    /// the opener's own closed words ([`super::open_url::Met::words`]).
    Window(String),
}

/// Claude Code's own failures, as 2.1.318 words them ([`FAILURES`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// "Invalid code. Please make sure the full code was copied".
    CodeIncomplete,
    /// "Failed to exchange authorization code for access token".
    Exchange,
    /// "Your account is on hold and can't sign in to Claude Code".
    OnHold,
    /// An organization's policy refused the token, and Claude Code withheld it ("… was not
    /// shown").
    Withheld,
    /// "Couldn't start the local listener that receives the browser sign-in".
    Listener,
    /// Any other "OAuth error: …".
    OAuth,
}

/// Each failure phrase, in the order they are looked for (the generic one last).
pub const FAILURES: &[(&[u8], Failure)] = &[
    (
        b"Invalid code. Please make sure the full code was copied",
        Failure::CodeIncomplete,
    ),
    (
        b"Failed to exchange authorization code for access token",
        Failure::Exchange,
    ),
    (
        b"Your account is on hold and can't sign in to Claude Code",
        Failure::OnHold,
    ),
    (b"was not shown", Failure::Withheld),
    (b"Couldn't start the local listener", Failure::Listener),
    (b"OAuth error:", Failure::OAuth),
];

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::CodeIncomplete => {
                "Claude Code did not take the pasted code (it was not the whole code); run it \
                 again and paste all of it"
            }
            Self::Exchange => {
                "claude.com did not turn the sign-in into a token; run it again (nothing was made)"
            }
            Self::OnHold => "claude.com says this account is on hold and cannot sign in",
            Self::Withheld => {
                "the account's organization refused a long-lived token; Claude Code withheld it \
                 (ask its admin to revoke it in Console if Claude Code said it could not)"
            }
            Self::Listener => "Claude Code could not listen for the browser's return; run it again",
            Self::OAuth => "Claude Code's sign-in failed; run it again (nothing was made)",
        })
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Route(name) => write!(
                f,
                "this terminal routes Claude Code through `{name}`; a token is made only on the \
                 default route"
            ),
            Self::Scratch => {
                f.write_str("the private folder for Claude Code's sign-in could not be made")
            }
            Self::Process => {
                f.write_str("Claude Code's setup-token could not be started or controlled")
            }
            Self::Request => f.write_str(
                "Claude Code asked for something other than an inference token; nothing was made",
            ),
            Self::Hang => f.write_str(
                "the sign-in did not finish within 15 minutes; Claude Code was ended and nothing \
                 was made",
            ),
            Self::Output => f.write_str(
                "Claude Code said more than the sign-in should without a token; it was ended",
            ),
            Self::Stopped => f.write_str("stopped; nothing was made"),
            Self::NoToken => f.write_str("Claude Code ended without showing a token"),
            Self::TwoTokens => {
                f.write_str("Claude Code showed two different tokens; neither was kept")
            }
            Self::Unchecked => f.write_str(
                "Claude Code showed a token before the sign-in it opened could be checked; it was \
                 not kept",
            ),
            Self::WrongAccount(email) => write!(
                f,
                "Claude Code made a token for another account; sign in as {email}"
            ),
            Self::Failed(failure) => failure.fmt(f),
            Self::Window(why) => write!(
                f,
                "the sign-in in aterm's sign-in window did not complete: {why}; nothing was made"
            ),
        }
    }
}

/// How long the success frame said the token lasts ("valid for <words>").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    /// "1 year" (365 days) or "N day(s)".
    Days(u32),
    /// "less than a day": the token is not stored (§4.6).
    UnderADay,
    /// No words this grammar reads: the owner's mint + [`LIFETIME`] applies.
    Unprinted,
}

/// What a capture made: the token, and value-free facts about it.
#[derive(Debug)]
pub struct Minted {
    /// The token, in grammar ([`value_grammar`]).
    pub token: SecretBytes,
    /// The unix second its success frame was read.
    pub minted_at: u64,
    /// How long the frame said it lasts.
    pub lifetime: Lifetime,
    /// The account the frame named, where it named one (2.1.318 names none); always the one
    /// asked for ([`Error::WrongAccount`] otherwise).
    pub account: Option<String>,
}

impl Minted {
    /// When it ends, and where that came from: the printed days as the earliest end they allow
    /// (`minted_at + days × 86400 − 43200`, since Claude Code rounds to whole days), else the
    /// owner's `minted_at + 31536000`, assumed. `None` for a token that ends within a day.
    #[must_use]
    pub fn expiry(&self) -> Option<(u64, ExpiryFrom)> {
        expiry_of(self.lifetime, self.minted_at)
    }
}

/// [`Minted::expiry`] of `lifetime`, minted at `minted_at`.
#[must_use]
pub fn expiry_of(lifetime: Lifetime, minted_at: u64) -> Option<(u64, ExpiryFrom)> {
    match lifetime {
        Lifetime::Days(days) => Some((
            minted_at
                .saturating_add(u64::from(days).saturating_mul(86_400))
                .saturating_sub(43_200),
            ExpiryFrom::Printed,
        )),
        Lifetime::UnderADay => None,
        Lifetime::Unprinted => Some((minted_at.saturating_add(LIFETIME), ExpiryFrom::Assumed)),
    }
}

/// The program a capture runs, and its arguments: Claude Code's `setup-token`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// The store's current Claude Code build.
    pub path: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
}

impl Program {
    /// `path setup-token`.
    #[must_use]
    pub fn setup_token(path: PathBuf) -> Self {
        Self {
            path,
            args: vec![OsString::from("setup-token")],
        }
    }
}

/// The person at the verb's terminal, as the capture talks to them.
pub trait Person: Sync {
    /// Shows `line` (never a byte the child wrote).
    fn say(&self, line: &str);
    /// Reads the code the browser shows, unshown, after `prompt`: `Ok(None)` once `cancelled`
    /// is set (the browser's own return came first).
    ///
    /// # Errors
    /// `Interrupted` at Ctrl-C; any other error ends the asking (the browser's return still
    /// works).
    fn paste(
        &self,
        prompt: &str,
        cancelled: &AtomicBool,
    ) -> std::io::Result<Option<Zeroizing<Vec<u8>>>>;
    /// Whether the person stopped it (Ctrl-C).
    fn stopped(&self) -> bool;
    /// Why the sign-in this capture waits on ended without authorizing, where something other
    /// than the child says so (aterm's sign-in window's opener); a person's browser says none.
    fn failed(&self) -> Option<String> {
        None
    }
}

/// Aterm's own sign-in window as the place a capture's page opens ([`Capture::window`]): the
/// opener the child starts with its authorization URL, and the two files of this capture's
/// own private folder the opener answers in.
pub struct WindowRoute {
    /// Aterm's opener ([`super::open_url::OPENER`]), the child's `BROWSER`.
    pub opener: PathBuf,
    /// Where the opener records a sign-in that ended without authorizing
    /// ([`super::open_url::OUTCOME_ENV`]).
    pub outcome: PathBuf,
    /// Where the opener hands a hosted flow's code ([`super::open_url::CODE_ENV`]).
    pub code: PathBuf,
}

/// What the child's environment holds beside [`child_env`] where its page opens in aterm's
/// sign-in window: the opener as its `BROWSER`, the window's grant
/// ([`super::open_url::WINDOW_ENV`]), the account to sign in as, the two files the opener
/// answers in, and the sign-in's trace file where the verb named one.
#[must_use]
pub fn window_env(
    route: &WindowRoute,
    sign_in_as: &str,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(&'static str, OsString)> {
    let env = vec![
        ("BROWSER", route.opener.as_os_str().to_owned()),
        (super::open_url::WINDOW_ENV, OsString::from("1")),
        (super::open_url::EMAIL_ENV, OsString::from(sign_in_as)),
        (
            super::open_url::OUTCOME_ENV,
            route.outcome.as_os_str().to_owned(),
        ),
        (super::open_url::CODE_ENV, route.code.as_os_str().to_owned()),
    ];
    #[cfg(target_os = "macos")]
    let env = {
        let mut env = env;
        if let Some(trace) = var(super::signin_window::TRACE_ENV).filter(|v| !v.is_empty()) {
            env.push((super::signin_window::TRACE_ENV, trace));
        }
        env
    };
    #[cfg(not(target_os = "macos"))]
    let _ = var;
    env
}

/// The capture's person where nobody is asked: the sign-in runs in aterm's own window, which
/// finishes it by itself. A hosted flow's code is the one the window's opener handed over
/// ([`WindowRoute::code`]), taken once; a sign-in the opener ended without authorizing ends
/// the capture with what it met ([`Person::failed`]).
pub struct WindowWaiter<'a> {
    route: &'a WindowRoute,
}

impl<'a> WindowWaiter<'a> {
    /// The waiter on `route`'s files.
    #[must_use]
    pub const fn new(route: &'a WindowRoute) -> Self {
        Self { route }
    }
}

impl Person for WindowWaiter<'_> {
    /// Nobody asked for this run's lines, and its standard error is whatever started it: a
    /// line is written with each address in it as `<address>`.
    fn say(&self, line: &str) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{}", super::open_url::without_addresses(line));
    }

    fn paste(
        &self,
        _prompt: &str,
        cancelled: &AtomicBool,
    ) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
        loop {
            if cancelled.load(Ordering::SeqCst) || self.failed().is_some() {
                return Ok(None);
            }
            match super::open_url::take_oauth_code(self.route.code.clone()) {
                Ok(Some(code)) => {
                    let code = Zeroizing::new(code);
                    return Ok(Some(Zeroizing::new(code.as_bytes().to_vec())));
                }
                Ok(None) => {}
                Err(why) => return Err(std::io::Error::other(why)),
            }
            std::thread::sleep(READ_TICK);
        }
    }

    fn stopped(&self) -> bool {
        false
    }

    fn failed(&self) -> Option<String> {
        super::open_url::read_terminal_outcome(&self.route.outcome).map(|failure| {
            failure
                .0
                .map_or_else(|| "its opener stopped".to_owned(), |met| met.words())
        })
    }
}

/// The variable of the verb's environment `env` that names a route, by name: the first
/// [`super::netwatch::names_a_route`] admits.
#[must_use]
pub fn route_in(env: &[(OsString, OsString)]) -> Option<String> {
    env.iter().find_map(|(key, value)| {
        let key = key.to_string_lossy();
        super::netwatch::names_a_route(&key, &value.to_string_lossy()).then(|| key.into_owned())
    })
}

/// The child's whole environment: [`super::login_drive::passed_unrouted`] from `var` (each one
/// set and not empty), [`CHILD_PATH`], `TERM=dumb`, both Claude homes
/// ([`super::accounts::HOME_ENV`], [`super::accounts::LOGIN_ENV`]) on `scratch`, and
/// [`OPT_OUTS`]. Nothing else crosses: no credential, `BROWSER`, route or aterm variable.
#[must_use]
pub fn child_env(
    var: &dyn Fn(&str) -> Option<OsString>,
    scratch: &Path,
) -> Vec<(&'static str, OsString)> {
    let mut env: Vec<(&'static str, OsString)> = super::login_drive::passed_unrouted()
        .filter_map(|name| var(name).filter(|v| !v.is_empty()).map(|v| (name, v)))
        .collect();
    env.push(("PATH", OsString::from(CHILD_PATH)));
    env.push(("TERM", OsString::from("dumb")));
    env.push((super::accounts::HOME_ENV, scratch.as_os_str().to_owned()));
    env.push((super::accounts::LOGIN_ENV, scratch.as_os_str().to_owned()));
    env.extend(OPT_OUTS.iter().map(|(k, v)| (*k, OsString::from(v))));
    env
}

/// A capture's own 0700 scratch folder under the account's store folder, named for the process
/// that made it (`setup-token-<pid>-<birth>`), removed when it drops. A capture whose process
/// was killed leaves its folder; the next capture under that store sweeps every one whose
/// process is gone ([`sweep`]).
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// Sweeps `root`, then makes this process's folder in it.
    ///
    /// # Errors
    /// `root` is no private folder of this user's and cannot be made one; this process cannot be
    /// read; the folder cannot be made (one of this name is there).
    pub fn make(root: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        aterm_types::fs_restricted::ensure_private_dir(root)?;
        sweep(root);
        let me = i32::try_from(std::process::id())
            .ok()
            .and_then(aterm_uds::process::read_identity)
            .ok_or_else(|| std::io::Error::other("this process could not be read"))?;
        let path = root.join(format!("{SCRATCH_PREFIX}{}-{}", me.pid, me.birth.0));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self { path })
    }

    /// Where it is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Removes each scratch folder in `root` ([`Scratch`]) whose process is gone: its pid reads no
/// process, or one of another birth. A name that does not parse, or an entry that is no real
/// folder, is left alone.
pub fn sweep(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(owner) = name
            .to_str()
            .and_then(|name| name.strip_prefix(SCRATCH_PREFIX))
            .and_then(|rest| rest.split_once('-'))
            .and_then(|(pid, birth)| Some((pid.parse::<i32>().ok()?, birth.parse::<u64>().ok()?)))
        else {
            continue;
        };
        let path = entry.path();
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_dir());
        let gone =
            aterm_uds::process::read_identity(owner.0).is_none_or(|now| now.birth.0 != owner.1);
        if is_dir && gone {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// What a capture is given.
pub struct Capture<'a> {
    /// The program it runs.
    pub program: &'a Program,
    /// The account's store folder, where its scratch folder goes.
    pub store: &'a Path,
    /// The account to sign in as: its email.
    pub sign_in_as: &'a str,
    /// The verb's own environment.
    pub env: &'a [(OsString, OsString)],
    /// Aterm's own sign-in window, where the child's page opens there; none opens it in the
    /// person's default browser.
    pub window: Option<&'a WindowRoute>,
    /// The hang detector ([`super::login_drive::HANG`] from the verb's start), on both clocks.
    pub deadline: WakeDeadline,
}

/// Runs the capture (the module's summary says how): the token Claude Code made, or why none.
///
/// # Errors
/// Each [`Error`], none holding a byte the child wrote.
pub fn capture(run: &Capture<'_>, person: &dyn Person) -> Result<Minted, Error> {
    if let Some(name) = route_in(run.env) {
        return Err(Error::Route(name));
    }
    let scratch = Scratch::make(run.store).map_err(|_| Error::Scratch)?;
    let var = |name: &str| {
        run.env
            .iter()
            .find(|(key, _)| key.to_str() == Some(name))
            .map(|(_, value)| value.clone())
    };
    let mut env = child_env(&var, scratch.path());
    if let Some(route) = run.window {
        env.extend(window_env(route, run.sign_in_as, &var));
    }
    let mut child = Child::spawn(run.program, &env, scratch.path())?;
    let cancel = AtomicBool::new(false);
    let minted = std::thread::scope(|scope| drive(run, person, &mut child, &cancel, scope));
    // A token's child is let exit; any other is killed at once. A token is kept even where its
    // child's end could not be confirmed: Claude Code shows it once, and it is already made.
    if minted.is_ok() {
        child.end(run.deadline);
    } else {
        child.kill();
    }
    drop(child);
    drop(scratch);
    minted
}

/// The child, spawned: ended (its group killed, then reaped) on drop where [`Child::end`] did
/// not end it.
struct Child {
    child: std::process::Child,
    pid: i32,
    master: Option<std::fs::File>,
    slave: Option<OwnedFd>,
    exit: OwnedFd,
    ended: bool,
}

impl Child {
    fn spawn(
        program: &Program,
        env: &[(&'static str, OsString)],
        scratch: &Path,
    ) -> Result<Self, Error> {
        let (master, slave, mut child) =
            aterm_pty::spawn_session_leader_on_pty_sized(ROWS, COLS, |slave| {
                let tty = || slave.try_clone().map(std::process::Stdio::from);
                #[allow(
                    clippy::disallowed_types,
                    reason = "the controlling-terminal spawn seam takes StdCommand and applies \
                              inherit_nothing before spawning"
                )]
                let mut cmd = std::process::Command::new(&program.path);
                cmd.env_clear()
                    .envs(env.iter().map(|(k, v)| (k, v)))
                    .args(&program.args)
                    .current_dir(scratch)
                    .stdout(tty()?)
                    .stderr(tty()?);
                Ok(cmd)
            })
            .map_err(|_| Error::Process)?;
        let pid = i32::try_from(child.id()).ok();
        let exit = pid.and_then(|_| aterm_uds::exitwatch::exit_fd(child.id()));
        let (Some(pid), Some(exit)) = (pid, exit) else {
            if let Some(pid) = pid {
                let _ = aterm_uds::process::kill_group_until_empty(pid);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Process);
        };
        let master = std::fs::File::from(master);
        let mut this = Self {
            child,
            pid,
            master: Some(master),
            slave: Some(slave),
            exit,
            ended: false,
        };
        if let Some(master) = &this.master
            && aterm_pty::set_nonblocking(master.as_raw_fd(), true).is_err()
        {
            this.kill();
            return Err(Error::Process);
        }
        Ok(this)
    }

    fn master(&self) -> Result<&std::fs::File, Error> {
        self.master.as_ref().ok_or(Error::Process)
    }

    /// Whether the child has exited (its exit watch is readable).
    fn exited(&self) -> bool {
        aterm_uds::ready::any_interests_now(&[aterm_uds::ready::Watch::new(
            self.exit.as_fd(),
            aterm_uds::ready::Interest::Read,
        )])
        .is_ok_and(|ready| !ready.is_empty())
    }

    /// Reads (and wipes) what the child wrote, until it exited or `until` passed: on Apple a
    /// session leader's exit waits for its terminal's output to drain, which reading the master
    /// lets it finish, so a child killed with a full terminal is never left stuck in its exit.
    fn drain_until_exited(&self, until: WakeDeadline) {
        let mut chunk = Zeroizing::new([0u8; 4096]);
        while !self.exited() && !until.passed() {
            let Some(master) = &self.master else {
                return;
            };
            let mut reader: &std::fs::File = master;
            while matches!(reader.read(&mut chunk[..]), Ok(n) if n > 0) {}
            let _ = aterm_uds::ready::either_until(
                master.as_fd(),
                self.exit.as_fd(),
                Instant::now() + until.tick().min(READ_TICK),
            );
        }
    }

    /// Its group ended and it reaped, the master last: its group killed once, its output
    /// drained until it exited ([`Child::drain_until_exited`], within [`EXIT_GRACE`]), the group
    /// killed again until no member is left (a member forked as the first signal landed), then
    /// it reaped and its terminal closed. Never reaps before the group is ended: the unreaped
    /// leader keeps its group's id from being reused.
    fn kill(&mut self) {
        if self.ended {
            return;
        }
        let _ = aterm_uds::process::kill_group_until_empty_within(self.pid, Duration::ZERO);
        self.drain_until_exited(WakeDeadline::after(EXIT_GRACE));
        self.reap()
    }

    /// The group of an exited (or killed) child ended, the child reaped, its terminal closed.
    fn reap(&mut self) {
        let _ = if self.exited() {
            super::agy_signin::finish_exited_group(self.pid)
        } else {
            aterm_uds::process::kill_group_until_empty_within(self.pid, EXIT_GRACE)
        };
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.ended = true;
        drop(self.master.take());
        drop(self.slave.take());
    }

    /// Ends a child that showed its token: it exits by itself (2.1.318 does 500 ms after its
    /// success frame), its output drained meanwhile, within [`EXIT_GRACE`] and never past
    /// `deadline`; one that has not by then is killed ([`Child::kill`]). Then its group is ended
    /// and it reaped, its terminal closed last.
    fn end(&mut self, deadline: WakeDeadline) {
        if self.ended {
            return;
        }
        self.drain_until_exited(deadline.within(EXIT_GRACE));
        if self.exited() {
            self.reap()
        } else {
            self.kill()
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Sets its flag when it drops, however the scope it guards ends: the paste thread's stop.
struct CancelOnDrop<'a>(&'a AtomicBool);

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What the paste thread hands back.
enum Pasted {
    /// The code, to write to the child's terminal.
    Code(Zeroizing<Vec<u8>>),
    /// Ctrl-C at the prompt.
    Interrupted,
}

/// Reads the child until a verdict; the paste prompt runs on a thread of `scope`, stopped by
/// `cancel` as this returns.
fn drive<'scope>(
    run: &Capture<'_>,
    person: &'scope dyn Person,
    child: &mut Child,
    cancel: &'scope AtomicBool,
    scope: &'scope std::thread::Scope<'scope, '_>,
) -> Result<Minted, Error> {
    let _cancel = CancelOnDrop(cancel);
    let mut out: Zeroizing<Vec<u8>> = Zeroizing::new(Vec::with_capacity(BUFFER));
    let mut cleaner = Cleaner::default();
    let mut judge = Judge::new(run.sign_in_as);
    let mut chunk = Zeroizing::new([0u8; 4096]);
    let (sent, pasted) = mpsc::sync_channel::<Pasted>(1);
    let mut asked = false;
    let mut told = false;
    loop {
        if person.stopped() {
            return Err(Error::Stopped);
        }
        if let Some(why) = person.failed() {
            return Err(Error::Window(why));
        }
        if run.deadline.passed() {
            return Err(Error::Hang);
        }
        let exited = child.exited();
        let tick = Instant::now() + run.deadline.tick().min(READ_TICK);
        let readable = if exited {
            true
        } else {
            aterm_uds::ready::either_until(child.master()?.as_fd(), child.exit.as_fd(), tick)
                .map_err(|_| Error::Process)?
                .is_some_and(|ready| ready.first)
        };
        if readable {
            loop {
                match child.master()?.read(&mut chunk[..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        let fed = cleaner.feed(&chunk[..n], &mut out);
                        chunk[..n].zeroize();
                        if !fed {
                            return Err(Error::Output);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    // EIO: no process holds the terminal's other end any more.
                    Err(_) => break,
                }
            }
        }
        match judge.judge(&out, exited) {
            Verdict::Pending => {}
            Verdict::Refused(error) => return Err(error),
            Verdict::Minted(minted) => return Ok(minted),
        }
        if judge.url_ok && !told {
            told = true;
            person.say(&if run.window.is_some() {
                format!(
                    "aterm's sign-in window opened on {} and authorizes the account by itself",
                    judge.host
                )
            } else {
                format!(
                    "a browser window opened on {}: sign in as {} there, then come back here \
                     (Ctrl-C stops)",
                    judge.host, run.sign_in_as
                )
            });
        }
        if judge.asks && judge.url_ok && !asked {
            asked = true;
            let sent = sent.clone();
            scope.spawn(move || ask_paste(person, cancel, &sent));
        }
        match pasted.try_recv() {
            Ok(Pasted::Code(code)) => {
                write_code(child, &code, run.deadline)?;
                person.say("handed the code to Claude Code");
            }
            Ok(Pasted::Interrupted) => return Err(Error::Stopped),
            Err(_) => {}
        }
    }
}

/// The paste thread: asks until a code that can be one comes, the asking is cancelled, or
/// Ctrl-C.
fn ask_paste(person: &dyn Person, cancel: &AtomicBool, sent: &mpsc::SyncSender<Pasted>) {
    loop {
        match person.paste(PASTE_PROMPT, cancel) {
            Ok(Some(code)) if code.is_empty() => {}
            Ok(Some(code)) if code.iter().all(|b| (0x21..=0x7e).contains(b)) => {
                let _ = sent.send(Pasted::Code(code));
                return;
            }
            Ok(Some(_)) => person.say("that is not a code (it holds a space or a control key)"),
            Ok(None) => return,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                let _ = sent.send(Pasted::Interrupted);
                return;
            }
            Err(_) => {
                person.say("the code could not be read here; the browser's own return still works");
                return;
            }
        }
    }
}

/// Writes `code` and Enter to the child's terminal, then wipes what it wrote from.
fn write_code(child: &Child, code: &[u8], deadline: WakeDeadline) -> Result<(), Error> {
    let mut line = Zeroizing::new(Vec::with_capacity(code.len() + 1));
    line.extend_from_slice(code);
    line.push(b'\r');
    let mut bytes = &line[..];
    while !bytes.is_empty() {
        if deadline.passed() {
            return Err(Error::Hang);
        }
        match child.master()?.write(bytes) {
            Ok(0) => return Err(Error::Process),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let _ = aterm_uds::ready::until(
                    child.master()?.as_fd(),
                    aterm_uds::ready::Interest::Write,
                    Instant::now() + deadline.tick().min(READ_TICK),
                );
            }
            Err(_) => return Err(Error::Process),
        }
    }
    Ok(())
}

/// The child's bytes cleaned as they come, into one buffer that never grows past its capacity:
/// printable bytes (UTF-8 included) kept; a carriage return or newline kept as a newline; a tab
/// as a space; every other control byte dropped; an escape sequence skipped, a CSI other than a
/// colour (`m`) or a cursor-forward leaving a newline so that a redraw's pieces are never joined
/// into one word; an OSC 8 link's address kept as a line of its own, and every other OSC dropped.
/// A cursor-forward is a space: `C`, and a move to a column (`G`, `` ` ``) right of where the
/// cursor stands, which is how 2.1.318's renderer skips the blanks between words on a `dumb`
/// terminal (S9 lane A, `docs/measured/claude-setup-token-lab-2026-11-06.md`); a move to a column
/// at or left of it is a redraw, a newline. Where the cursor stands is counted in characters
/// since the last line's start, one a character.
#[derive(Debug, Default)]
pub struct Cleaner {
    state: State,
    /// Where the current OSC's payload starts in the buffer.
    osc_at: usize,
    /// The cursor's column, 0-based, as the cleaned characters and moves place it.
    col: u32,
    /// The current CSI's first two numeric parameters (0 where absent).
    params: [u32; 2],
    /// Which of them is being read.
    param: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Esc,
    EscIntermediate,
    Csi,
    Osc,
    OscEsc,
}

impl Cleaner {
    /// Cleans `raw` onto `out`: `false` once `out` is full (its capacity), with nothing past it
    /// kept.
    #[must_use]
    pub fn feed(&mut self, raw: &[u8], out: &mut Vec<u8>) -> bool {
        raw.iter().all(|&b| self.step(b, out))
    }

    /// Cleans one byte onto `out`: `false` where `out` is full.
    fn step(&mut self, b: u8, out: &mut Vec<u8>) -> bool {
        match self.state {
            State::Ground => match b {
                0x1b => {
                    self.state = State::Esc;
                    true
                }
                b'\r' | b'\n' => {
                    self.col = 0;
                    push(out, b'\n')
                }
                b'\t' => {
                    self.col += 1;
                    push(out, b' ')
                }
                0x00..=0x1f | 0x7f => true,
                _ => {
                    if !(0x80..=0xbf).contains(&b) {
                        self.col += 1;
                    }
                    push(out, b)
                }
            },
            State::Esc => match b {
                b'[' => {
                    self.state = State::Csi;
                    self.params = [0; 2];
                    self.param = 0;
                    true
                }
                b']' => {
                    self.state = State::Osc;
                    let ok = push(out, b'\n');
                    self.osc_at = out.len();
                    ok
                }
                0x20..=0x2f => {
                    self.state = State::EscIntermediate;
                    true
                }
                _ => {
                    self.state = State::Ground;
                    true
                }
            },
            State::EscIntermediate => {
                if (0x30..=0x7e).contains(&b) {
                    self.state = State::Ground;
                }
                true
            }
            State::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    self.state = State::Ground;
                    self.end_csi(b, out)
                } else {
                    match b {
                        b'0'..=b'9' => {
                            if let Some(p) = self.params.get_mut(self.param) {
                                *p = p.saturating_mul(10).saturating_add(u32::from(b - b'0'));
                            }
                        }
                        b';' => self.param += 1,
                        _ => {}
                    }
                    true
                }
            }
            State::Osc => match b {
                0x07 => {
                    self.end_osc(out);
                    true
                }
                0x1b => {
                    self.state = State::OscEsc;
                    true
                }
                0x00..=0x1f | 0x7f => true,
                _ => push(out, b),
            },
            State::OscEsc => {
                if b == b'\\' {
                    self.end_osc(out);
                    true
                } else {
                    // An escape inside an OSC ends it unterminated: drop what it held, and
                    // read this byte as the escape's next.
                    cut(out, self.osc_at);
                    self.state = State::Esc;
                    self.step(b, out)
                }
            }
        }
    }

    /// Ends a CSI whose final byte is `fin`: a colour is nothing; a cursor-forward, or a move to a
    /// column right of the cursor, is a space; a move to a column at or left of it, and every other
    /// CSI, a newline (the cursor's column moved as the sequence moves it).
    fn end_csi(&mut self, fin: u8, out: &mut Vec<u8>) -> bool {
        let n = |p: u32| p.max(1);
        match fin {
            b'm' => true,
            b'C' => {
                self.col = self.col.saturating_add(n(self.params[0]));
                push(out, b' ')
            }
            b'G' | b'`' => {
                let to = n(self.params[0]) - 1;
                let forward = to > self.col;
                let same = to == self.col;
                self.col = to;
                if same {
                    true
                } else {
                    push(out, if forward { b' ' } else { b'\n' })
                }
            }
            b'H' | b'f' => {
                self.col = n(self.params[1]) - 1;
                push(out, b'\n')
            }
            b'D' => {
                self.col = self.col.saturating_sub(n(self.params[0]));
                push(out, b'\n')
            }
            _ => push(out, b'\n'),
        }
    }

    /// Ends the OSC whose payload is `out[osc_at..]`: an OSC 8 (`8;params;uri`) keeps its `uri`
    /// as a line of its own; any other is dropped.
    fn end_osc(&mut self, out: &mut Vec<u8>) {
        self.state = State::Ground;
        let at = self.osc_at.min(out.len());
        let uri = out[at..].strip_prefix(b"8;").and_then(|rest| {
            rest.iter()
                .position(|&c| c == b';')
                .map(|i| (2 + i + 1, rest.len() - i - 1))
        });
        match uri {
            Some((from, len)) if len > 0 => {
                out.copy_within(at + from..at + from + len, at);
                cut(out, at + len);
                let _ = push(out, b'\n');
            }
            _ => cut(out, at),
        }
    }
}

/// Pushes `b` where `out` has room: `false` where it is full.
fn push(out: &mut Vec<u8>, b: u8) -> bool {
    if out.len() < out.capacity() {
        out.push(b);
        true
    } else {
        false
    }
}

/// Wipes and drops everything of `out` from `at`.
fn cut(out: &mut Vec<u8>, at: usize) {
    if at < out.len() {
        out[at..].zeroize();
        out.truncate(at);
    }
}

/// What the cleaned output says so far.
pub enum Verdict {
    /// Nothing yet.
    Pending,
    /// No token: why.
    Refused(Error),
    /// The token.
    Minted(Minted),
}

/// The closed grammar, read a whole line at a time as the cleaned output grows (and the rest
/// once the child exited); what it found is kept as offsets into the output, so no child byte
/// is copied until a token is taken.
pub struct Judge<'a> {
    sign_in_as: &'a str,
    /// The end of what was read, whole lines.
    read: usize,
    /// Whether an authorization request was shown and admitted ([`admit_url`]).
    pub url_ok: bool,
    /// The admitted request's host ("claude.com" or "claude.ai").
    pub host: &'static str,
    /// Whether the paste prompt was shown.
    pub asks: bool,
    /// Where the success marker's line ends.
    marker: Option<usize>,
    /// The first token-shaped value: its place.
    token: Option<(usize, usize)>,
    /// Whether a token came after the marker.
    token_after_marker: bool,
    lifetime: Lifetime,
    account: Option<String>,
}

/// The success frame's first line ("✓ Long-lived …", without its tick).
pub const SUCCESS_MARKER: &[u8] = b"Long-lived authentication token created successfully!";

/// The success frame's last line (its export line): the frame is whole once it is read, so every
/// token-shaped value of the frame, this line's own included, was judged before the token is
/// taken.
pub const FRAME_END: &[u8] = b"Use this token by setting";

/// What 2.1.318 says while it waits for a pasted code.
pub const PASTE_MARKERS: &[&[u8]] = &[b"Paste code here"];

/// A token's first bytes (`sk-ant-oat`), spelled in halves so no fixture scanner reads a key.
const TOKEN_HEAD: &[u8] = b"sk-ant-oa\x74";

impl<'a> Judge<'a> {
    /// A judge for a capture that signs in as `sign_in_as`.
    #[must_use]
    pub fn new(sign_in_as: &'a str) -> Self {
        Self {
            sign_in_as,
            read: 0,
            url_ok: false,
            host: "claude.com",
            asks: false,
            marker: None,
            token: None,
            token_after_marker: false,
            lifetime: Lifetime::Unprinted,
            account: None,
        }
    }

    /// Reads the whole lines of `out` it has not read (and, once `ended`, the rest), then says
    /// what they add up to; once ended with no token, [`Error::NoToken`].
    pub fn judge(&mut self, out: &[u8], ended: bool) -> Verdict {
        let upto = if ended {
            out.len()
        } else {
            out.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
        };
        while self.read < upto {
            let end = out[self.read..upto]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(upto, |i| self.read + i);
            let line = (self.read, end);
            self.read = (end + 1).min(upto);
            if let Some(verdict) = self.line(out, line) {
                return verdict;
            }
        }
        if !self.asks {
            self.asks = PASTE_MARKERS
                .iter()
                .any(|m| find(&out[self.read.min(out.len())..], m).is_some());
        }
        if ended {
            return Verdict::Refused(Error::NoToken);
        }
        Verdict::Pending
    }

    /// Reads the line `out[at.0..at.1]`: a verdict where it makes one.
    fn line(&mut self, out: &[u8], (from, to): (usize, usize)) -> Option<Verdict> {
        let line = &out[from..to];
        if let Some(refused) = self.urls(line) {
            return Some(Verdict::Refused(refused));
        }
        if let Some((_, failure)) = FAILURES
            .iter()
            .find(|(phrase, _)| find(line, phrase).is_some())
        {
            return Some(Verdict::Refused(Error::Failed(*failure)));
        }
        if PASTE_MARKERS.iter().any(|m| find(line, m).is_some()) {
            self.asks = true;
        }
        if let Some(refused) = self.tokens(out, from, to) {
            return Some(Verdict::Refused(refused));
        }
        if self.marker.is_none() && find(line, SUCCESS_MARKER).is_some() {
            self.marker = Some(to);
            return None;
        }
        if self.marker.is_some() {
            if let Some(words) = valid_for(line) {
                self.lifetime = words;
            }
            if self.account.is_none()
                && let Some(email) = email_in(line)
            {
                if !super::accounts::email_matches(self.sign_in_as, email) {
                    return Some(Verdict::Refused(Error::WrongAccount(
                        self.sign_in_as.to_owned(),
                    )));
                }
                self.account = Some(email.to_owned());
            }
            if self.token_after_marker && find(line, FRAME_END).is_some() {
                return Some(self.minted(out));
            }
        }
        None
    }

    /// The token, taken: [`Error::Unchecked`] where no authorization request was admitted.
    fn minted(&mut self, out: &[u8]) -> Verdict {
        if !self.url_ok {
            return Verdict::Refused(Error::Unchecked);
        }
        let Some((from, to)) = self.token else {
            return Verdict::Refused(Error::NoToken);
        };
        let minted_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Verdict::Minted(Minted {
            token: SecretBytes::from(&out[from..to]),
            minted_at,
            lifetime: self.lifetime,
            account: self.account.take(),
        })
    }

    /// Checks each authorization request on `line` ([`admit_url`]): [`Error::Request`] at one it
    /// does not admit.
    fn urls(&mut self, line: &[u8]) -> Option<Error> {
        let mut at = 0;
        while let Some(i) = find(&line[at..], b"http") {
            let start = at + i;
            let rest = &line[start..];
            let len = rest
                .iter()
                .position(|&c| c <= b' ' || matches!(c, b'"' | b'\'' | b'<' | b'>' | b'`'))
                .unwrap_or(rest.len());
            let url = &rest[..len];
            at = start + len.max(4);
            if !(url.starts_with(b"https://") || url.starts_with(b"http://")) {
                continue;
            }
            match admit_url(url) {
                Admit::NotARequest => {}
                Admit::Admitted(host) => {
                    self.url_ok = true;
                    self.host = host;
                }
                Admit::Refused => return Some(Error::Request),
            }
        }
        None
    }

    /// Notes each token-shaped value in `out[from..to]`: [`Error::TwoTokens`] at one unlike the
    /// first.
    fn tokens(&mut self, out: &[u8], from: usize, to: usize) -> Option<Error> {
        let mut at = from;
        while let Some(i) = find(&out[at..to], TOKEN_HEAD) {
            let start = at + i;
            let token_byte = |c: &u8| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-';
            let len = out[start..to]
                .iter()
                .position(|c| !token_byte(c))
                .unwrap_or(to - start);
            at = start + len.max(1);
            if start > from && token_byte(&out[start - 1]) {
                continue;
            }
            let value = &out[start..start + len];
            if !value_grammar(value) {
                continue;
            }
            match self.token {
                None => self.token = Some((start, start + len)),
                Some((a, b)) if out[a..b] == *value => {}
                Some(_) => return Some(Error::TwoTokens),
            }
            if self.marker.is_some() {
                self.token_after_marker = true;
            }
        }
        None
    }
}

/// Where `needle` first is in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The lifetime a "(valid for <words>):" line says: "1 year", "N day(s)", "less than a day".
/// Words of no other shape read as [`Lifetime::Unprinted`]; a line with none, `None`.
#[must_use]
pub fn valid_for(line: &[u8]) -> Option<Lifetime> {
    let at = find(line, b"valid for ")? + b"valid for ".len();
    let rest = &line[at..];
    let words = &rest[..find(rest, b")").unwrap_or(rest.len())];
    Some(match words {
        b"1 year" => Lifetime::Days(365),
        b"less than a day" => Lifetime::UnderADay,
        _ => {
            let (n, unit) = match words.iter().position(|&c| c == b' ') {
                Some(i) => (&words[..i], &words[i + 1..]),
                None => return Some(Lifetime::Unprinted),
            };
            let days = std::str::from_utf8(n)
                .ok()
                .filter(|n| n.bytes().all(|c| c.is_ascii_digit()))
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|days| (1..=36_500).contains(days));
            match (days, unit) {
                (Some(1), b"day") => Lifetime::Days(1),
                (Some(days), b"days") if days > 1 => Lifetime::Days(days),
                _ => Lifetime::Unprinted,
            }
        }
    })
}

/// The first email-shaped word on `line` (`local@domain.tld`, ASCII), if any.
#[must_use]
pub fn email_in(line: &[u8]) -> Option<&str> {
    let at = line.iter().position(|&c| c == b'@')?;
    let local_byte = |c: &u8| c.is_ascii_alphanumeric() || b"._%+-".contains(c);
    let domain_byte = |c: &u8| c.is_ascii_alphanumeric() || b".-".contains(c);
    let start = line[..at]
        .iter()
        .rposition(|c| !local_byte(c))
        .map_or(0, |i| i + 1);
    let end = line[at + 1..]
        .iter()
        .position(|c| !domain_byte(c))
        .map_or(line.len(), |i| at + 1 + i);
    let domain = line[at + 1..end]
        .strip_suffix(b".")
        .unwrap_or(&line[at + 1..end]);
    let end = at + 1 + domain.len();
    let shaped = start < at
        && domain.contains(&b'.')
        && !domain.starts_with(b".")
        && domain
            .rsplit(|&c| c == b'.')
            .next()
            .is_some_and(|tld| tld.len() >= 2);
    shaped
        .then(|| std::str::from_utf8(&line[start..end]).ok())
        .flatten()
}

/// What [`admit_url`] makes of one URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// No authorization request: its path does not end at `/authorize` and it names no scope,
    /// redirect or challenge.
    NotARequest,
    /// A setup-token's request on this host.
    Admitted(&'static str),
    /// A request for anything else: the child is ended.
    Refused,
}

/// The hosts whose authorize path a setup-token's request may take.
pub const HOSTS: &[&str] = &["claude.com", "claude.ai"];

/// The authorize paths on [`HOSTS`].
pub const AUTHORIZE_PATHS: &[&str] = &["/cai/oauth/authorize", "/oauth/authorize"];

/// The scope a setup-token asks for, alone.
pub const SCOPE: &str = "user:inference";

/// Judges `url` as an authorization request: one is admitted when it is `https`, on one of
/// [`HOSTS`] with no user, password or port, at one of [`AUTHORIZE_PATHS`], with one `scope`
/// that is [`SCOPE`] alone, and, where it names a lifetime (`expires_in`), [`LIFETIME`].
#[must_use]
pub fn admit_url(url: &[u8]) -> Admit {
    let (scheme_ok, rest) = match url.strip_prefix(b"https://") {
        Some(rest) => (true, rest),
        None => (false, url.strip_prefix(b"http://").unwrap_or(url)),
    };
    let rest = &rest[..find(rest, b"#").unwrap_or(rest.len())];
    let (authority, path_query) = rest.split_at(
        rest.iter()
            .position(|&c| c == b'/' || c == b'?')
            .unwrap_or(rest.len()),
    );
    let (path, query) = match find(path_query, b"?") {
        Some(i) => (&path_query[..i], &path_query[i + 1..]),
        None => (path_query, &b""[..]),
    };
    let params: Vec<(Vec<u8>, Vec<u8>)> = query
        .split(|&c| c == b'&')
        .filter(|p| !p.is_empty())
        .map(|p| match find(p, b"=") {
            Some(i) => (decode(&p[..i]), decode(&p[i + 1..])),
            None => (decode(p), Vec::new()),
        })
        .collect();
    let named = |key: &[u8]| -> Vec<&[u8]> {
        params
            .iter()
            .filter(|(k, _)| k.as_slice() == key)
            .map(|(_, v)| v.as_slice())
            .collect()
    };
    let request = path.ends_with(b"/authorize")
        || [&b"scope"[..], b"redirect_uri", b"code_challenge"]
            .iter()
            .any(|key| !named(key).is_empty());
    if !request {
        return Admit::NotARequest;
    }
    let Some(host) = HOSTS.iter().find(|h| authority == h.as_bytes()) else {
        return Admit::Refused;
    };
    let scopes = named(b"scope");
    let lifetimes = named(b"expires_in");
    let ok = scheme_ok
        && AUTHORIZE_PATHS.iter().any(|p| path == p.as_bytes())
        && scopes.len() == 1
        && scopes[0] == SCOPE.as_bytes()
        && lifetimes.len() <= 1
        && lifetimes
            .first()
            .is_none_or(|v| *v == LIFETIME.to_string().as_bytes());
    if ok {
        Admit::Admitted(host)
    } else {
        Admit::Refused
    }
}

/// `form`, `application/x-www-form-urlencoded`-decoded (`+` a space, `%XX` its byte; a `%` with
/// no two hex digits after it kept as it is).
fn decode(form: &[u8]) -> Vec<u8> {
    let hex = |c: u8| {
        char::from(c)
            .to_digit(16)
            .and_then(|d| u8::try_from(d).ok())
    };
    let mut out = Vec::with_capacity(form.len());
    let mut i = 0;
    while i < form.len() {
        match form[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < form.len() => match (hex(form[i + 1]), hex(form[i + 2])) {
                (Some(a), Some(b)) => {
                    out.push(a * 16 + b);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// The person at the verb's own terminal: lines go to its stderr, the code is read unshown from
/// `/dev/tty` ([`crate::fleet_member::read_unshown_secret`]), and Ctrl-C is noted (SIGINT's
/// disposition replaced while it lives, put back when it drops) for the capture to end its child
/// and return [`Error::Stopped`].
pub struct Terminal {
    was: Option<libc::sigaction>,
}

/// Whether SIGINT came while a [`Terminal`] lived.
static STOPPED: AtomicBool = AtomicBool::new(false);

extern "C" fn note_stop(_signal: libc::c_int) {
    STOPPED.store(true, Ordering::SeqCst);
}

impl Terminal {
    /// Takes SIGINT for the capture's life.
    #[must_use]
    pub fn new() -> Self {
        STOPPED.store(false, Ordering::SeqCst);
        // SAFETY: `sigaction` is plain data for which all-zero is a valid value (an empty mask,
        // no flags).
        let mut noting: libc::sigaction = unsafe { std::mem::zeroed() };
        noting.sa_sigaction = note_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // SAFETY: As above.
        let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: A valid disposition for SIGINT; `was` receives the one it replaces.
        let set = unsafe { libc::sigaction(libc::SIGINT, &noting, &mut was) } == 0;
        Self {
            was: set.then_some(was),
        }
    }
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(was) = &self.was {
            // SAFETY: Puts back the disposition `Terminal::new` replaced.
            unsafe {
                libc::sigaction(libc::SIGINT, was, std::ptr::null_mut());
            }
        }
    }
}

impl Person for Terminal {
    fn say(&self, line: &str) {
        let mut err = std::io::stderr().lock();
        let _ = writeln!(err, "{line}");
    }

    fn paste(
        &self,
        prompt: &str,
        cancelled: &AtomicBool,
    ) -> std::io::Result<Option<Zeroizing<Vec<u8>>>> {
        crate::fleet_member::read_unshown_secret(prompt, cancelled).inspect_err(|e| {
            if e.kind() == std::io::ErrorKind::Interrupted {
                STOPPED.store(true, Ordering::SeqCst);
            }
        })
    }

    fn stopped(&self) -> bool {
        STOPPED.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
#[path = "setup_token_tests.rs"]
pub(in crate::harness) mod tests;
