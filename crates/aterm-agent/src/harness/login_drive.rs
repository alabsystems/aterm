// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vendor CLI's own sign-in, run by the harness in a staging home
//! (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md` §6): the child's standard output and error
//! are read line by line, bounded and ANSI stripped, against the route's closed grammar
//! ([`Grammar`]); the page URL it names must be `https` on a pinned host, or nothing is opened and
//! the child is killed; the page is handed to an [`Approver`] (the headless browser); and the
//! child must then say it is done and exit 0 within [`HANG`]. Nothing here writes the URL, the
//! code or anything else the child printed: what is kept of a run is its host, its flow and how
//! it ended ([`Drove::why`]).

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::ai::connector::{Cli, Completion, Drive, Flow, Grammar, Region, Route, Seam, SignIn};

pub use super::auth_deadline::Deadline;

/// The hang detector of one sign-in: the vendors' device codes live 15 minutes (Codex's) to
/// 30 (Kimi's), and a sign-in the page never approved is ended at this.
pub const HANG: Duration = Duration::from_secs(15 * 60);

/// The most of one line that is read; the rest of it is dropped unread.
pub const LINE_MAX: usize = 2048;

/// The most lines of one stream that are read; the rest is drained unread, so the child never
/// blocks on a full pipe.
pub const LINES_MAX: usize = 512;

/// How long a sign-in's output is read once its child exited: a grandchild that keeps the pipe
/// open does not hold the sign-in to [`HANG`].
const OUTPUT_GRACE: Duration = Duration::from_secs(2);

#[cfg(test)]
thread_local! {
    static OUTPUT_GRACE_SET: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// While held, this thread's [`drive`] reads a sign-in's output for the grace it was set with
/// once the child exited. Its drop, a panic's unwind included, puts back the grace it replaced.
#[cfg(test)]
struct GraceSet(Option<Duration>);

#[cfg(test)]
impl GraceSet {
    fn set(grace: Duration) -> Self {
        Self(OUTPUT_GRACE_SET.replace(Some(grace)))
    }
}

#[cfg(test)]
impl Drop for GraceSet {
    fn drop(&mut self) {
        OUTPUT_GRACE_SET.set(self.0);
    }
}

/// How long [`drive`] reads output once its child exited: [`OUTPUT_GRACE`]. The unit tests'
/// stand-ins leave no pipe open past their exit, so there it is the hang detector, which waits
/// for a reader thread a loaded machine starves, unless a test set a grace on this thread
/// ([`GraceSet`]).
fn output_grace() -> Duration {
    #[cfg(not(test))]
    {
        OUTPUT_GRACE
    }
    #[cfg(test)]
    {
        OUTPUT_GRACE_SET
            .get()
            .unwrap_or(aterm_spec::wait::MUST_HAPPEN)
    }
}

/// The shortest and longest code a grammar admits ([`is_code`]).
const CODE_LEN: std::ops::RangeInclusive<usize> = 4..=16;

/// The variables a sign-in's child is handed from this process's environment, as they are: who
/// it is, where its temporary files go, its locale and its network's proxy and certificates.
/// Nothing else crosses: no key, token, identity or header override reaches it.
pub const PASSED: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// `raw` without its ANSI escapes (CSI, OSC and two-byte sequences) or other control
/// characters, and, where a carriage return redrew the line, only what follows the last one.
#[must_use]
pub fn strip_ansi(raw: &str) -> String {
    let line = raw.rsplit('\r').find(|part| !part.is_empty()).unwrap_or("");
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else if c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

/// The lowercase host of an `https` URL with no user info, whitespace, control character or
/// backslash in it, and no port but 443's; `None` for anything else.
#[must_use]
pub fn https_host(url: &str) -> Option<String> {
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.contains('@') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((host, "443")) => host,
        Some(_) => return None,
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    (!host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-']))
    .then_some(host)
}

/// Whether `url` is `https` on one of `hosts`, exactly ([`https_host`]).
#[must_use]
pub fn pinned(url: &str, hosts: &[&str]) -> bool {
    https_host(url).is_some_and(|host| hosts.iter().any(|h| h.eq_ignore_ascii_case(&host)))
}

fn is_url(word: &str) -> bool {
    (word.starts_with("https://") || word.starts_with("http://"))
        && !word.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Whether `word` reads as a sign-in code: [`CODE_LEN`] letters, digits and dashes, a letter or
/// digit among them.
#[must_use]
pub fn is_code(word: &str) -> bool {
    CODE_LEN.contains(&word.len())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && word.chars().any(|c| c.is_ascii_alphanumeric())
}

fn is_email(word: &str) -> bool {
    word.len() <= super::accounts::EMAIL_MAX
        && word.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
        })
        && !word.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Url,
    Code,
}

/// What a sign-in's lines said so far ([`Scan::feed`]): the first URL and code its grammar
/// names, whether it said it is done, and the email its done line names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// The value a lead line left owed to the next line of its own stream (0 the child's
    /// output, 1 its errors): [`drive`] reads the two interleaved in any order, so a line of the
    /// one never takes the other's value.
    pending: [Option<Want>; 2],
    /// The page's URL.
    pub url: Option<String>,
    /// The code the page shows or asks for.
    pub code: Option<String>,
    /// Whether a done line was read.
    pub done: bool,
    /// The email the done line names ([`Grammar::signed_in_as`]).
    pub email: Option<String>,
}

impl Scan {
    /// Reads one line (as the child printed it) against `grammar`, all of them one stream's.
    pub fn feed(&mut self, grammar: &Grammar, raw: &str) {
        self.feed_from(0, grammar, raw);
    }

    /// [`Scan::feed`] for a line of the child's stream `stream` (0 its output, 1 its errors): a
    /// lead's value is read from the next line of the lead's own stream.
    fn feed_from(&mut self, stream: usize, grammar: &Grammar, raw: &str) {
        let pending = &mut self.pending[stream.min(1)];
        let line = strip_ansi(raw);
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(want) = pending.take() {
            match want {
                Want::Url if is_url(line) => {
                    self.url.get_or_insert_with(|| line.to_owned());
                    return;
                }
                Want::Code if is_code(line) => {
                    self.code.get_or_insert_with(|| line.to_owned());
                    return;
                }
                _ => {}
            }
        }
        for lead in grammar.url {
            if let Some(at) = line.find(lead) {
                let rest = line[at + lead.len()..].trim();
                if is_url(rest) {
                    self.url.get_or_insert_with(|| rest.to_owned());
                } else {
                    *pending = Some(Want::Url);
                }
                return;
            }
        }
        for lead in grammar.code {
            if let Some(at) = line.find(lead) {
                let rest = line[at + lead.len()..].trim();
                if is_code(rest) {
                    self.code.get_or_insert_with(|| rest.to_owned());
                } else {
                    *pending = Some(Want::Code);
                }
                return;
            }
        }
        if done_in(grammar, line) {
            self.done = true;
            if let Some(lead) = grammar.signed_in_as
                && let Some(at) = line.find(lead)
            {
                let rest = line[at + lead.len()..].trim();
                if is_email(rest) {
                    self.email = Some(rest.to_owned());
                }
            }
        }
    }
}

fn done_in(grammar: &Grammar, line: &str) -> bool {
    grammar.done.iter().any(|phrase| line.contains(phrase))
}

/// Reads `from` to its end, handing each line (its first [`LINE_MAX`] bytes, lossily decoded)
/// to `line`, the first [`LINES_MAX`] lines only; the rest is drained unread.
pub fn read_lines(from: impl Read, line: impl FnMut(String)) {
    read_lines_within(from, LINE_MAX, LINES_MAX, line);
}

/// [`read_lines`] with its own bounds: each line's first `line_max` bytes, the first
/// `lines_max` lines.
pub fn read_lines_within(
    mut from: impl Read,
    line_max: usize,
    lines_max: usize,
    mut line: impl FnMut(String),
) {
    let mut buf = [0u8; 4096];
    let mut held: Vec<u8> = Vec::new();
    let mut lines = 0usize;
    let mut emit = |held: &mut Vec<u8>, lines: &mut usize| {
        if *lines < lines_max {
            line(String::from_utf8_lossy(held).into_owned());
        }
        *lines += 1;
        held.clear();
    };
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &b in &buf[..n] {
            if b == b'\n' {
                emit(&mut held, &mut lines);
            } else if held.len() < line_max {
                held.push(b);
            }
        }
    }
    if !held.is_empty() {
        emit(&mut held, &mut lines);
    }
}

/// The arguments of `route`'s sign-in verb for a row of `region`.
#[must_use]
pub fn child_args(route: &Route, region: Option<Region>) -> Vec<String> {
    match &route.drive {
        Drive::Lines {
            args, region_flag, ..
        } => {
            let mut out: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            if let (Some(flag), Some(region)) = (region_flag, region) {
                out.push((*flag).to_owned());
                out.push(region.as_str().to_owned());
            }
            out
        }
        Drive::CodexPkce => vec!["login".to_owned()],
        Drive::NativeGoogle => Vec::new(),
    }
}

/// The whole environment of `cli`'s sign-in child in `staging` (design §6): [`PASSED`] from
/// `var`, `TERM=dumb` and `NO_COLOR=1`, the CLI's home variable set to `staging` (and a
/// [`Seam::LoginFile`]'s variable to its file there), the route's own variables
/// ([`SignIn::env`]), and `PATH` set to `path`, which for a [`SignIn::no_opener`] CLI is an empty
/// private directory ([`opener_free_path`]).
#[must_use]
pub fn child_env(
    cli: Cli,
    signin: &SignIn,
    staging: &Path,
    path: &OsStr,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Vec<(String, OsString)> {
    let mut env: Vec<(String, OsString)> = PASSED
        .iter()
        .filter_map(|name| {
            var(name)
                .filter(|v| !v.is_empty())
                .map(|v| ((*name).to_owned(), v))
        })
        .collect();
    env.push(("PATH".to_owned(), path.to_os_string()));
    env.push(("TERM".to_owned(), OsString::from("dumb")));
    env.push(("NO_COLOR".to_owned(), OsString::from("1")));
    if let Some(home) = cli.connector().home.var {
        env.push((home.to_owned(), staging.as_os_str().to_os_string()));
    }
    if let Seam::LoginFile { var, file } = cli.accounts().seam {
        env.retain(|(k, _)| k != var);
        env.push((var.to_owned(), staging.join(file).into_os_string()));
    }
    for (k, v) in signin.env {
        env.retain(|(name, _)| name != k);
        env.push(((*k).to_owned(), OsString::from(v)));
    }
    env
}

/// An empty private directory under `store_dir` to be a sign-in child's whole `PATH`
/// ([`SignIn::no_opener`]), removed when dropped.
/// # Errors
/// The store directory or the temporary directory cannot be made.
pub fn opener_free_path(store_dir: &Path) -> std::io::Result<aterm_tempfile::TempDir> {
    aterm_types::fs_restricted::ensure_private_dir(store_dir)?;
    aterm_tempfile::Builder::new()
        .prefix(".signin-path-")
        .tempdir_in(store_dir)
}

/// A sign-in child's program and `PATH`: `path` where given; else, for a
/// [`SignIn::no_opener`] CLI, a private directory ([`opener_free_path`]) holding only the
/// interpreter a `#!/usr/bin/env <name>` program names ([`env_interpreter`]), linked from the
/// parent's `PATH`; else the parent's `PATH`. A bare program word is found on the parent's
/// `PATH` first ([`on_path`]), since the child's own may hold nothing. The directory, where
/// made, is removed when the third field drops.
/// # Errors
/// The private directory cannot be made.
pub fn child_program(
    program: &Path,
    path: Option<&OsStr>,
    no_opener: bool,
    store_dir: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> std::io::Result<(PathBuf, OsString, Option<aterm_tempfile::TempDir>)> {
    let parent = var("PATH").unwrap_or_default();
    let program = on_path(program, &parent).unwrap_or_else(|| program.to_path_buf());
    if let Some(path) = path {
        return Ok((program, path.to_os_string(), None));
    }
    if !no_opener {
        return Ok((program, parent, None));
    }
    let dir = opener_free_path(store_dir)?;
    if let Some(name) = env_interpreter(&program)
        && let Some(interpreter) = on_path(Path::new(&name), &parent)
    {
        link_into(&interpreter, &dir.path().join(&name))?;
    }
    let path = dir.path().as_os_str().to_os_string();
    Ok((program, path, Some(dir)))
}

#[cfg(unix)]
fn link_into(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn link_into(target: &Path, link: &Path) -> std::io::Result<()> {
    std::fs::copy(target, link).map(|_| ())
}

/// The words a sign-in child's opener-free `PATH` never links, whatever a script's interpreter
/// line names: the programs that open a browser.
const OPENERS: &[&str] = &["open", "xdg-open", "gio", "sensible-browser", "www-browser"];

/// The interpreter `program`'s first line names through `env` (`#!/usr/bin/env node`,
/// `#!/usr/bin/env -S node --flag`): a bare name, never a path or an opener ([`OPENERS`]).
#[must_use]
pub fn env_interpreter(program: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = [0_u8; 256];
    let (mut file, _) = super::disk::versions::open_regular_file(program).ok()?;
    let n = file.read(&mut head).ok()?;
    let head = &head[..n];
    let line = head.strip_prefix(b"#!")?;
    let line = &line[..line.iter().position(|b| *b == b'\n').unwrap_or(line.len())];
    let line = std::str::from_utf8(line).ok()?;
    let mut words = line.split_whitespace();
    let env = words.next()?;
    if !(env == "env" || env.ends_with("/env")) {
        return None;
    }
    let mut name = None;
    while let Some(w) = words.next() {
        if w == "--" {
            name = words.find(|w| !w.contains('='));
            break;
        }
        if matches!(w, "-u" | "-C" | "-P" | "-a" | "--unset" | "--chdir") {
            let _ = words.next()?;
            continue;
        }
        if w.starts_with('-') || w.contains('=') {
            continue;
        }
        name = Some(w);
        break;
    }
    let name = name?;
    (!name.contains('/') && !matches!(name, "." | "..") && !OPENERS.contains(&name))
        .then(|| name.to_owned())
}

/// The executable file a bare `word` names on `path` (the parent's `PATH`, its absolute
/// entries in order); `None` for a word with a directory in it, or one found nowhere.
#[must_use]
pub fn on_path(word: &Path, path: &OsStr) -> Option<PathBuf> {
    let mut parts = word.components();
    let bare = matches!(
        (parts.next(), parts.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !bare {
        return None;
    }
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(word))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The program of `cli`'s sign-in on this machine: the binary atpkg's managed copy runs (never
/// its shim, which adds launch postures), else `~/.local/bin/<id>`, else the bare word, which
/// a sign-in finds on the parent's `PATH` ([`child_program`]).
#[must_use]
pub fn program_of(cli: Cli, home: &Path) -> PathBuf {
    if cli == Cli::Claude {
        return super::accounts_sync::resolve_claude_program(home);
    }
    let id = cli.id();
    if let Some(twin) = super::upgrade_drive::managed_agent_twin(id)
        && let Some(text) = super::accounts::read_bounded(&twin, 64 * 1024)
            .ok()
            .flatten()
        && let Some(exe) = super::upgrade_catalog::twin_binary(&text)
        && exe.is_file()
    {
        return exe;
    }
    let local = home.join(".local/bin").join(id);
    if local.is_file() {
        return local;
    }
    // A bare word: `sign_in_with` finds it on the parent's `PATH` ([`child_program`]).
    PathBuf::from(id)
}

/// What a sign-in hands its [`Approver`]: the page, held in memory only.
pub struct Handoff<'a> {
    /// The page's URL, on a pinned host.
    pub url: &'a str,
    /// The code the CLI printed, for a device flow.
    pub code: Option<&'a str>,
    /// How the page hands the login back.
    pub completion: Completion,
    /// The hosts its pages may be on.
    pub hosts: &'a [&'a str],
    /// The row's email.
    pub email: Option<&'a str>,
    /// Whether the CLI already said it is done, or closed its output.
    pub finished: &'a dyn Fn() -> bool,
    /// The original sign-in budget, including time the host slept.
    pub deadline: Deadline<'a>,
}

/// What an [`Approver`] made of the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// It approved it; `page` when a page it pressed on showed the row's email.
    Approved {
        /// The page showed the row's email.
        page: bool,
    },
    /// Approval could not run (for example, browser preparation failed or its
    /// login lifetime ended). This is recorded for automatic recovery, never
    /// turned into a request for a person to approve the sign-in.
    NotRun(String),
    /// It stopped where only the person can go on (an identity provider, a sign-up page, no
    /// mailbox): why, without the URL or code.
    NeedsYou(String),
    /// It stopped on the vendor's own sign-in (a page that waits on a person, another identity
    /// provider, a password): the row's browser profile is signed out of the vendor's site, so a
    /// person signs it in once (`accounts browser`): why, without the URL or code.
    NeedsSignIn(String),
    /// It refused the page (another code, another account): why.
    Refused(String),
    /// It waited on Cloudflare's bot check, which did not clear
    /// ([`super::headless_oauth::bot_check`]): why, naming Cloudflare and the page's host and
    /// path, never its text. Software never tries to pass one. A Claude flow may use the fleet
    /// issuer's interactive challenge service; if that is unavailable or does not finish, the
    /// sign-in ends as an error on record without asking the AI.
    BotCheck(String),
}

/// What a resume operation established before a new native child is considered.
pub enum Resume {
    /// No completion from this admitted attempt was used.
    Fresh,
    /// A retained credential is staged for native refresh; its earlier effect remains consumed.
    NativeSeed,
    /// A retained native completion will be finalized and revalidated.
    Completed(Outcome),
}

/// Approves a sign-in's page ([`drive`]): the headless browser, or a test's stand-in.
pub trait Approver {
    /// The admitted automatic operation, retained by actual pipe-reader work.
    fn work_scope(&self) -> Option<super::account_work::Scope> {
        None
    }
    /// Resume an unpublished native completion before requiring or starting a native child.
    /// A returned completion must be revalidated by `complete` before publication.
    fn resume(
        &mut self,
        _request: &Request<'_>,
        _deadline: Deadline<'_>,
    ) -> Result<Resume, String> {
        Ok(Resume::Fresh)
    }

    /// Bind private finalization state to this exact native attempt before it starts.
    /// This hook must not start provider authentication.
    fn begin(&mut self, _request: &Request<'_>) -> Result<(), String> {
        Ok(())
    }

    /// Approve `handoff`'s page, or say why not.
    fn approve(&mut self, handoff: &Handoff<'_>) -> Approval;

    /// Finalize private browser state only after the native child has completed.
    /// A failure prevents publication; non-success outcomes must discard pending state.
    fn complete(
        &mut self,
        _request: &Request<'_>,
        _outcome: &Outcome,
        _deadline: Deadline<'_>,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// How a driven sign-in ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The native child completed successfully, or a retained completion was revalidated.
    /// `email` from native completion or canonical revalidation, `page` where the
    /// approved page showed the row's email.
    SignedIn {
        /// The email the CLI's own report named.
        email: Option<String>,
        /// The page showed the row's email.
        page: bool,
    },
    /// A complete native response was saved, but work ended before fresh approval.
    /// The artifact remains in its original staging directory; this grants no adoption authority.
    CompletedHeld,
    /// It ended without saying it is done, or with another exit (`None`: a signal ended it).
    Exited(Option<i32>),
    /// The URL it named is not `https` on a pinned host: nothing was opened, and it was ended.
    OffPin,
    /// The approver refused its page, and it was ended.
    Refused(String),
    /// The approver stopped where only the person can go on, and it was ended.
    NeedsYou(String),
    /// The approver stopped on the vendor's sign-in page, which the row's browser profile is
    /// signed out of, and it was ended.
    NeedsSignIn(String),
    /// The approver waited on Cloudflare's bot check, which did not clear, and it was ended
    /// ([`Approval::BotCheck`]).
    BotCheck(String),
    /// Sign-in completed without succeeding (provider OAuth error, missing authorization code, or CLI failure).
    Failed,
    /// [`HANG`] ran out, and it was ended.
    Hung,
    /// No provider action or retained completion was used.
    NotRun(String),
    /// The CLI's sign-in named its page and the approver stopped before opening it (no browser to
    /// open it in, its download failed), so nothing was approved and the CLI was ended. A person
    /// reads this as [`Outcome::NotRun`]: no sign-in ran. Shared recovery reads it as
    /// [`Outcome::Unverified`]: the CLI's own request for that page may have reached the provider.
    PageNotOpened(String),
    /// A provider action may have occurred, but its result is unverified.
    /// This is not evidence that repeating the provider action is safe.
    Unverified(String),
}

impl Outcome {
    /// Its ledger word: what happened, never what was printed.
    #[must_use]
    pub fn word(&self) -> String {
        match self {
            Outcome::SignedIn { .. } => "signed-in".to_owned(),
            Outcome::CompletedHeld => "completed-held".to_owned(),
            Outcome::Exited(Some(code)) => format!("exited-{code}"),
            Outcome::Exited(None) => "signalled".to_owned(),
            Outcome::OffPin => "off-pin".to_owned(),
            Outcome::Refused(_) => "refused".to_owned(),
            Outcome::NeedsYou(_) => "needs-you".to_owned(),
            Outcome::NeedsSignIn(_) => "needs-sign-in".to_owned(),
            Outcome::BotCheck(_) => "bot-check".to_owned(),
            Outcome::Failed => "failed".to_owned(),
            Outcome::Hung => "hung".to_owned(),
            Outcome::NotRun(_) | Outcome::PageNotOpened(_) => "not-run".to_owned(),
            Outcome::Unverified(_) => "unverified".to_owned(),
        }
    }
}

/// A driven sign-in: how it ended and the host of the page it named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drove {
    /// How it ended.
    pub outcome: Outcome,
    /// The host of the page it named ([`https_host`]), where it named one.
    pub host: Option<String>,
}

impl Drove {
    /// The ledger's line for it under `flow`: host, flow and outcome.
    #[must_use]
    pub fn why(&self, flow: Flow) -> String {
        format!(
            "{} {} {}",
            self.host.as_deref().unwrap_or("no-page"),
            flow.as_str(),
            self.outcome.word()
        )
    }
}

/// One sign-in child ([`drive`]).
#[derive(Debug)]
pub struct Spec<'a> {
    /// The program.
    pub program: &'a Path,
    /// Arguments before [`Spec::args`] (a test binary's own `--exact <test>`).
    pub lead: &'a [OsString],
    /// The sign-in verb's arguments ([`child_args`]).
    pub args: &'a [String],
    /// The child's whole environment ([`child_env`]).
    pub env: &'a [(String, OsString)],
    /// Its working directory.
    pub cwd: &'a Path,
    /// What its lines say.
    pub grammar: &'a Grammar,
    /// How the page hands the login back.
    pub completion: Completion,
    /// The pinned hosts.
    pub hosts: &'a [&'a str],
    /// The row's email, for the approver.
    pub email: Option<&'a str>,
    /// The hang detector ([`HANG`]): [`drive`] measures it on both clocks ([`Deadline`]), so it
    /// counts the time the host slept.
    pub hang: Duration,
}

pub(super) fn end(child: &mut aterm_uds::spawnfd::Child) {
    end_with(child, aterm_uds::spawnfd::Child::wait);
}

fn end_with(
    child: &mut aterm_uds::spawnfd::Child,
    mut wait: impl FnMut(&mut aterm_uds::spawnfd::Child) -> std::io::Result<std::process::ExitStatus>,
) {
    // Only the unreaped actual child can reserve this dedicated group's numeric identity.
    // The bounded group operation is a signal attempt, not descendant-drain evidence.
    #[cfg(unix)]
    {
        if child.is_reaped() {
            return;
        }
        if child.owns_unreaped() {
            if let Ok(pid) = i32::try_from(child.id()) {
                let _ = aterm_uds::process::kill_group_until_empty(pid);
            }
            if child.owns_unreaped() {
                let _ = child.kill();
            }
        }
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    loop {
        #[cfg(unix)]
        if !child.owns_unreaped() && !child.is_reaped() {
            // ECHILD is sticky in Child: neither numeric retry nor a new signal is allowed.
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if wait(child).is_ok() {
            return;
        }
        // Work's deadline does not release the caller's admission/locks on uncertain wait.
        std::thread::sleep(Duration::from_millis(20));
    }
}

struct ProviderOwner(aterm_uds::spawnfd::Child);

impl std::ops::Deref for ProviderOwner {
    type Target = aterm_uds::spawnfd::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ProviderOwner {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ProviderOwner {
    fn drop(&mut self) {
        end(&mut self.0);
    }
}

fn spawn_reader(
    work: Option<super::account_work::Scope>,
    read: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let read: Box<dyn FnOnce() + Send> = match work {
        Some(scope) => Box::new(scope.wrap(read)),
        None => Box::new(read),
    };
    std::thread::Builder::new().spawn(read)
}

/// Runs `spec`'s child, hands its page to `approver` once its grammar named the URL (and, for a
/// device flow, the code), and waits for it to say it is done and exit, all within
/// [`Spec::hang`], which counts the time the host slept ([`Deadline`]): a sign-in the host
/// slept past is ended at its first look after the host wakes. The child is tied to this process ([`super::align::tie`]), so a quit, an exec
/// in its place, a crash or a `SIGKILL` of this process ends it too; where this process's
/// executable was replaced since it started, so the tie cannot hold, it is not run.
pub fn drive(spec: &Spec<'_>, approver: &mut dyn Approver) -> Drove {
    drive_until(spec, approver, Deadline::after(spec.hang))
}

fn drive_until(spec: &Spec<'_>, approver: &mut dyn Approver, deadline: Deadline<'_>) -> Drove {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = aterm_uds::spawnfd::command(spec.program);
    cmd.args(spec.lead)
        .args(spec.args)
        .env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_os_str())))
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);
    let not_run = |why: String| Drove {
        outcome: Outcome::NotRun(why),
        host: None,
    };
    if deadline.expired() {
        return not_run("native sign-in deadline expired".into());
    }
    let work = approver.work_scope();
    if work
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return not_run("automatic account work is cancelled".into());
    }
    if let Err(why) = super::align::tie(&mut cmd) {
        return not_run(why);
    }
    if work
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return not_run("automatic account work is cancelled".into());
    }
    if deadline.expired() {
        return not_run("native sign-in deadline expired".into());
    }
    let mut child = match cmd.spawn() {
        Ok(child) => ProviderOwner(child),
        Err(e) => return not_run(format!("{}: {e}", spec.program.display())),
    };
    if let Err(why) = super::align::tie_held() {
        end(&mut child);
        return Drove {
            outcome: Outcome::Unverified(why),
            host: None,
        };
    }
    let (tx, rx) = mpsc::channel::<Option<(usize, String)>>();
    let said_done = Arc::new(AtomicBool::new(false));
    let closed = Arc::new(AtomicUsize::new(0));
    let done_phrases: Vec<String> = spec.grammar.done.iter().map(|p| (*p).to_owned()).collect();
    let streams: [Option<Box<dyn Read + Send>>; 2] = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ];
    let mut open = 0usize;
    for (from, stream) in streams.into_iter().enumerate() {
        let Some(stream) = stream else {
            continue;
        };
        open += 1;
        let tx = tx.clone();
        let said_done = Arc::clone(&said_done);
        let closed = Arc::clone(&closed);
        let done_phrases = done_phrases.clone();
        let read = move || {
            read_lines(stream, |line| {
                let text = strip_ansi(&line);
                if done_phrases.iter().any(|p| text.contains(p.as_str())) {
                    said_done.store(true, Ordering::SeqCst);
                }
                let _ = tx.send(Some((from, line)));
            });
            closed.fetch_add(1, Ordering::SeqCst);
            let _ = tx.send(None);
        };
        if let Err(why) = spawn_reader(work.clone(), read) {
            return Drove {
                outcome: Outcome::Unverified(format!(
                    "native sign-in pipe reader could not start: {why}"
                )),
                host: None,
            };
        }
    }
    drop(tx);
    let streams = open;
    let finished = || said_done.load(Ordering::SeqCst) || closed.load(Ordering::SeqCst) >= streams;
    let mut scan = Scan::default();
    let mut handed = false;
    let mut page = false;
    let mut host = None;
    let mut status = None;
    let mut exited_at: Option<Instant> = None;
    let grace = output_grace();
    loop {
        if work
            .as_ref()
            .is_some_and(super::account_work::Scope::cancelled)
        {
            return Drove {
                outcome: Outcome::Unverified("automatic account work is cancelled".into()),
                host,
            };
        }
        let left = deadline.remaining().unwrap_or_default();
        if left.is_zero() {
            end(&mut child);
            return Drove {
                outcome: Outcome::Hung,
                host,
            };
        }
        match rx.recv_timeout(left.min(Duration::from_millis(50))) {
            Ok(Some((from, line))) => scan.feed_from(from, spec.grammar, &line),
            Ok(None) => open = open.saturating_sub(1),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => open = 0,
        }
        if deadline.expired() {
            end(&mut child);
            return Drove {
                outcome: Outcome::Hung,
                host,
            };
        }
        if !handed
            && let Some(url) = scan.url.clone()
            && (spec.completion == Completion::Loopback
                || spec.grammar.code.is_empty()
                || scan.code.is_some())
        {
            handed = true;
            host = https_host(&url);
            if !pinned(&url, spec.hosts) {
                end(&mut child);
                return Drove {
                    outcome: Outcome::OffPin,
                    host,
                };
            }
            let handoff = Handoff {
                url: &url,
                code: scan.code.as_deref(),
                completion: spec.completion,
                hosts: spec.hosts,
                email: spec.email,
                finished: &finished,
                deadline,
            };
            match approver.approve(&handoff) {
                Approval::Approved { page: shown } => {
                    page = shown;
                    if status.is_some() {
                        exited_at = Some(Instant::now());
                    }
                }
                Approval::NotRun(why) => {
                    end(&mut child);
                    return Drove {
                        outcome: Outcome::PageNotOpened(why),
                        host,
                    };
                }
                Approval::NeedsYou(why) => {
                    end(&mut child);
                    return Drove {
                        outcome: Outcome::NeedsYou(why),
                        host,
                    };
                }
                Approval::NeedsSignIn(why) => {
                    end(&mut child);
                    return Drove {
                        outcome: Outcome::NeedsSignIn(why),
                        host,
                    };
                }
                Approval::Refused(why) => {
                    end(&mut child);
                    return Drove {
                        outcome: Outcome::Refused(why),
                        host,
                    };
                }
                Approval::BotCheck(why) => {
                    end(&mut child);
                    return Drove {
                        outcome: Outcome::BotCheck(why),
                        host,
                    };
                }
            }
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(exited)) => {
                    status = Some(exited);
                    exited_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(_) => {
                    end(&mut child);
                    return Drove {
                        outcome: Outcome::Exited(None),
                        host,
                    };
                }
            }
        }
        if open == 0 && status.is_some() {
            break;
        }
        if exited_at.is_some_and(|at| at.elapsed() >= grace) {
            break;
        }
    }
    while let Ok(msg) = rx.try_recv() {
        if let Some((from, line)) = msg {
            scan.feed_from(from, spec.grammar, &line);
        }
    }
    let code = status.and_then(|s| s.code());
    let outcome = match code {
        Some(0) if handed && scan.done => Outcome::SignedIn {
            email: scan.email,
            page,
        },
        other => Outcome::Exited(other),
    };
    Drove { outcome, host }
}

/// Opaque identity of an admitted native recovery attempt. Pool writers bind
/// its row and parent facts; shared-root admissions bind the exact consumed
/// intent. The key alone grants no authority to perform or publish root effects.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RecoveryKey(pub(super) [u8; 32]);

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey(opaque)")
    }
}

type RecoveryCheck = dyn Fn(Option<[u8; 32]>) -> Result<(), String> + Send + Sync;

#[derive(Clone, Copy, PartialEq, Eq)]
enum RecoveryDomain {
    SharedRoot,
    #[cfg(unix)]
    Pool,
}

/// Live authority retained alongside an admitted recovery key. Constructing a key
/// alone cannot authorize root-scoped browser work or session publication.
#[derive(Clone)]
pub struct RecoveryAdmission {
    domain: RecoveryDomain,
    key: RecoveryKey,
    check: std::sync::Arc<RecoveryCheck>,
    native_entry: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    kimi: std::sync::Arc<std::sync::Mutex<Option<PoolKimiProof>>>,
}

/// Immutable validation authority, without custody of a subsequently retained proof.
/// A proof may retain this view without forming a cycle through its admission's slot.
#[cfg(unix)]
#[derive(Clone)]
pub(super) struct RecoveryAuthority {
    domain: RecoveryDomain,
    key: RecoveryKey,
    check: std::sync::Arc<RecoveryCheck>,
    native_entry: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(unix)]
impl RecoveryAuthority {
    pub(super) fn key(&self) -> RecoveryKey {
        self.key
    }
    pub(super) fn is_shared_root(&self) -> bool {
        self.domain == RecoveryDomain::SharedRoot
    }
    pub(super) fn current(&self, principal: Option<[u8; 32]>) -> Result<(), String> {
        (self.check)(principal)
    }
}
#[cfg(unix)]
impl std::fmt::Debug for RecoveryAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryAuthority(opaque)")
    }
}
#[cfg(unix)]
impl PartialEq for RecoveryAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.domain == other.domain
            && self.key == other.key
            && std::sync::Arc::ptr_eq(&self.check, &other.check)
            && std::sync::Arc::ptr_eq(&self.native_entry, &other.native_entry)
    }
}
#[cfg(unix)]
impl Eq for RecoveryAuthority {}

#[cfg(unix)]
struct PoolKimiProof {
    native: super::cli::shared_provider::KimiProof,
    _claim: std::sync::Arc<super::browser_window::ProfileLock>,
}

impl RecoveryAdmission {
    #[cfg(unix)]
    pub(super) fn authority(&self) -> RecoveryAuthority {
        RecoveryAuthority {
            domain: self.domain,
            key: self.key,
            check: self.check.clone(),
            native_entry: self.native_entry.clone(),
        }
    }
    pub(super) fn new(
        key: RecoveryKey,
        native_entry: bool,
        check: impl Fn(Option<[u8; 32]>) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            domain: RecoveryDomain::SharedRoot,
            key,
            check: std::sync::Arc::new(check),
            native_entry: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(native_entry)),
            #[cfg(unix)]
            kimi: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[cfg(unix)]
    pub(super) fn pool(
        key: RecoveryKey,
        check: impl Fn(Option<[u8; 32]>) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            domain: RecoveryDomain::Pool,
            key,
            check: std::sync::Arc::new(check),
            native_entry: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            kimi: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    #[cfg(unix)]
    pub(super) fn is_shared_root(&self) -> bool {
        self.domain == RecoveryDomain::SharedRoot
    }

    #[cfg(unix)]
    pub(super) fn retain_kimi(
        &self,
        native: super::cli::shared_provider::KimiProof,
        claim: std::sync::Arc<super::browser_window::ProfileLock>,
    ) -> Result<(), String> {
        use super::cli::shared_provider::LoginProof as _;
        if self.is_shared_root() {
            return Err("pool Kimi proof has a shared-root admission".into());
        }
        self.current(Some(native.principal()))?;
        let mut kept = self
            .kimi
            .lock()
            .map_err(|_| "pool Kimi proof custody is unavailable")?;
        if kept.is_some() {
            return Err("pool Kimi proof was already retained".into());
        }
        *kept = Some(PoolKimiProof {
            native,
            _claim: claim,
        });
        Ok(())
    }

    /// Bounded local evidence only; safe inside the pool publication transaction.
    #[cfg(unix)]
    pub(super) fn kimi_principal(
        &self,
        staging: &Path,
        region: crate::ai::connector::Region,
        expected: &str,
    ) -> Result<Option<[u8; 32]>, String> {
        use super::cli::shared_provider::LoginProof as _;
        let kept = self
            .kimi
            .lock()
            .map_err(|_| "pool Kimi proof custody is unavailable")?;
        let Some(kept) = kept.as_ref() else {
            return Ok(None);
        };
        kept.native.check_native(staging, region, expected)?;
        Ok(Some(kept.native.principal()))
    }

    pub(super) fn key(&self) -> RecoveryKey {
        self.key
    }

    fn enter_native(&self) -> bool {
        self.native_entry
            .compare_exchange(
                true,
                false,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    pub(super) fn current(&self, principal: Option<[u8; 32]>) -> Result<(), String> {
        (self.check)(principal)
    }
}

impl std::fmt::Debug for RecoveryAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryAdmission(opaque)")
    }
}
impl PartialEq for RecoveryAdmission {
    fn eq(&self, other: &Self) -> bool {
        self.domain == other.domain
            && self.key == other.key
            && std::sync::Arc::ptr_eq(&self.check, &other.check)
    }
}
impl Eq for RecoveryAdmission {}

/// One sign-in a renewal asks for ([`sign_in_live`]): the CLI, its route and pinned hosts for
/// the row, and the staging directory it signs into.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Optional admitted recovery identity; manual and unscoped flows carry none.
    pub recovery: Option<RecoveryKey>,
    /// A live root check, required for root-scoped recovery and browser publication.
    pub admission: Option<&'a RecoveryAdmission>,
    /// The CLI.
    pub cli: Cli,
    /// The row's route ([`SignIn::route`]).
    pub route: &'static Route,
    /// The row.
    pub row: &'a super::accounts::Row,
    /// The staging directory: the child's home.
    pub staging: &'a Path,
    /// The CLI's store, whose directory holds the opener-free `PATH`.
    pub store: &'a super::accounts::Store,
    /// The route's hosts for the row ([`crate::ai::connector::Hosts::of`]).
    pub hosts: &'static [&'static str],
}

impl Request<'_> {
    pub(super) fn check_admission(&self) -> Result<(), String> {
        if let Some(admission) = self.admission {
            if self.recovery != Some(admission.key()) {
                return Err("native recovery admission differs from its key".into());
            }
            admission.current(None)?;
        }
        Ok(())
    }

    fn signin(&self) -> Option<&'static SignIn> {
        self.cli.accounts().signin.as_ref()
    }
}

/// The macOS sandbox a [`SignIn::no_desk`] CLI's sign-in child runs under ([`NO_DESK`]).
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// The [`SANDBOX_EXEC`] profile that keeps a sign-in child off this machine's desk, as the lab
/// measured it on Grok Build 1.0.70 (design §5 (g)): LaunchServices, Apple events, the
/// pasteboard, `open`, `osascript` and the launch of an installed app denied, everything else
/// allowed, so the CLI's own browser open fails and it still prints its URL and code.
pub const NO_DESK: &str = concat!(
    r##"(version 1)(allow default)(deny lsopen)(deny appleevent-send)"##,
    r##"(deny mach-lookup (global-name-regex #"launchservices") "##,
    r##"(global-name-regex #"^com\.apple\.lsd\.") (global-name-regex #"appleevents") "##,
    r##"(global-name-regex #"^com\.apple\.coreservices\.") "##,
    r##"(global-name-regex #"CoreServicesUIAgent") (global-name-regex #"pasteboard"))"##,
    r##"(deny process-exec (literal "/usr/bin/open") (literal "/usr/bin/osascript") "##,
    r##"(subpath "/Applications") (subpath "/System/Applications") "##,
    r##"(subpath "/System/Cryptexes") (subpath "/System/Volumes/Preboot/Cryptexes"))"##,
);

/// The program a sandbox is tried on before a sign-in runs under it ([`sandbox_refusal`]).
const SANDBOX_PROBE: &str = "/usr/bin/true";

/// How long that try may take.
#[cfg(not(test))]
const SANDBOX_PROBE_WAIT: Duration = Duration::from_secs(20);
/// How long that try may take: the unit tests try the real `sandbox-exec`, which runs
/// `/usr/bin/true` and exits, so a try a loaded machine holds is waited for by the hang detector,
/// not read as a refusal.
#[cfg(test)]
const SANDBOX_PROBE_WAIT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Why `sandbox` cannot run a child under [`NO_DESK`] here: it is tried on [`SANDBOX_PROBE`],
/// and anything but its exit 0 within [`SANDBOX_PROBE_WAIT`] is a refusal. `sandbox-exec` exits
/// 71 before its child starts where this process already runs under another sandbox, which
/// macOS does not let it nest.
#[must_use]
pub fn sandbox_refusal(sandbox: &Path) -> Option<String> {
    use aterm_uds::spawnfd::Stdio;
    let mut cmd = aterm_uds::spawnfd::command(sandbox);
    cmd.args(no_desk_argv(Path::new(SANDBOX_PROBE), &[]))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => ProviderOwner(child),
        Err(e) => return Some(format!("{} did not start ({e})", sandbox.display())),
    };
    let now = Instant::now();
    let until = now
        .checked_add(SANDBOX_PROBE_WAIT)
        .unwrap_or_else(|| now + Duration::from_secs(60));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                end(&mut child);
                return Some(format!(
                    "{} did not finish a try within {} s",
                    sandbox.display(),
                    SANDBOX_PROBE_WAIT.as_secs()
                ));
            }
            Err(e) => {
                return Some(format!(
                    "{} could not be waited for ({e})",
                    sandbox.display()
                ));
            }
        }
    };
    match status.code() {
        Some(0) => None,
        Some(code) => Some(format!(
            "{} exited {code} on a try, so it cannot apply its sandbox here (it cannot where \
             this process already runs under another sandbox)",
            sandbox.display()
        )),
        None => Some(format!(
            "{} was ended by a signal on a try",
            sandbox.display()
        )),
    }
}

/// The arguments [`SANDBOX_EXEC`] takes to run `program` (after `lead`) under [`NO_DESK`].
#[must_use]
pub fn no_desk_argv(program: &Path, lead: &[OsString]) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("-p"),
        OsString::from(NO_DESK),
        program.as_os_str().to_os_string(),
    ];
    argv.extend(lead.iter().cloned());
    argv
}

/// `req`'s sign-in run as `program` (after `lead`), its `PATH` `path` (else the opener-free
/// one, where the CLI's [`SignIn::no_opener`]), its other variables from `var`
/// ([`child_env`]), its page handed to `approver`, within `hang`, which counts the time the host
/// slept ([`Deadline`]). Where the CLI's
/// [`SignIn::no_desk`] names this system, the child runs under the sandbox `no_desk` names
/// ([`no_desk_argv`]). A retained completion is revalidated before native setup. Otherwise
/// no child runs where its sandbox is not a file, cannot apply here ([`sandbox_refusal`]),
/// or the program is not installed.
pub fn sign_in_with(
    req: &Request<'_>,
    (program, lead): (&Path, &[OsString]),
    path: Option<&OsStr>,
    no_desk: Option<&Path>,
    var: &dyn Fn(&str) -> Option<OsString>,
    approver: &mut dyn Approver,
    hang: Duration,
) -> Drove {
    sign_in_until(
        req,
        (program, lead),
        path,
        no_desk,
        var,
        approver,
        Deadline::after(hang),
    )
}

fn sign_in_until(
    req: &Request<'_>,
    (program, lead): (&Path, &[OsString]),
    path: Option<&OsStr>,
    no_desk: Option<&Path>,
    var: &dyn Fn(&str) -> Option<OsString>,
    approver: &mut dyn Approver,
    deadline: Deadline<'_>,
) -> Drove {
    let not_run = |why: String| Drove {
        outcome: Outcome::NotRun(why),
        host: None,
    };
    let work = approver.work_scope();
    if work
        .as_ref()
        .is_some_and(super::account_work::Scope::cancelled)
    {
        return not_run("automatic account work is cancelled".into());
    }
    let Some(signin) = req.signin() else {
        return not_run("this CLI has no sign-in in this build".to_owned());
    };
    if matches!(req.route.drive, Drive::NativeGoogle) {
        return not_run("native Google login requires its typed grant driver".into());
    }
    if deadline.expired() {
        return not_run("native sign-in deadline expired".into());
    }
    if !matches!(req.route.drive, Drive::Lines { .. }) && deadline.wake_parent().is_none() {
        return not_run("the injected sign-in clock has no transferable native deadline".into());
    }
    if req
        .admission
        .is_some_and(|admission| !admission.enter_native())
    {
        return Drove {
            outcome: Outcome::Unverified(
                "retained native admission is read-only or already entered".into(),
            ),
            host: None,
        };
    }
    if let Err(why) = req.check_admission() {
        return not_run(why);
    }
    let retained = match approver.resume(req, deadline) {
        Ok(Resume::Completed(outcome)) => {
            return complete_sign_in(
                req,
                approver,
                Drove {
                    outcome,
                    host: None,
                },
                deadline,
                true,
                CompletionCustody::Ordinary,
            );
        }
        Ok(Resume::Fresh) => false,
        Ok(Resume::NativeSeed) => true,
        Err(why) => {
            return Drove {
                outcome: Outcome::Unverified(why),
                host: None,
            };
        }
    };
    let not_run = |why: String| Drove {
        outcome: if retained {
            Outcome::Unverified(why)
        } else {
            Outcome::NotRun(why)
        },
        host: None,
    };
    if deadline.expired() {
        return not_run("native sign-in deadline expired".into());
    }
    let Drive::Lines {
        grammar,
        completion,
        ..
    } = &req.route.drive
    else {
        let Some(native_deadline) = deadline.wake_parent() else {
            return not_run(
                "the injected sign-in clock has no transferable native deadline".into(),
            );
        };
        if native_deadline.passed() {
            return not_run("native sign-in deadline expired".into());
        }
        if let Err(why) = approver.begin(req).and_then(|()| req.check_admission()) {
            return not_run(why);
        }
        let drove = super::codex_app_login::login_in_home(
            req.staging,
            approver,
            req.hosts,
            req.row.email.as_deref(),
            native_deadline,
        );
        let custody = if matches!(drove.outcome, Outcome::SignedIn { .. }) {
            CompletionCustody::NativeSaved
        } else {
            CompletionCustody::Ordinary
        };
        return complete_sign_in(req, approver, drove, deadline, retained, custody);
    };
    let sandbox = if signin.no_desk.contains(&std::env::consts::OS) {
        let Some(sandbox) = no_desk.filter(|sandbox| sandbox.is_file()) else {
            return not_run(format!(
                "{} opens this machine's default browser through the OS, and the sandbox that \
                 stops it ({}) is not here; nothing was run",
                req.cli.connector().product,
                no_desk.unwrap_or(Path::new(SANDBOX_EXEC)).display()
            ));
        };
        if let Some(why) = sandbox_refusal(sandbox) {
            return not_run(format!(
                "{} opens this machine's default browser through the OS, and the sandbox that \
                 stops it cannot run here: {why}; run it from a terminal that is not sandboxed; \
                 nothing was run",
                req.cli.connector().product
            ));
        }
        Some(sandbox)
    } else {
        None
    };
    let (program, path, _opener_free) =
        match child_program(program, path, signin.no_opener, &req.store.dir, var) {
            Ok(child) => child,
            Err(e) => return not_run(format!("no private PATH could be made: {e}")),
        };
    if sandbox.is_some() && !program.is_file() {
        return not_run(format!(
            "`{}` is not installed here (found neither at that path nor on `PATH`)",
            program.display()
        ));
    }
    let sandboxed: Vec<OsString>;
    let (program, lead) = match sandbox {
        Some(sandbox) => {
            sandboxed = no_desk_argv(&program, lead);
            (sandbox, sandboxed.as_slice())
        }
        None => (program.as_path(), lead),
    };
    let env = child_env(req.cli, signin, req.staging, &path, var);
    let email = req.row.email.as_deref();
    let args = child_args(req.route, req.row.vendor.region);
    if deadline.expired() {
        return not_run("native sign-in deadline expired".into());
    }
    if let Err(why) = approver.begin(req).and_then(|()| req.check_admission()) {
        return not_run(why);
    }
    let drove = drive_until(
        &Spec {
            program,
            lead,
            args: &args,
            env: &env,
            cwd: req.staging,
            grammar,
            completion: *completion,
            hosts: req.hosts,
            email,
            hang: deadline.remaining().unwrap_or_default(),
        },
        approver,
        deadline,
    );
    complete_sign_in(
        req,
        approver,
        drove,
        deadline,
        retained,
        CompletionCustody::Ordinary,
    )
}

/// Evidence supplied only by the native Codex return boundary after its durable save.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompletionCustody {
    Ordinary,
    NativeSaved,
}

fn complete_sign_in(
    req: &Request<'_>,
    approver: &mut dyn Approver,
    mut drove: Drove,
    deadline: Deadline<'_>,
    retained: bool,
    custody: CompletionCustody,
) -> Drove {
    if drove.outcome == Outcome::CompletedHeld {
        return drove;
    }
    if retained && let Outcome::NotRun(why) | Outcome::PageNotOpened(why) = &drove.outcome {
        drove.outcome = Outcome::Unverified(why.clone());
    }
    let native_saved = custody == CompletionCustody::NativeSaved;
    let stopped = |approver: &dyn Approver| {
        deadline.expired() || approver.work_scope().is_some_and(|scope| scope.cancelled())
    };
    if native_saved && stopped(approver) {
        drove.outcome = Outcome::CompletedHeld;
        return drove;
    }
    if let Err(why) = req.check_admission() {
        drove.outcome = if native_saved {
            Outcome::CompletedHeld
        } else {
            Outcome::Unverified(why)
        };
        return drove;
    }
    if native_saved && stopped(approver) {
        drove.outcome = Outcome::CompletedHeld;
        return drove;
    }
    if let Err(why) = approver.complete(req, &drove.outcome, deadline) {
        drove.outcome = if native_saved {
            Outcome::CompletedHeld
        } else {
            Outcome::Unverified(why)
        };
    }
    if native_saved && stopped(approver) {
        drove.outcome = Outcome::CompletedHeld;
    }
    drove
}

/// `req`'s sign-in on this machine: the CLI's own binary ([`program_of`]), under
/// [`SANDBOX_EXEC`] where its [`SignIn::no_desk`] names this system, the headless browser
/// approving its page ([`super::headless_oauth::VendorApprover`]), within [`HANG`].
pub fn sign_in_live(
    req: &Request<'_>,
    home: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Drove {
    sign_in_live_scoped(req, home, var, None)
}

/// Drive a live sign-in with the caller's existing automatic operation lifetime.
pub fn sign_in_live_scoped(
    req: &Request<'_>,
    home: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
    work: Option<super::account_work::Scope>,
) -> Drove {
    let program = program_of(req.cli, home);
    let mut approver = super::headless_oauth::VendorApprover::new(
        req.cli,
        &req.row.label,
        req.row.email.as_deref(),
        &req.store.shared_dir(),
        Some(home),
    )
    .with_work(work);
    sign_in_with(
        req,
        (&program, &[]),
        None,
        Some(Path::new(SANDBOX_EXEC)),
        var,
        &mut approver,
        HANG,
    )
}

/// The private `PATH` directory a [`login_command`] holds, removed when it drops.
pub type LoginPath = Option<aterm_tempfile::TempDir>;

/// Whether `route`'s page hands the login back on a loopback callback on the machine the CLI
/// runs on (a [`Completion::Loopback`] route, and Codex's native PKCE browser route, whose
/// `redirect_uri` is `http://127.0.0.1:1455/…`): a browser on another machine cannot finish it.
#[must_use]
pub fn calls_back_here(route: &Route) -> bool {
    match &route.drive {
        Drive::Lines { completion, .. } => *completion == Completion::Loopback,
        Drive::CodexPkce => true,
        Drive::NativeGoogle => false,
    }
}

/// The route `accounts login` runs for `req` ([`login_command`]): the row's own, except over
/// SSH (`over_ssh`) where that one calls back to this machine ([`calls_back_here`]), which the
/// person's browser, on their own machine, cannot reach: there it is the CLI's device route
/// (Codex's `login --device-auth`, Grok Build's), whose page any browser finishes.
/// # Errors
/// Over SSH, a route that calls back here of a CLI that has no device route.
pub fn login_route(req: &Request<'_>, over_ssh: bool) -> Result<&'static Route, String> {
    if !over_ssh || !calls_back_here(req.route) {
        return Ok(req.route);
    }
    req.signin()
        .and_then(|signin| signin.route(Some(Flow::Device)))
        .ok_or_else(|| {
            format!(
                "over SSH, {}'s browser sign-in calls back to this machine, which your browser \
                 cannot reach, and it has no device sign-in; run it at this machine's desk",
                req.cli.connector().product
            )
        })
}

/// A sign-in a person runs on this terminal, as [`off_this_desk`] makes it.
#[derive(Debug)]
pub struct DeskSafe {
    /// What runs: the CLI, or [`SANDBOX_EXEC`] in front of it.
    pub program: PathBuf,
    /// Its arguments: the CLI's own, behind [`no_desk_argv`]'s where it runs under the sandbox.
    pub args: Vec<OsString>,
    /// The child's `PATH`: this process's, or the opener-free directory [`Self::held`] holds.
    pub path: OsString,
    /// The opener-free `PATH`'s directory, removed when it drops.
    pub held: LoginPath,
}

/// `program args…` as a sign-in a person runs on this terminal, where the CLI shows its own URL
/// and code. At the desk it runs as given with this process's `PATH`, and may open the
/// person's browser. Over SSH (`over_ssh`) a browser it opened would be this machine's, on a
/// desk nobody is at, not the person's, so there a CLI whose `signin` is [`SignIn::no_opener`]
/// gets the opener-free `PATH` ([`child_program`], whose directory [`DeskSafe::held`] holds),
/// and one whose [`SignIn::no_desk`] names this system runs under `no_desk` ([`NO_DESK`]), its
/// program found on this process's `PATH` first; it prints its URL for the person to open where
/// they are. `product` names the CLI in a refusal. `accounts login` ([`login_command`]) and
/// `aterm ai login` (`crate::ai::login::sign_in`) both run their CLI so.
/// # Errors
/// Over SSH, a no-desk CLI's sandbox is missing or cannot run here ([`sandbox_refusal`]),
/// which is checked before anything is made, or no private `PATH` could be made.
pub fn off_this_desk(
    signin: Option<&SignIn>,
    product: &str,
    (program, args): (PathBuf, Vec<OsString>),
    (over_ssh, no_desk): (bool, Option<&Path>),
    store_dir: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<DeskSafe, String> {
    let parent = var("PATH").unwrap_or_default();
    if !over_ssh {
        return Ok(DeskSafe {
            program,
            args,
            path: parent,
            held: None,
        });
    }
    let sandbox = if signin.is_some_and(|s| s.no_desk.contains(&std::env::consts::OS)) {
        let Some(sandbox) = no_desk.filter(|sandbox| sandbox.is_file()) else {
            return Err(format!(
                "over SSH, {product} would open this machine's default browser, and the sandbox \
                 that stops it ({}) is not here",
                no_desk.unwrap_or(Path::new(SANDBOX_EXEC)).display()
            ));
        };
        if let Some(why) = sandbox_refusal(sandbox) {
            return Err(format!(
                "over SSH, {product} would open this machine's default browser, and the sandbox \
                 that stops it cannot run here: {why}"
            ));
        }
        Some(sandbox)
    } else {
        None
    };
    let (program, path, held) = if signin.is_some_and(|s| s.no_opener) {
        child_program(&program, None, true, store_dir, var)
            .map_err(|e| format!("no private PATH could be made: {e}"))?
    } else {
        (program, parent.clone(), None)
    };
    let Some(sandbox) = sandbox else {
        return Ok(DeskSafe {
            program,
            args,
            path,
            held,
        });
    };
    let resolved = on_path(&program, &parent).unwrap_or(program);
    let mut wrapped = no_desk_argv(&resolved, &[]);
    wrapped.extend(args);
    Ok(DeskSafe {
        program: sandbox.to_path_buf(),
        args: wrapped,
        path,
        held,
    })
}

/// `accounts login <label> --cli <id>`'s command: `req`'s sign-in on this terminal, kept off
/// this machine's desk over SSH (`over_ssh`) as [`off_this_desk`] says, its environment
/// [`child_env`]'s with the `PATH` that gives; a route that calls back to this machine runs as
/// the CLI's device route there ([`login_route`]). A Codex browser route runs `codex login`,
/// the native PKCE loopback driver being the harness's alone.
/// # Errors
/// Over SSH, the route calls back here and the CLI has no device route ([`login_route`]), or
/// [`off_this_desk`] refuses (a no-desk CLI's sandbox is missing or cannot run here, or no
/// private `PATH` could be made).
pub fn login_command(
    req: &Request<'_>,
    home: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
    (over_ssh, no_desk): (bool, Option<&Path>),
) -> Result<(aterm_uds::spawnfd::Command, LoginPath), String> {
    let route = login_route(req, over_ssh)?;
    let signin = req.signin();
    let args: Vec<OsString> = match route.drive {
        Drive::NativeGoogle => {
            return Err("native Google login requires its typed grant driver".into());
        }
        Drive::CodexPkce => vec![OsString::from("login")],
        Drive::Lines { .. } => child_args(route, req.row.vendor.region)
            .into_iter()
            .map(OsString::from)
            .collect(),
    };
    let kept = off_this_desk(
        signin,
        req.cli.connector().product,
        (program_of(req.cli, home), args),
        (over_ssh, no_desk),
        &req.store.dir,
        var,
    )?;
    let env = signin
        .map(|signin| child_env(req.cli, signin, req.staging, &kept.path, var))
        .unwrap_or_default();
    let mut cmd = aterm_uds::spawnfd::command(&kept.program);
    cmd.args(&kept.args)
        .env_clear()
        .envs(
            env.iter()
                .filter(|(k, _)| k != "TERM" && k != "NO_COLOR")
                .map(|(k, v)| (k.as_str(), v.as_os_str())),
        )
        .current_dir(req.staging);
    if let Some(term) = var("TERM") {
        cmd.env("TERM", term);
    }
    Ok((cmd, kept.held))
}

#[cfg(test)]
#[path = "login_drive_tests.rs"]
mod tests;
