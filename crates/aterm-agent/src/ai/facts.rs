// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine and CLI state inspection for the AI doctor.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::connector::Connector;
use super::store::{Record, Records};

/// What one child run left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// Its exit code; `None` when a signal took it.
    pub exit: Option<i32>,
    /// Its standard output: the tail, bounded.
    pub stdout: String,
    /// Its standard error, read to its end: the tail, bounded.
    pub stderr: String,
}

/// Why a run produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunFailed {
    /// It ran past its budget and was killed with its process group.
    Timeout,
    /// It would not start, or could not be waited for.
    NotRun(String),
}

/// One bounded child run.
#[derive(Debug, Clone)]
pub struct Job<'a> {
    /// What runs: an absolute path, never a PATH lookup.
    pub program: &'a Path,
    /// Its arguments.
    pub args: &'a [String],
    /// Its whole environment.
    pub env: &'a [(String, String)],
    /// A file for stdin, or closed.
    pub stdin: Option<&'a Path>,
    /// Its working directory.
    pub cwd: Option<&'a Path>,
    /// How long it may run.
    pub budget: Duration,
}

/// The child processes the AI layer starts.
pub trait Runner: Sync {
    /// Runs `job` with captured output, killed with its process group at its budget.
    ///
    /// # Errors
    ///
    /// Why it produced nothing ([`RunFailed`]).
    fn run(&self, job: &Job<'_>) -> Result<Ran, RunFailed>;

    /// Runs `program args…` on this terminal and returns its exit code.
    ///
    /// # Errors
    ///
    /// Why it could not be started.
    fn terminal(
        &self,
        program: &Path,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
    ) -> Result<i32, String>;
}

/// The shipped [`Runner`].
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveRunner;

/// How much of each stream is kept.
pub const OUTPUT_KEEP: usize = aterm_uds::deadline::OUTPUT_KEEP;

fn tail(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(OUTPUT_KEEP)..]).into_owned()
}

fn open_regular_stdin(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(not(unix))]
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stdin path is a symbolic link",
        ));
    }
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = open.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stdin path is not a regular file",
        ));
    }
    Ok(file)
}

impl Runner for LiveRunner {
    fn run(&self, job: &Job<'_>) -> Result<Ran, RunFailed> {
        let mut cmd = aterm_uds::spawnfd::command(job.program);
        cmd.args(job.args).env_clear();
        for (k, v) in job.env {
            cmd.env(k, v);
        }
        if let Some(dir) = job.cwd {
            cmd.current_dir(dir);
        }
        let feed = match job.stdin {
            Some(path) => aterm_uds::spawnfd::Stdio::from(
                open_regular_stdin(path).map_err(|e| RunFailed::NotRun(e.to_string()))?,
            ),
            None => aterm_uds::spawnfd::Stdio::null(),
        };
        let now = Instant::now();
        let deadline = now
            .checked_add(job.budget)
            .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
        match aterm_uds::deadline::output_until_from(&mut cmd, feed, deadline) {
            Ok(out) => Ok(Ran {
                exit: out.status.code(),
                stdout: tail(&out.stdout),
                stderr: tail(&out.stderr),
            }),
            Err(aterm_uds::deadline::RunError::TimedOut) => Err(RunFailed::Timeout),
            Err(e) => Err(RunFailed::NotRun(e.to_string())),
        }
    }

    fn terminal(
        &self,
        program: &Path,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&Path>,
    ) -> Result<i32, String> {
        let mut cmd = aterm_uds::spawnfd::command(program);
        cmd.args(args).env_clear();
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        crate::harness::cli::terminal_run(cmd)
    }
}

/// The managed copy of a CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    /// The atpkg shim that runs the selected store or development copy.
    pub path: PathBuf,
    /// Its version label, from a linked package's product/client metadata or atpkg's record.
    pub version: Option<String>,
}

/// Why a CLI is not to be used here, where that comes from outside `[ai]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// No build for this platform.
    Unavailable,
    /// `[packages] exclude` names it.
    Excluded,
    /// Uninstalled here on purpose (`aterm pkg uninstall`).
    Removed,
    /// Removal evidence could not be read; this grants no permission to install.
    RemovalUnreadable,
}

/// Accounts v2's view of the Claude row that serves this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAccounts {
    /// The account store whose current selection serves a review, when readable.
    pub store: Option<crate::harness::accounts::Store>,
    /// The configured roster path, including an absent file's path.
    pub roster: Option<PathBuf>,
    /// The serving row's label (the own row's where the roster is refused or the records cannot
    /// be read).
    pub label: String,
    /// Whether `accounts.toml` lists the row.
    pub listed: bool,
    /// Why `accounts.toml` is refused, when it is.
    pub refused: Option<String>,
    /// Why accounts v2's records cannot be read, where `accounts.toml` is not refused and they
    /// cannot: its state or login bindings, and the folder that holds them (`state.json: not
    /// JSON (…), in <dir>`). The view then yields only that, as for a refused `accounts.toml`.
    pub unread: Option<String>,
    /// Why the selected credential cannot execute, independently of its login health.
    pub launch_refused: Option<String>,
    /// Why no local sign-in can repair the selected account, with no eligible alternative.
    pub sign_in_refused: Option<String>,
    /// Whether the row has an email and renews by itself.
    pub renews: bool,
    /// The login directory a renewal bound the row to, or `None` for the default login.
    pub login: Option<String>,
    /// Whether a login was made in that bound directory (`accounts::login_made`).
    pub made: bool,
    /// `accounts::LoginState::as_str` where the row renews, or `"expired"` for a
    /// plain row that the account store records as signed out.
    pub state: Option<&'static str>,
    /// The forecast end, when due.
    pub renew_by: Option<i64>,
    /// The usage limit's end, when limited.
    pub limited_until: Option<i64>,
    /// The newest turn served under it.
    pub served_at: Option<i64>,
    /// When a renewal last bound it, in unix seconds.
    pub renewed_at: Option<i64>,
    /// When accounts v2 recorded it signed out, where it is: a login wall, a displacement by the
    /// vendor's own sign-in, or a check that found no login all write the same word.
    pub signed_out_at: Option<i64>,
    /// The next not-enrolled or signed-out row to sign into when the serving row is limited,
    /// when that row has an email.
    pub next_sign_in: Option<String>,
    /// aterm's own sign-in of the row a sign-in renews (`next_sign_in`, else this row) that
    /// failed and stands on record.
    pub tried: Option<Tried>,
    /// Whether a route signs that row in with nobody at this Mac: a mailbox answers its email's
    /// sign-in, or its own browser profile is driven in a headless browser
    /// ([`crate::harness::cli::claude_row_can_run_headless`], the test `aterm harness accounts
    /// renew` makes with no terminal). `false` where the row renews by no email.
    pub unattended: bool,
    /// The switch that keeps aterm's own sign-ins off here, where one does
    /// ([`crate::harness::accounts_sync::sign_ins_off_for`]).
    pub off: Option<&'static str>,
}

/// An automatic sign-in of a row that failed and stands on record (`logins.json`'s
/// `sign_in_attempt`, [`crate::harness::accounts::SignInAttempt`]): what aterm itself tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tried {
    /// How it ended: `no-route`, `failed` or `blocked`.
    pub outcome: &'static str,
    /// The routes its ladder tried and what each found, one bounded line.
    pub routes: String,
    /// Since when it has ended so (Unix seconds).
    pub since: i64,
}

/// `label`'s failed automatic sign-in on record in `logins`, where one stands.
fn tried_of(logins: &crate::harness::accounts::Logins, label: &str) -> Option<Tried> {
    let attempt = logins
        .get(label)?
        .sign_in_attempt
        .as_ref()
        .filter(|a| a.outcome.failing())?;
    Some(Tried {
        outcome: attempt.outcome.as_str(),
        routes: attempt.routes.clone(),
        since: attempt.since,
    })
}

/// Everything the doctor knows of one CLI.
#[derive(Debug, Clone)]
pub struct Facts {
    /// The row.
    pub connector: &'static Connector,
    /// Why atpkg keeps it off this machine, if it does.
    pub kept: Option<Kept>,
    /// The managed copy, when installed.
    pub managed: Option<Managed>,
    /// A copy on `PATH` aterm does not manage.
    pub foreign: Option<PathBuf>,
    /// Its home: `Err` holds the variable's value when it is set but relative.
    pub home: Result<PathBuf, String>,
    /// The value of its home's variable (Claude Code's `CLAUDE_CONFIG_DIR`, Codex's `CODEX_HOME`),
    /// where this environment sets it: its home is then not this machine's default one, so
    /// neither accounts v2's view nor its own pool is read.
    pub home_var: Option<String>,
    /// Whether the home exists: it has run here.
    pub home_exists: bool,
    /// A login file seen, and when the vendor last wrote it.
    pub login_file: Option<(PathBuf, Option<i64>)>,
    /// Whether Codex selects a non-file credential backend (`cli_auth_credentials_store`).
    pub hidden_store: bool,
    /// Claude: its profile names a signed-in account (`oauthAccount`).
    pub profile_signed_in: bool,
    /// Claude's plan, from its profile.
    pub plan: Option<String>,
    /// Accounts v2's view (Claude only).
    pub accounts: Option<ClaudeAccounts>,
    /// The usage limit's end from the vendor's own readings (Codex's rollouts).
    pub limited_until: Option<i64>,
    /// This layer's record.
    pub record: Record,
    /// The provider's credential backend for this record; `None` for unscoped providers.
    pub record_scope: Option<&'static str>,
    /// Why the configured credential backend cannot be identified, without credential data.
    pub credential_store_error: Option<String>,
    /// The row's credential variables set in this environment (names only).
    pub env_credentials: Vec<&'static str>,
    /// Its own accounts pool ([`pool_for`]); Claude Code's is [`Facts::accounts`].
    pub pool: Option<CliAccounts>,
    /// The source a later review wave can recheck, even before a pool is configured.
    pub review_source: Option<ReviewSource>,
}

/// Data needed to recheck a pooled review's account without creating a pool or binding.
#[derive(Debug, Clone)]
pub struct ReviewSource {
    /// The existing account store's location; constructing it writes nothing.
    pub store: crate::harness::accounts::Store,
    /// The configured roster path, including when the file is absent.
    pub roster: PathBuf,
    /// The caller's config file; its pre-parsed mode is only a gather-time snapshot.
    pub config: Option<PathBuf>,
    /// An explicit rotation override; otherwise each wave reads the current config.
    pub mode_override: Option<crate::harness::accounts::Mode>,
    /// The measured Codex launch program, when this provider needs one.
    pub program: Option<PathBuf>,
}

impl Facts {
    /// The file account pool only owns launches and health while its backend is selected.
    pub(crate) fn login_pool(&self) -> Option<&CliAccounts> {
        self.pool.as_ref().filter(|_| {
            !self.hidden_store && self.credential_store_error.is_none() && self.home_var.is_none()
        })
    }

    /// Whether this run uses the default login rather than an explicit home or an
    /// isolated account directory. A keyring-backed pool still uses the default login.
    #[must_use]
    pub(crate) fn uses_default_login(&self) -> bool {
        self.home_var.is_none()
            && self
                .accounts
                .as_ref()
                .is_none_or(|account| account.login.is_none())
            && (self.hidden_store
                || self
                    .pool
                    .as_ref()
                    .and_then(|pool| pool.serving_login.as_ref())
                    .is_none_or(|(_, directory)| directory.is_none()))
    }

    /// Managed accounts own their health in the account store, including a managed
    /// default home. The local CLI record belongs only to an unmanaged default login.
    #[must_use]
    pub(crate) fn default_record(&self) -> Option<&Record> {
        if self.credential_store_error.is_some() {
            return None;
        }
        let managed = self.accounts.as_ref().is_some_and(|a| {
            a.listed || a.refused.is_some() || a.unread.is_some() || a.launch_refused.is_some()
        }) || (!self.hidden_store
            && self.pool.as_ref().is_some_and(|p| {
                p.serving_label.is_some() || p.refused.is_some() || p.launch_refused.is_some()
            }));
        (self.uses_default_login() && !managed).then_some(&self.record)
    }
}

/// What ends a refused launch of a pool's CLI ([`CliAccounts::launch_refused`]), which decides
/// whose act the doctor's row asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchEnd {
    /// The sign-in of the row, labelled so, that the owner's pin holds every launch to and that
    /// is not signed in here: the pool's sign-in signs it in next where it can
    /// ([`CliAccounts::next`]).
    SignIn(String),
    /// A change to the owner's pin or roster, which only a person makes
    /// ([`CliAccounts::launch_step`] names it where it is known): a pin naming no row, a pin on a
    /// row of its own on a Codex program that keeps no row's login its own, or a roster that
    /// lists no row such a program runs.
    Owner,
    /// None of those: the pool's records as they stand (another Mac's lease or warm set, a wall,
    /// no usable row), which a lease's or a wall's end changes by itself, as does a row's
    /// sign-in where the serving row's own state asks for one.
    Pool,
}

/// A CLI's own accounts pool ([`Facts::pool`]): the rows its roster beside `accounts.toml`
/// (`accounts-<cli>.toml`) lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliAccounts {
    /// Its roster (`accounts-<cli>.toml`).
    pub roster: PathBuf,
    /// Why the roster is refused, when it is.
    pub refused: Option<String>,
    /// Why the serving account requires shared authority or an execution grant before local sign-in.
    pub sign_in_refused: Option<String>,
    /// The row a sign-in signs in next: the first with an email whose login is not made here,
    /// else the first due, else the first with an email. A CLI whose renewals cannot approve its
    /// page (Grok Build's, [`crate::ai::connector::Cli::renews_headlessly`]) signs a row in by
    /// `accounts login`, which needs no email, so it then takes the first row with none whose
    /// login is not made here, else the first such row. `None` where none of those is. While the
    /// owner's pin holds every launch to a row not signed in here ([`LaunchEnd::SignIn`]), it
    /// is that row wherever it can be signed in so (an email, or any row for Grok Build). On a
    /// Codex program that keeps no row's login its own ([`Self::unread`]) it is the row on this
    /// machine's own login while that row is signed out or not signed in here and nothing else
    /// keeps it from serving ([`crate::harness::codex_alab::DefaultRow::SignedOut`]), whose
    /// sign-in is then Codex's own in its default home; while that row's login works, or another
    /// Mac's warm set keeps it from this Mac (codex rows carry no claims), it is never that
    /// row, and where that program serves that row alone ([`Self::default_only`]) it is `None`:
    /// no other row runs on that build.
    pub next: Option<String>,
    /// How many rows the roster lists.
    pub rows: usize,
    /// How many of them carry an email.
    pub emails: usize,
    /// How many of them have a login made on this machine (`accounts::enrolled_for`).
    pub enrolled: usize,
    /// How many of them are signed in here: a login made on this machine that the pool has not
    /// recorded signed out.
    pub signed_in: usize,
    /// The rows whose login is due for renewal or has ended (`accounts::renew_due`), by label.
    pub due: Vec<String>,
    /// The pool's on-disk store (`accounts::Store::at_for`), when loaded.
    pub store: Option<crate::harness::accounts::Store>,
    /// An explicit rotation override; otherwise a review rereads the current config.
    pub mode_override: Option<crate::harness::accounts::Mode>,
    /// The Codex launch program whose credential-routing capabilities were measured.
    pub program: Option<PathBuf>,
    /// The serving row's label, when the pool selects one: for Codex, Grok Build and Kimi Code
    /// CLI the row a launch aterm makes takes as the records stand
    /// ([`crate::harness::accounts_pool::launch_now`]) wherever it takes one, and under rotation
    /// off with no pin, where a launch takes no row, the row on this machine's own login, which
    /// a plain launch runs on.
    pub serving_label: Option<String>,
    /// The label of the row on this machine's own login (the one with no `dir`), when the roster
    /// lists one.
    pub default_label: Option<String>,
    /// What that row can do for a Codex run on a program that keeps no row's login its own
    /// ([`crate::harness::accounts_pool::default_serving`]), for Codex's pool.
    pub default_row: Option<crate::harness::codex_alab::DefaultRow>,
    /// Whether the Codex program a launch aterm makes runs keeps no row's login its own
    /// ([`crate::harness::worker::codex_launch_program`] held to
    /// [`crate::harness::codex_alab::auth_path_unread`], or no program is known), read where the
    /// roster lists a row.
    pub unread: bool,
    /// Whether that program serves the row on this machine's own login alone: it is
    /// [`Self::unread`], the roster lists that row and a row of its own, and the owner's pin
    /// holds no row of its own (whose launch is refused instead). The serving row is then that
    /// row wherever the pool would serve one, and no other row is signed in next.
    pub default_only: bool,
    /// Why a launch aterm makes of this pool's CLI from its default home is refused as the
    /// records stand, in the launch's own words
    /// ([`crate::harness::accounts_pool::launch_now`]): a pin on a row not signed in here or
    /// naming none, no usable row, or on a Codex program that keeps no row's login its own a pin
    /// on a row of its own or a default row that cannot serve. `aterm ai review` runs no reviewer
    /// of it then. `None` where a launch takes the serving row, or no row (it runs on the login
    /// its starter carries).
    pub launch_refused: Option<String>,
    /// The steps that end that refusal, each a command in backticks, where they are known: on a
    /// Codex program that keeps no row's login its own, under the owner's pin on a row of its
    /// own, the pin of the row on this machine's own login where it can serve, or where the
    /// roster lists none, that login listed as a row and then pinned
    /// ([`crate::harness::codex_alab::unread_fix`]).
    pub launch_step: Option<String>,
    /// What ends that refusal, where there is one ([`LaunchEnd`]).
    pub launch_end: Option<LaunchEnd>,
    /// The label the owner's pin (`aterm harness accounts use <label>`) holds every launch to,
    /// where a pin stands.
    pub pinned: Option<String>,
    /// What a default-home run sets for the serving row (`accounts::login_env_for`), when the
    /// pool selects one.
    pub serving_login: Option<(&'static str, Option<String>)>,
    /// The serving row's login file on disk and when the vendor last wrote it, when `serving_login`
    /// is `Some`.
    pub serving_login_file: Option<(PathBuf, Option<i64>)>,
    /// `accounts::LoginState::as_str` on the serving row, where it renews, or `"expired"` when the
    /// pool store recorded a non-renewing serving row signed out.
    pub state: Option<&'static str>,
    /// When the pool store recorded the serving row signed out, where it did and said when (one
    /// word for a login wall, a displacement by the vendor's own sign-in and a check that found
    /// no login).
    pub signed_out_at: Option<i64>,
    /// The serving row's forecast end, when due.
    pub renew_by: Option<i64>,
    /// The serving row's usage limit end from the pool store, when limited.
    pub limited_until: Option<i64>,
    /// The newest turn served under the serving row.
    pub served_at: Option<i64>,
    /// When a renewal last bound the serving row, in unix seconds.
    pub renewed_at: Option<i64>,
    /// Another row with an email that is signed out or not enrolled when the serving row is
    /// limited or signed out (never where [`Self::default_only`], since no other row runs
    /// there, nor under rotation off, which takes no row but the owner's pin), else the serving
    /// row itself where it has an email and is signed out.
    pub next_sign_in: Option<String>,
    /// aterm's own sign-in of the row a sign-in signs in ([`Self::sign_in_target`]) that failed
    /// and stands on record.
    pub tried: Option<Tried>,
    /// The step a person owes that row, where its last renewal stopped for one and no signed-in
    /// `accounts browser` window answered it since
    /// ([`crate::harness::accounts_sync::unaddressed_sign_in_task_at`]): its browser profile
    /// signed in once, or its sign-in approved.
    pub person_task: Option<crate::harness::accounts::SignInAction>,
    /// When that row's newest sign-in on record met Cloudflare's bot check, which did not clear
    /// ([`crate::harness::accounts_sync::bot_check_on_record_at`]).
    pub bot_check_at: Option<i64>,
    /// The switch that keeps aterm's own sign-ins off here, where one does
    /// ([`crate::harness::accounts_sync::sign_ins_off_for`]).
    pub off: Option<&'static str>,
}

impl CliAccounts {
    /// The row a sign-in of `cli`'s pool signs in and the `aterm harness accounts` verb it takes
    /// ([`crate::harness::accounts::Cli::sign_in_verb`]): [`Self::next`], except where that row
    /// is this machine's own login's on a Codex build that keeps no row's login its own
    /// ([`crate::ai::login::default_home_sign_in`]), whose sign-in is Codex's own in its default
    /// home, a person's and no pool verb. `None` where no such row is.
    #[must_use]
    pub fn sign_in_target(
        &self,
        cli: crate::harness::accounts::Cli,
    ) -> Option<(&str, &'static str)> {
        if crate::ai::login::default_home_sign_in(self) {
            return None;
        }
        self.next
            .as_deref()
            .map(|label| (label, cli.sign_in_verb(true)))
    }
}

/// What the doctor reads, gathered once for every CLI.
pub struct Machine<'a> {
    /// Unix seconds.
    pub now: i64,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// This process's environment.
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// atpkg's prefix.
    pub pkg: Option<atpkg::store::Layout>,
    /// `[packages] exclude`.
    pub exclude: Vec<String>,
    /// This layer's records.
    pub records: Records,
    /// The harness state root (accounts v2's store lives under it).
    pub harness_state: Option<PathBuf>,
    /// `accounts.toml`.
    pub roster: Option<PathBuf>,
    /// Pre-parsed `[harness] accounts` mode from the caller's config file (`None` reads
    /// `aterm.toml` beside `roster`).
    pub accounts_mode: Option<crate::harness::accounts::Mode>,
    /// The caller's config file, whose `[harness]` switches can keep aterm's own sign-ins off
    /// ([`crate::harness::accounts_sync::sign_ins_off_for`]); `None` reads none.
    pub config: Option<PathBuf>,
}

/// Resolves the home directory for `c` from its variable `c.home.var` (read through `var`) or
/// `$HOME`.
///
/// # Errors
///
/// The variable's value when it is set and relative, or `""` with no `$HOME`.
pub fn home_of(
    c: &Connector,
    var: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(name) = c.home.var
        && let Some(set) = var(name).filter(|v| !v.is_empty())
    {
        let p = PathBuf::from(&set);
        return if p.is_absolute() { Ok(p) } else { Err(set) };
    }
    home.map(|h| h.join(c.home.default)).ok_or_else(String::new)
}

fn mtime(path: &Path) -> Option<i64> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    i64::try_from(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()).ok()
}

/// The managed copy of `id` under `pkg`, with its linked package or installed version.
#[must_use]
pub fn managed_of(pkg: &atpkg::store::Layout, id: &str) -> Option<Managed> {
    let link = atpkg::linkmode::live_linked_checkout(pkg, id);
    let linked = matches!(&link, Ok(Some(_)));
    let paths = if linked {
        [
            pkg.shim(&atpkg::store::ToolName::new(id)?),
            pkg.agents_dir().join(id),
        ]
    } else {
        [pkg.agents_dir().join(id), pkg.bin_dir().join(id)]
    };
    let path = paths.into_iter().find(|p| p.is_file())?;
    let version = if linked {
        crate::harness::codex_alab::linked_package_versions(pkg, id)
            .map(|(product, client)| format!("{product} (client {client}; dev-linked)"))
    } else if link.is_err()
        && !atpkg::which(pkg, id).is_some_and(|target| {
            atpkg::active_builds(pkg)
                .get(id)
                .is_some_and(|build| target.starts_with(pkg.build_dir(id, *build)))
        })
    {
        None
    } else {
        atpkg::status::read(pkg)
            .and_then(|s| s.programs.get(id).map(|p| p.state.clone()))
            .and_then(|state| {
                state
                    .strip_prefix(atpkg::state::MANAGED_PREFIX)
                    .and_then(|rest| rest.split_whitespace().next())
                    .map(str::to_string)
            })
    };
    Some(Managed { path, version })
}

/// A copy of `id` on `PATH` outside atpkg's prefix.
fn foreign_of(
    id: &str,
    var: &dyn Fn(&str) -> Option<String>,
    pkg: Option<&atpkg::store::Layout>,
) -> Option<PathBuf> {
    let path = var("PATH")?;
    on_path(id, path.as_ref(), |dir| {
        pkg.is_none_or(|l| !dir.starts_with(&l.prefix))
    })
}

/// The first file named `word` in a directory of `path` (a `PATH` value) that `keep` admits: the
/// copy a shell that types the bare word starts.
fn on_path(word: &str, path: &OsStr, keep: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|dir| keep(dir))
        .map(|dir| dir.join(word))
        .find(|p| p.is_file())
}

/// Whether a launch that runs `program` would start nothing here, the doctor's `not installed`
/// with no copy found on `PATH` either ([`gather`]'s `managed` and `foreign`): an absolute path
/// that names no file, or a bare word that no directory of `path` (the `PATH` a launch's shell
/// reads, as the doctor reads it) holds. `false` for a bare word where `path` is not known, and
/// for a relative path with a folder, which is the shell's folder's.
#[must_use]
pub fn launch_finds_nothing(program: &Path, path: Option<&OsStr>) -> bool {
    if program.is_absolute() {
        return !program.is_file();
    }
    let mut parts = program.components();
    match (parts.next(), parts.next(), path) {
        (Some(std::path::Component::Normal(word)), None, Some(path)) => word
            .to_str()
            .is_some_and(|word| on_path(word, path, |_| true).is_none()),
        _ => false,
    }
}

/// Accounts v2's view of the serving Claude row: the active row when it is ready here, else the
/// own row when it is, else the first ready row, else the own row. A refused roster yields only
/// its refusal ([`ClaudeAccounts::refused`]), and unreadable state or login bindings only why
/// ([`ClaudeAccounts::unread`]). `None` with no harness state root.
fn local_sign_in_refusal(
    row: &crate::harness::accounts::Row,
    state: &crate::harness::accounts::State,
    logins: &crate::harness::accounts::Logins,
) -> Option<String> {
    if let Some(holder) = &row.held_by {
        return Some(format!(
            "account {} is held for another Mac's warm set ({holder})",
            row.label
        ));
    }
    if let Err(why) = state.execution_verdict(&row.label) {
        return Some(why.as_str().to_owned());
    }
    match logins.shared.required(row) {
        Ok(None) => None,
        Ok(Some(_)) => Some("this account requires a fresh root-authorized shared sign-in".into()),
        Err(why) => Some(why),
    }
}

fn claude_accounts(m: &Machine<'_>) -> Option<ClaudeAccounts> {
    use crate::harness::accounts;
    let store = accounts::Store::at(m.harness_state.as_ref()?);
    let loaded = m.roster.as_ref().map(|p| accounts::Roster::load(p));
    let cache = store
        .load(m.now)
        .and_then(|(state, _)| store.load_logins(m.now).map(|(logins, _)| (state, logins)));
    let refused = match &loaded {
        Some(Err(why)) => Some(crate::harness::one_line(why, 160)),
        _ => None,
    };
    let unread = cache
        .as_ref()
        .err()
        .filter(|_| refused.is_none())
        .map(|why| {
            let file = store.state_file();
            format!(
                "{}, in {}",
                crate::harness::one_line(why, 160),
                file.parent().unwrap_or(&file).display()
            )
        });
    if refused.is_some() || unread.is_some() {
        return Some(ClaudeAccounts {
            store: None,
            roster: m.roster.clone(),
            label: crate::ai::login::OWN_LABEL.to_string(),
            listed: false,
            refused,
            unread,
            launch_refused: None,
            sign_in_refused: None,
            renews: false,
            login: None,
            made: false,
            state: None,
            renew_by: None,
            limited_until: None,
            served_at: None,
            renewed_at: None,
            signed_out_at: None,
            next_sign_in: None,
            tried: None,
            unattended: false,
            off: None,
        });
    }
    let listed_roster = match loaded {
        Some(Ok(r)) if !r.rows.is_empty() => Some(r),
        _ => None,
    };
    let (state, logins) = cache.ok()?;
    let Some(roster) = listed_roster else {
        return Some(ClaudeAccounts {
            store: Some(store),
            roster: m.roster.clone(),
            label: crate::ai::login::OWN_LABEL.to_owned(),
            listed: false,
            refused: None,
            unread: None,
            launch_refused: None,
            sign_in_refused: None,
            renews: false,
            login: None,
            made: false,
            state: None,
            renew_by: None,
            limited_until: None,
            served_at: None,
            renewed_at: None,
            signed_out_at: None,
            next_sign_in: None,
            tried: None,
            unattended: false,
            off: m.config.as_deref().and_then(|config| {
                crate::harness::accounts_sync::sign_ins_off_for(
                    config,
                    crate::harness::accounts::Cli::Claude,
                )
            }),
        });
    };
    let bound = accounts::bound(&roster, &logins);
    let enrolled = accounts::enrolled(&bound);
    let (row, _) = select_claude_serving_row(&bound, &state, &enrolled, m.now)?;
    let row = row.clone();
    let made = row
        .dir
        .as_deref()
        .is_some_and(|d| accounts::login_made(Path::new(d)));
    let renews = row.email.is_some();
    let binding = logins.get(&row.label);
    let row_enrolled = row.dir.is_none() || enrolled.contains(&row.label);
    let cond = accounts::condition(&row, &state, row_enrolled, m.now);
    let limited_until = match cond {
        accounts::Condition::Limited { until } => Some(until),
        _ => None,
    };
    let signed_out = matches!(
        cond,
        accounts::Condition::SignedOut | accounts::Condition::NotEnrolled
    );
    let local = |row: &accounts::Row| local_sign_in_refusal(row, &state, &logins).is_none();
    let local_roster = accounts::Roster {
        rows: bound
            .rows
            .iter()
            .filter(|row| local(row))
            .cloned()
            .collect(),
    };
    let next_sign_in = if (limited_until.is_some() || signed_out) && (!state.pinned || local(&row))
    {
        accounts::renew_next(&local_roster, &state, &enrolled, Some(&row.label), m.now).or_else(
            || (signed_out && local(&row) && row.email.is_some()).then(|| row.label.clone()),
        )
    } else {
        None
    };
    let sign_in_refused = next_sign_in
        .is_none()
        .then(|| local_sign_in_refusal(&row, &state, &logins))
        .flatten();
    let target = next_sign_in.as_deref().unwrap_or(&row.label);
    let target_email = bound.row(target).and_then(|r| r.email.as_deref());
    let renews_target = target_email.is_some() && bound.row(target).is_some_and(local);
    let (login_state, renew_by) = if renews {
        let login = accounts::login_state(&row, &logins, &state, m.now);
        let by = match login {
            accounts::LoginState::Due { by } => Some(by),
            _ => None,
        };
        (Some(login.as_str()), by)
    } else if cond == accounts::Condition::SignedOut {
        (Some("expired"), None)
    } else {
        (None, None)
    };
    Some(ClaudeAccounts {
        label: row.label.clone(),
        listed: true,
        refused: None,
        unread: None,
        launch_refused: state
            .execution_verdict(&row.label)
            .err()
            .map(|why| why.as_str().to_owned()),
        sign_in_refused,
        renews,
        login: row.dir.clone(),
        made,
        state: login_state,
        renew_by,
        limited_until,
        served_at: binding.and_then(|b| b.served_at),
        renewed_at: binding.and_then(|b| b.renewed_at),
        signed_out_at: state
            .accounts
            .get(&row.label)
            .filter(|a| a.auth == accounts::Auth::SignedOut)
            .and_then(|a| a.auth_at),
        tried: tried_of(&logins, target),
        unattended: renews_target
            && crate::harness::cli::claude_row_can_run_headless(
                &|name| (m.var)(name).map(std::ffi::OsString::from),
                true,
                m.home.as_deref(),
                &store,
                &logins,
                target,
                target_email,
            ),
        off: m.config.as_deref().and_then(|config| {
            crate::harness::accounts_sync::sign_ins_off_for(
                config,
                crate::harness::accounts::Cli::Claude,
            )
        }),
        next_sign_in,
        store: Some(store),
        roster: m.roster.clone(),
    })
}

/// Select Claude's current row and say whether it can serve a new review now.
/// The fallback row remains available to the doctor even when no row is ready.
pub(crate) fn select_claude_serving_row<'a>(
    bound: &'a crate::harness::accounts::Roster,
    state: &crate::harness::accounts::State,
    enrolled: &std::collections::BTreeSet<String>,
    now: i64,
) -> Option<(&'a crate::harness::accounts::Row, bool)> {
    use crate::harness::accounts;
    if state.pinned
        && let Some(row) = state.active.as_deref().and_then(|label| bound.row(label))
        && state.execution_verdict(&row.label).is_err()
    {
        return Some((row, false));
    }
    let own = bound
        .row_for_login(None)
        .filter(|r| r.held_by.is_none())
        .or_else(|| {
            bound
                .rows
                .iter()
                .find(|r| r.held_by.is_none() && r.was.iter().any(Option::is_none))
        })
        .or_else(|| bound.rows.iter().find(|r| r.held_by.is_none()))
        .or_else(|| bound.row_for_login(None))
        .or_else(|| {
            bound
                .rows
                .iter()
                .find(|r| r.was.iter().any(Option::is_none))
        })
        .or_else(|| bound.rows.first())?;
    let ready = |row: &accounts::Row| {
        row.held_by.is_none()
            && state.execution_verdict(&row.label).is_ok()
            && (row.dir.is_none() || enrolled.contains(&row.label))
            && matches!(
                accounts::condition(row, state, true, now),
                accounts::Condition::Usable
                    | accounts::Condition::Low
                    | accounts::Condition::Unseen
            )
    };
    let row = state
        .active
        .as_deref()
        .and_then(|label| bound.row(label))
        .filter(|row| ready(row))
        .or_else(|| {
            if ready(own) {
                Some(own)
            } else {
                bound.rows.iter().find(|row| ready(row))
            }
        })
        .unwrap_or(own);
    Some((row, ready(row)))
}

/// Claude's profile: whether it names a signed-in account, and the plan.
fn claude_profile(m: &Machine<'_>, home: &Path) -> (bool, Option<String>) {
    use crate::harness::profile;
    let Some(user_home) = m.home.as_deref() else {
        return (false, None);
    };
    let config = (m.var)("CLAUDE_CONFIG_DIR");
    let path = profile::path_in(user_home, config.as_deref(), home);
    match profile::read(&path) {
        Ok(p) => (p.key.is_some(), p.plan.title()),
        Err(_) => (false, None),
    }
}

/// Whether Codex keeps its login anywhere but `auth.json` in its home, where aterm does not look
/// (`cli_auth_credentials_store` = `keyring`, `auto` or `ephemeral` in its `config.toml`): its
/// account pool then has no row's own login file to start a session under.
#[must_use]
pub fn codex_keyring(home: &Path) -> bool {
    codex_auth_store(home).is_ok_and(CodexAuthStore::hidden)
}

/// Whether Codex's readable configuration selects file credentials, including its default.
/// Unknown or unreadable configuration cannot authorize a file-account launch or renewal.
#[must_use]
pub fn codex_file_store(home: &Path) -> bool {
    codex_auth_store(home).is_ok_and(|storage| storage == CodexAuthStore::File)
}

/// The credential backend named by Codex's current configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexAuthStore {
    File,
    Keyring,
    Auto,
    Ephemeral,
}

impl CodexAuthStore {
    pub(crate) fn hidden(self) -> bool {
        self != Self::File
    }

    pub(crate) const fn scope(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Keyring => "keyring",
            Self::Auto => "auto",
            Self::Ephemeral => "ephemeral",
        }
    }
}

/// Read the backend without opening any credential or accessing the OS keyring.
/// # Errors
/// The configuration cannot be read or parsed, or names an unknown backend.
pub(crate) fn codex_auth_store(home: &Path) -> Result<CodexAuthStore, String> {
    let text = crate::harness::accounts::read_bounded(&home.join("config.toml"), 256 * 1024)
        .map_err(|why| format!("cannot read Codex credential storage configuration: {why}"))?;
    let Some(text) = text else {
        return Ok(CodexAuthStore::File);
    };
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&text)
        .map_err(|_| "cannot parse Codex credential storage configuration".to_owned())?;
    match doc.get("cli_auth_credentials_store") {
        None => Ok(CodexAuthStore::File),
        Some(value) => match value.as_str() {
            Some("file") => Ok(CodexAuthStore::File),
            Some("keyring") => Ok(CodexAuthStore::Keyring),
            Some("auto") => Ok(CodexAuthStore::Auto),
            Some("ephemeral") => Ok(CodexAuthStore::Ephemeral),
            _ => Err("unknown Codex credential storage backend".to_owned()),
        },
    }
}

/// A CLI's stored login file under `dir` (`stat`ed, never opened), with its write time.
fn store_file_in(dir: &Path, c: &Connector) -> Option<(PathBuf, Option<i64>)> {
    super::connector::stored_login_file(c, dir).map(|p| {
        let w = mtime(&p);
        (p, w)
    })
}

/// The end of Codex's usage limit from its own `rate_limits` readings
/// ([`super::codex_limits::home_pick`]): the latest reset ahead of a window at 100%, read only
/// from a reading no older than [`super::codex_limits::FRESH_S`]. A reading older than `since` is
/// ignored.
fn codex_limit(home: &Path, since: Option<i64>, now: i64) -> Option<i64> {
    use super::codex_limits as limits;
    let pick = limits::home_pick(home, now)?;
    if now.saturating_sub(pick.newest) > limits::FRESH_S || since.is_some_and(|at| pick.newest < at)
    {
        return None;
    }
    pick.windows
        .iter()
        .filter(|w| w.used_percent >= 100.0)
        .filter_map(|w| w.resets_at)
        .filter(|at| *at > now)
        .max()
}

/// Gathers [`Facts`] for one CLI.
#[must_use]
pub fn gather(m: &Machine<'_>, c: &'static Connector) -> Facts {
    let home = home_of(c, m.var, m.home.as_deref());
    let home_exists = home.as_ref().is_ok_and(|h| h.exists());
    let managed = m.pkg.as_ref().and_then(|l| managed_of(l, c.id));
    let foreign = foreign_of(c.id, m.var, m.pkg.as_ref());
    let kept = if !atpkg::vendor_direct::served_here(c.id) {
        Some(Kept::Unavailable)
    } else if m.exclude.iter().any(|e| e == c.id) {
        Some(Kept::Excluded)
    } else if managed.is_none() {
        m.pkg
            .as_ref()
            .and_then(|layout| match layout.removed_programs() {
                Ok(removed) => removed.contains(c.id).then_some(Kept::Removed),
                Err(_) => Some(Kept::RemovalUnreadable),
            })
    } else {
        None
    };
    let home_login_file = home.as_ref().ok().and_then(|h| store_file_in(h, c));
    let home_var = c.home.var.and_then(m.var).filter(|s| !s.is_empty());
    let default_home = home_var.is_none();
    let (profile_signed_in, plan, accounts) = match (c.is_claude(), home.as_ref()) {
        (true, Ok(h)) => {
            let (signed_in, plan) = claude_profile(m, h);
            let view = if default_home {
                claude_accounts(m)
            } else {
                None
            };
            (signed_in, plan, view)
        }
        (true, Err(_)) => (false, None, None),
        (false, _) => (false, None, None),
    };
    let is_codex = c.id == super::connector::CODEX.id;
    let (storage, credential_store_error) = match (is_codex, home.as_ref()) {
        (true, Ok(home)) => match codex_auth_store(home) {
            Ok(storage) => (Some(storage), None),
            Err(why) => (None, Some(why)),
        },
        _ => (None, None),
    };
    let hidden_store = storage.is_some_and(CodexAuthStore::hidden);
    let record_scope = storage.map(CodexAuthStore::scope);
    // The Codex a launch aterm makes runs, the one program every judge of a row's own login
    // reads (`worker::codex_launch_program`), here from this machine's atpkg layout.
    let launch_program = is_codex.then(|| {
        let agents = m.pkg.as_ref().map(atpkg::store::Layout::agents_dir);
        crate::harness::worker::codex_launch_program_in(agents.as_deref())
    });
    let pool = if default_home {
        crate::harness::accounts::Cli::parse(c.id).and_then(|cli| {
            pool_for_with(
                m,
                cli,
                is_codex.then_some(hidden_store || credential_store_error.is_some()),
                launch_program.as_deref(),
            )
        })
    } else {
        None
    };
    let review_source = default_home
        .then(|| {
            let cli = crate::harness::accounts::Cli::parse(c.id)?;
            if cli == crate::harness::accounts::Cli::Claude {
                return None;
            }
            cli.accounts().signin.as_ref()?;
            Some(ReviewSource {
                store: crate::harness::accounts::Store::at_for(
                    m.harness_state.as_deref()?,
                    cli,
                    m.home.as_deref()?,
                ),
                roster: m
                    .roster
                    .as_ref()?
                    .parent()?
                    .join(cli.accounts().roster_file),
                config: m.config.clone(),
                mode_override: if m.config.is_some() {
                    None
                } else {
                    m.accounts_mode
                },
                program: launch_program.clone(),
            })
        })
        .flatten();
    let login_file = match pool.as_ref().filter(|p| p.serving_login.is_some()) {
        _ if hidden_store || credential_store_error.is_some() => None,
        Some(p) => p.serving_login_file.clone(),
        None => home_login_file,
    };
    let limited_until = match pool.as_ref().filter(|p| p.serving_login.is_some()) {
        _ if hidden_store || credential_store_error.is_some() => None,
        Some(p) => p.limited_until.or_else(|| {
            if is_codex
                && p.enrolled <= 1
                && p.serving_login
                    .as_ref()
                    .is_some_and(|(_, directory)| directory.is_none())
            {
                home.as_ref()
                    .ok()
                    .and_then(|h| codex_limit(h, p.renewed_at, m.now))
            } else {
                None
            }
        }),
        None => match (is_codex, home.as_ref()) {
            (true, Ok(h)) => codex_limit(h, None, m.now),
            _ => accounts.as_ref().and_then(|a| a.limited_until),
        },
    };
    Facts {
        connector: c,
        kept,
        managed,
        foreign,
        home,
        home_var,
        home_exists,
        login_file,
        hidden_store,
        profile_signed_in,
        plan,
        accounts,
        limited_until,
        record: if is_codex && storage.is_none() {
            Record::default()
        } else {
            m.records.of_scope(c.id, record_scope)
        },
        record_scope,
        credential_store_error,
        env_credentials: c
            .credential_env
            .iter()
            .copied()
            .filter(|k| (m.var)(k).is_some_and(|v| !v.is_empty()))
            .collect(),
        pool,
        review_source,
    }
}

/// `cli`'s own accounts pool, where its roster file sits beside `accounts.toml` and the harness
/// signs it in (`accounts renew --cli <id>`). `None` where there is no such roster or sign-in,
/// and for Claude Code, whose pool is [`ClaudeAccounts`].
#[must_use]
pub fn pool_for(m: &Machine<'_>, cli: crate::harness::accounts::Cli) -> Option<CliAccounts> {
    pool_for_with(m, cli, None, None)
}

/// [`pool_for`] for the Codex `program` a launch aterm makes runs
/// ([`crate::harness::worker::codex_launch_program`]; `None` where none is known): a Codex
/// program that keeps no row's login its own serves the row on this machine's own login in place
/// of a row of its own the pool would serve, the owner's pin aside. The serving row is the one a
/// launch takes ([`crate::harness::accounts_pool::launch_now`]) wherever it takes one.
fn pool_for_with(
    m: &Machine<'_>,
    cli: crate::harness::accounts::Cli,
    codex_hidden_store: Option<bool>,
    program: Option<&Path>,
) -> Option<CliAccounts> {
    use crate::harness::accounts::{self, Class, Cli, Condition, LoginState, Mode};
    if cli == Cli::Claude {
        return None;
    }
    cli.accounts().signin.as_ref()?;
    let config_dir = m.roster.as_ref()?.parent()?;
    let roster_path = config_dir.join(cli.accounts().roster_file);
    let roster = match accounts::roster_text(&roster_path).and_then(|text| {
        text.map(|text| accounts::Roster::parse_for(cli, &text))
            .transpose()
    }) {
        Ok(None) => return None,
        Ok(Some(roster)) => roster,
        Err(why) => {
            return Some(CliAccounts {
                roster: roster_path,
                refused: Some(why),
                ..CliAccounts::default()
            });
        }
    };
    let cache = match (m.harness_state.as_deref(), m.home.as_deref()) {
        (Some(state), Some(home)) => {
            let store = accounts::Store::at_for(state, cli, home);
            store.load(m.now).and_then(|(state, _)| {
                store
                    .load_logins(m.now)
                    .map(|(logins, _)| (home, store, state, logins))
            })
        }
        _ => Err("account store is unavailable".to_owned()),
    };
    let (user_home, store, state, logins) = match cache {
        Ok(loaded) => loaded,
        Err(why) => {
            return Some(CliAccounts {
                roster: roster_path,
                refused: Some(why),
                ..CliAccounts::default()
            });
        }
    };
    let c = cli.connector();
    let default_dir = user_home.join(cli.accounts().default_home);
    let default_login = store_file_in(&default_dir, c);
    let bound = accounts::bound(&roster, &logins);
    let enrolled_here = accounts::enrolled_in(cli, &bound, user_home);
    let emails = bound.rows.iter().filter(|r| r.email.is_some()).count();
    let local = |row: &accounts::Row| local_sign_in_refusal(row, &state, &logins).is_none();
    let executable =
        |row: &accounts::Row| row.held_by.is_none() && state.execution_verdict(&row.label).is_ok();
    let with_email = || bound.rows.iter().filter(|r| local(r) && r.email.is_some());
    let due: Vec<String> = with_email()
        .filter(|row| enrolled_here.contains(&row.label))
        .filter(|row| {
            matches!(
                accounts::login_state(row, &logins, &state, m.now),
                LoginState::Expired | LoginState::Due { .. }
            )
        })
        .map(|row| row.label.clone())
        .collect();
    let next = with_email()
        .find(|r| !enrolled_here.contains(&r.label))
        .map(|r| r.label.clone())
        .or_else(|| due.first().cloned())
        .or_else(|| with_email().next().map(|r| r.label.clone()))
        .or_else(|| {
            let no_email = || bound.rows.iter().filter(|r| local(r) && r.email.is_none());
            (!cli.renews_headlessly())
                .then(|| {
                    no_email()
                        .find(|r| !enrolled_here.contains(&r.label))
                        .or_else(|| no_email().next())
                })
                .flatten()
                .map(|r| r.label.clone())
        });
    let enrolled_count = bound
        .rows
        .iter()
        .filter(|r| enrolled_here.contains(&r.label))
        .count();
    let default_home = c
        .home
        .var
        .is_none_or(|v| (m.var)(v).is_none_or(|s| s.is_empty()));
    let hidden_store =
        cli == Cli::Codex && codex_hidden_store.unwrap_or_else(|| !codex_file_store(&default_dir));
    let mode = m.accounts_mode.unwrap_or_else(|| {
        crate::supervise::config::SupervisorConfig::from_path(Some(&config_dir.join("aterm.toml")))
            .0
            .accounts
    });
    let row_cond =
        |r: &accounts::Row| accounts::condition(r, &state, enrolled_here.contains(&r.label), m.now);
    let row_present =
        |r: &accounts::Row| !matches!(row_cond(r), Condition::NotEnrolled | Condition::SignedOut);
    let row_ready = |r: &accounts::Row| {
        matches!(
            row_cond(r),
            Condition::Usable | Condition::Low | Condition::Unseen
        )
    };
    let serving =
        if !bound.rows.is_empty() && cli.accounts().measured && default_home && !hidden_store {
            let active = state.active.as_deref().and_then(|label| bound.row(label));
            if state.pinned && active.is_some_and(|row| !executable(row)) {
                active
            } else if mode == Mode::Off {
                // Rotation off takes the owner's pin, else no row: a plain launch runs on this
                // machine's own login, whose row's records are then the ones that stand.
                if state.pinned {
                    active.filter(|r| row_present(r))
                } else {
                    bound.row_for_login(None)
                }
            } else {
                let own = bound
                    .row_for_login(None)
                    .filter(|r| executable(r))
                    .or_else(|| {
                        bound
                            .rows
                            .iter()
                            .find(|r| executable(r) && r.was.iter().any(Option::is_none))
                    })
                    .or_else(|| bound.rows.iter().find(|r| executable(r)))
                    .or_else(|| bound.row_for_login(None))
                    .or_else(|| {
                        bound
                            .rows
                            .iter()
                            .find(|r| r.was.iter().any(Option::is_none))
                    })
                    .or_else(|| bound.rows.first());
                let first_in_class = |pred: &dyn Fn(&accounts::Row) -> bool| {
                    [Class::Dedicated, Class::Shared]
                        .into_iter()
                        .find_map(|cls| {
                            bound
                                .rows
                                .iter()
                                .filter(|r| executable(r) && r.class == cls)
                                .find(|r| pred(r))
                        })
                };
                active
                    .filter(|r| {
                        executable(r) && ((state.pinned && row_present(r)) || row_cond(r).fresh())
                    })
                    .or_else(|| own.filter(|r| executable(r) && row_cond(r).fresh()))
                    .or_else(|| first_in_class(&|r| row_cond(r).fresh()))
                    .or_else(|| active.filter(|r| executable(r) && row_ready(r)))
                    .or_else(|| own.filter(|r| executable(r) && row_ready(r)))
                    .or_else(|| first_in_class(&row_ready))
                    .or_else(|| bound.rows.iter().find(|r| executable(r) && row_present(r)))
                    .or(own)
            }
        } else {
            None
        };
    let default_row = bound.row_for_login(None);
    // The program a Codex launch runs keeps no row's login its own: the one known ignores
    // `CODEX_AUTH_PATH` or names no build (the bare word `codex`), or none is known, so nothing
    // here shows that a launch reads it. Read only where the roster lists a row to judge, since
    // it scans the build's bytes.
    let unread = cli == Cli::Codex
        && !bound.rows.is_empty()
        && program.is_none_or(|p| crate::harness::codex_alab::auth_path_unread(p).is_some());
    let pinned_own = state.pinned
        && state
            .active
            .as_deref()
            .and_then(|label| bound.row(label))
            .is_some_and(|r| r.dir.is_some());
    let default_only = unread
        && default_row.is_some()
        && bound.rows.iter().any(|r| r.dir.is_some())
        && !pinned_own;
    let serving = if default_only {
        serving.and(default_row)
    } else {
        serving
    };
    // What a launch aterm makes takes as the records stand, which `aterm ai review` holds its
    // reviewer to: the serving row is that launch's row wherever it takes one, and where it is
    // refused no reviewer runs ([`CliAccounts::launch_refused`]). A launch from a home of its own
    // takes no row. With no Codex program known, the bare word stands in: it names no build.
    let launch = if default_home {
        crate::harness::accounts_pool::launch_now(
            (cli, hidden_store, mode),
            &bound,
            &state,
            &enrolled_here,
            program.or(Some(Path::new("codex"))),
            m.now,
        )
    } else {
        Ok(None)
    };
    let serving = match &launch {
        Ok(Some(row)) => Some(*row),
        Ok(None) | Err(_) => serving,
    };
    let default_serving = (cli == Cli::Codex && !bound.rows.is_empty()).then(|| {
        match crate::harness::accounts_pool::default_serving(&bound, &state, &enrolled_here, m.now)
        {
            Ok(row) => crate::harness::codex_alab::DefaultRow::Serves(row.label.clone()),
            Err(blocked) => blocked,
        }
    });
    // On such a program the default row signs in by Codex's own sign-in in its default home while
    // it is signed out here, whatever else the pool would sign in next: no other row runs there.
    // One held for another Mac's warm set or leased elsewhere is no sign-in target here.
    let default_sign_in = match &default_serving {
        Some(crate::harness::codex_alab::DefaultRow::SignedOut { label, .. })
            if unread && bound.row(label).is_some_and(local) =>
        {
            Some(label.clone())
        }
        _ => None,
    };
    // What ends a refused launch where the steps are known: on such a program, under the owner's
    // pin on a row of its own, the pin of the row on this machine's own login where it can serve,
    // or where none is listed, that login listed as a row and then pinned (a listing alone leaves
    // the pin where it is).
    let launch_step = match (&launch, &default_serving) {
        (Err(_), Some(crate::harness::codex_alab::DefaultRow::Serves(label)))
            if unread && pinned_own =>
        {
            Some(format!(
                "`{}`",
                crate::harness::codex_alab::unread_fix(Some(label))
            ))
        }
        (Err(_), Some(crate::harness::codex_alab::DefaultRow::Unlisted))
            if unread && pinned_own =>
        {
            Some(format!(
                "`{}`, then `{}`",
                crate::harness::codex_alab::unread_fix(None),
                crate::harness::codex_alab::unread_fix(Some("<label>"))
            ))
        }
        _ => None,
    };
    // Whose act ends a refused launch: the owner's pin on a row not signed in here is ended by
    // that row's sign-in (on such a program, only where it is the row on this machine's own login,
    // since a row of its own is refused there signed in too); a pin naming no row, a pin on a row
    // of its own on such a program, or a roster listing no row it runs, by the owner's change;
    // anything else by the pool's records as they change (a lease, a warm set, a wall).
    let launch_end = launch.as_ref().err().map(|_| {
        use crate::harness::codex_alab::DefaultRow;
        let pinned = state
            .pinned
            .then(|| state.active.as_deref().and_then(|label| bound.row(label)));
        match pinned {
            _ if launch_step.is_some() => LaunchEnd::Owner,
            Some(None) => LaunchEnd::Owner,
            Some(Some(row)) if unread && row.dir.is_some() => LaunchEnd::Owner,
            Some(Some(row)) if local(row) && !row_present(row) => {
                LaunchEnd::SignIn(row.label.clone())
            }
            _ if unread
                && matches!(
                    default_serving,
                    Some(DefaultRow::Unlisted | DefaultRow::Moved(_))
                ) =>
            {
                LaunchEnd::Owner
            }
            _ => LaunchEnd::Pool,
        }
    });
    let mut out = CliAccounts {
        roster: roster_path,
        refused: None,
        next: next.filter(|_| {
            !state.pinned
                || state
                    .active
                    .as_deref()
                    .and_then(|label| bound.row(label))
                    .is_none_or(local)
        }),
        rows: bound.rows.len(),
        emails,
        enrolled: enrolled_count,
        signed_in: bound.rows.iter().filter(|r| row_present(r)).count(),
        due,
        store: Some(store),
        mode_override: m.accounts_mode,
        program: program.map(Path::to_path_buf),
        default_label: default_row.map(|r| r.label.clone()),
        default_row: default_serving,
        unread,
        default_only,
        launch_refused: launch.as_ref().err().cloned(),
        launch_step,
        launch_end,
        pinned: state.active.clone().filter(|_| state.pinned),
        ..CliAccounts::default()
    };
    if let Some(row) = serving {
        out.serving_label = Some(row.label.clone());
        out.serving_login = Some(accounts::login_env_for(cli, row));
        out.serving_login_file = match row.dir.as_deref().filter(|d| !d.is_empty()) {
            Some(dir) => store_file_in(Path::new(dir), c),
            None => default_login,
        };
        let cond = row_cond(row);
        out.limited_until = match cond {
            Condition::Limited { until } => Some(until),
            _ => None,
        };
        out.signed_out_at = state
            .accounts
            .get(&row.label)
            .filter(|account| account.auth == accounts::Auth::SignedOut)
            .and_then(|account| account.auth_at);
        let signed_out = matches!(cond, Condition::SignedOut | Condition::NotEnrolled);
        if out.limited_until.is_some() || signed_out {
            // Rotation off takes no row but the pin, so no other row's sign-in serves a launch.
            out.next_sign_in = [Class::Dedicated, Class::Shared]
                .into_iter()
                .filter(|_| !default_only && mode != Mode::Off)
                .find_map(|cls| {
                    bound
                        .rows
                        .iter()
                        .filter(|r| {
                            local(r)
                                && r.class == cls
                                && r.label != row.label
                                && r.email.is_some()
                                && matches!(
                                    row_cond(r),
                                    Condition::NotEnrolled | Condition::SignedOut
                                )
                                && !state
                                    .accounts
                                    .get(&r.label)
                                    .is_some_and(|a| a.limited_until(m.now).is_some())
                        })
                        .map(|r| r.label.clone())
                        .next()
                })
                .or_else(|| {
                    (signed_out && local(row) && row.email.is_some()).then(|| row.label.clone())
                });
            if state.pinned && !local(row) {
                out.next_sign_in = None;
            }
            if let Some(next) = &out.next_sign_in {
                out.next = Some(next.clone());
            }
        }
        let binding = logins.get(&row.label);
        out.served_at = binding.and_then(|b| b.served_at);
        out.renewed_at = binding.and_then(|b| b.renewed_at);
        if row.email.is_some() {
            let ls = accounts::login_state(row, &logins, &state, m.now);
            if let LoginState::Due { by } = ls {
                out.renew_by = Some(by);
            }
            out.state = Some(ls.as_str());
        } else if cond == Condition::SignedOut {
            out.state = Some("expired");
        }
    }
    // While the owner's pin holds every launch to a row not signed in here, that row's sign-in is
    // the one that ends the refusal, wherever the pool can sign it in: no other row's serves a
    // launch.
    if let Some(LaunchEnd::SignIn(label)) = &out.launch_end
        && bound
            .row(label)
            .is_some_and(|row| row.email.is_some() || !cli.renews_headlessly())
    {
        out.next = Some(label.clone());
    }
    if default_sign_in.is_some() {
        out.next = default_sign_in;
    } else if default_only || (unread && out.next.is_some() && out.next == out.default_label) {
        // Such a build signs in no other row where it serves the default row alone, and that
        // row's login still works, which only Codex's own sign-in, revoking it first, renews
        // there: `aterm ai login` signs in nothing.
        out.next = None;
    }
    if out.next.is_none() {
        out.sign_in_refused = serving.and_then(|row| local_sign_in_refusal(row, &state, &logins));
    }
    // What stands on record of the row a sign-in signs in, and the switches that keep aterm's
    // own sign-ins off here.
    if let Some((label, _)) = out.sign_in_target(cli) {
        let label = label.to_owned();
        out.tried = tried_of(&logins, &label);
        if let Some(store) = out.store.as_ref() {
            out.person_task = logins
                .get(&label)
                .and_then(|b| b.sign_in_task)
                .filter(|_| {
                    crate::harness::accounts_sync::unaddressed_sign_in_task_at(
                        store, &logins, &label,
                    )
                    .is_some()
                })
                .map(|task| task.action);
            out.bot_check_at = crate::harness::accounts_sync::bot_check_on_record_at(store, &label);
        }
    }
    out.off = m
        .config
        .as_deref()
        .and_then(|config| crate::harness::accounts_sync::sign_ins_off_for(config, cli));
    Some(out)
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
