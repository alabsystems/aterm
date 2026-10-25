// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Machine and CLI state inspection for the AI doctor.

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
                std::fs::File::open(path).map_err(|e| RunFailed::NotRun(e.to_string()))?,
            ),
            None => aterm_uds::spawnfd::Stdio::null(),
        };
        match aterm_uds::deadline::output_until_from(&mut cmd, feed, Instant::now() + job.budget) {
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
    /// The atpkg shim that runs the verified store copy.
    pub path: PathBuf,
    /// Its version, from atpkg's record.
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
}

/// Accounts v2's view of the Claude row that serves this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAccounts {
    /// The serving row's label (the own row's where the roster is refused).
    pub label: String,
    /// Whether `accounts.toml` lists the row.
    pub listed: bool,
    /// Why `accounts.toml` is refused, when it is.
    pub refused: Option<String>,
    /// Whether the row has an email and renews by itself.
    pub renews: bool,
    /// The login directory a renewal bound the row to, or `None` for the default login.
    pub login: Option<String>,
    /// Whether a login was made in that bound directory (`accounts::login_made`).
    pub made: bool,
    /// `accounts::LoginState::as_str`, where the row renews.
    pub state: Option<&'static str>,
    /// The forecast end, when due.
    pub renew_by: Option<i64>,
    /// The usage limit's end, when limited.
    pub limited_until: Option<i64>,
    /// The newest turn served under it.
    pub served_at: Option<i64>,
    /// When a renewal last bound it, in unix seconds.
    pub renewed_at: Option<i64>,
    /// The next not-enrolled or signed-out row to sign into when the serving row is limited,
    /// when that row has an email.
    pub next_sign_in: Option<String>,
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
    /// Whether the home exists: it has run here.
    pub home_exists: bool,
    /// A login file seen, and when the vendor last wrote it.
    pub login_file: Option<(PathBuf, Option<i64>)>,
    /// Whether Codex keeps its login in the OS keyring (`cli_auth_credentials_store`).
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
    /// The row's credential variables set in this environment (names only).
    pub env_credentials: Vec<&'static str>,
    /// Its own accounts pool ([`pool_for`]); Claude Code's is [`Facts::accounts`].
    pub pool: Option<CliAccounts>,
}

/// A CLI's own accounts pool ([`Facts::pool`]): the rows its roster beside `accounts.toml`
/// (`accounts-<cli>.toml`) lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CliAccounts {
    /// Its roster (`accounts-<cli>.toml`).
    pub roster: PathBuf,
    /// Why the roster is refused, when it is.
    pub refused: Option<String>,
    /// The row a sign-in renews: the first with an email whose login is not made here, else
    /// the first due, else the first with an email; `None` where no row has an email.
    pub next: Option<String>,
    /// How many rows the roster lists.
    pub rows: usize,
    /// How many of them have a login made on this machine (`accounts::enrolled_for`).
    pub enrolled: usize,
    /// The rows whose login is due for renewal or has ended (`accounts::renew_due`), by label.
    pub due: Vec<String>,
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

/// The managed copy of `id` under `pkg`, with atpkg's recorded version.
#[must_use]
pub fn managed_of(pkg: &atpkg::store::Layout, id: &str) -> Option<Managed> {
    let path = [pkg.agents_dir().join(id), pkg.bin_dir().join(id)]
        .into_iter()
        .find(|p| p.is_file())?;
    let version = atpkg::status::read(pkg)
        .and_then(|s| s.programs.get(id).map(|p| p.state.clone()))
        .and_then(|state| {
            state
                .strip_prefix(atpkg::state::MANAGED_PREFIX)
                .and_then(|rest| rest.split_whitespace().next())
                .map(str::to_string)
        });
    Some(Managed { path, version })
}

/// A copy of `id` on `PATH` outside atpkg's prefix.
fn foreign_of(
    id: &str,
    var: &dyn Fn(&str) -> Option<String>,
    pkg: Option<&atpkg::store::Layout>,
) -> Option<PathBuf> {
    let path = var("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| pkg.is_none_or(|l| !dir.starts_with(&l.prefix)))
        .map(|dir| dir.join(id))
        .find(|p| p.is_file())
}

/// Accounts v2's view of the serving Claude row: the active row when it is ready here, else the
/// own row when it is, else the first ready row, else the own row. A refused roster yields only
/// its refusal ([`ClaudeAccounts::refused`]), as does unreadable state or login bindings.
/// `None` with no harness state root.
fn claude_accounts(m: &Machine<'_>) -> Option<ClaudeAccounts> {
    use crate::harness::accounts;
    let store = accounts::Store::at(m.harness_state.as_ref()?);
    let loaded = m.roster.as_ref().map(|p| accounts::Roster::load(p));
    let cache = store
        .load(m.now)
        .and_then(|(state, _)| store.load_logins(m.now).map(|(logins, _)| (state, logins)));
    let refused = match &loaded {
        Some(Err(why)) => Some(crate::harness::one_line(why, 160)),
        _ => cache
            .as_ref()
            .err()
            .map(|why| crate::harness::one_line(why, 160)),
    };
    if refused.is_some() {
        return Some(ClaudeAccounts {
            label: crate::ai::login::OWN_LABEL.to_string(),
            listed: false,
            refused,
            renews: false,
            login: None,
            made: false,
            state: None,
            renew_by: None,
            limited_until: None,
            served_at: None,
            renewed_at: None,
            next_sign_in: None,
        });
    }
    let listed_roster = match loaded {
        Some(Ok(r)) if !r.rows.is_empty() => Some(r),
        _ => None,
    };
    let listed = listed_roster.is_some();
    let roster = listed_roster.unwrap_or_else(accounts::Roster::own);
    let (state, logins) = cache.ok()?;
    let bound = accounts::bound(&roster, &logins);
    let enrolled = accounts::enrolled(&bound);
    let own_row = bound
        .row_for_login(None)
        .or_else(|| {
            bound
                .rows
                .iter()
                .find(|r| r.was.iter().any(Option::is_none))
        })
        .or_else(|| bound.rows.first())?;
    let row_ready = |r: &accounts::Row| {
        let here = r.dir.is_none() || enrolled.contains(&r.label);
        here && matches!(
            accounts::condition(r, &state, true, m.now),
            accounts::Condition::Usable | accounts::Condition::Low | accounts::Condition::Unseen
        )
    };
    let row = state
        .active
        .as_deref()
        .and_then(|label| bound.row(label))
        .filter(|r| row_ready(r))
        .or_else(|| {
            if row_ready(own_row) {
                Some(own_row)
            } else {
                bound.rows.iter().find(|r| row_ready(r))
            }
        })
        .unwrap_or(own_row)
        .clone();
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
    let next_sign_in = if limited_until.is_some() || signed_out {
        accounts::renew_next(&bound, &state, &enrolled, Some(&row.label), m.now)
            .or_else(|| (signed_out && row.email.is_some()).then(|| row.label.clone()))
    } else {
        None
    };
    let (login_state, renew_by) = if renews {
        let login = accounts::login_state(&row, &logins, &state, m.now);
        let by = match login {
            accounts::LoginState::Due { by } => Some(by),
            _ => None,
        };
        (Some(login.as_str()), by)
    } else {
        (None, None)
    };
    Some(ClaudeAccounts {
        label: row.label.clone(),
        listed,
        refused,
        renews,
        login: row.dir.clone(),
        made,
        state: login_state,
        renew_by,
        limited_until,
        served_at: binding.and_then(|b| b.served_at),
        renewed_at: binding.and_then(|b| b.renewed_at),
        next_sign_in,
    })
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
    let Ok(Some(text)) =
        crate::harness::accounts::read_bounded(&home.join("config.toml"), 256 * 1024)
    else {
        return false;
    };
    aterm_toml::from_str::<aterm_toml::Value>(&text)
        .ok()
        .and_then(|doc| {
            doc.get("cli_auth_credentials_store")
                .and_then(aterm_toml::Value::as_str)
                .map(|s| matches!(s, "keyring" | "auto" | "ephemeral"))
        })
        .unwrap_or(false)
}

/// The end of Codex's usage limit from its own `rate_limits` readings
/// ([`super::codex_limits::home_pick`]): the latest reset ahead of a window at 100%, read only
/// from a reading no older than [`super::codex_limits::FRESH_S`].
fn codex_limit(home: &Path, now: i64) -> Option<i64> {
    use super::codex_limits as limits;
    let pick = limits::home_pick(home, now)?;
    if now.saturating_sub(pick.newest) > limits::FRESH_S {
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
    let kept = if !atpkg::vendor_direct::served_here(c.id) {
        Some(Kept::Unavailable)
    } else if m.exclude.iter().any(|e| e == c.id) {
        Some(Kept::Excluded)
    } else if managed.is_none()
        && m.pkg
            .as_ref()
            .is_some_and(|l| l.removed_programs().contains(c.id))
    {
        Some(Kept::Removed)
    } else {
        None
    };
    let login_file = home.as_ref().ok().and_then(|h| {
        c.stores
            .iter()
            .map(|rel| h.join(rel))
            .find(|p| p.is_file())
            .map(|p| {
                let w = mtime(&p);
                (p, w)
            })
    });
    let default_home = c
        .home
        .var
        .is_none_or(|v| (m.var)(v).is_none_or(|s| s.is_empty()));
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
    let hidden_store = is_codex && home.as_ref().is_ok_and(|h| codex_keyring(h));
    let limited_until = match (is_codex, home.as_ref()) {
        (true, Ok(h)) => codex_limit(h, m.now),
        _ => accounts.as_ref().and_then(|a| a.limited_until),
    };
    Facts {
        connector: c,
        kept,
        managed,
        foreign: foreign_of(c.id, m.var, m.pkg.as_ref()),
        home,
        home_exists,
        login_file,
        hidden_store,
        profile_signed_in,
        plan,
        accounts,
        limited_until,
        record: m.records.of(c.id),
        env_credentials: c
            .credential_env
            .iter()
            .copied()
            .filter(|k| (m.var)(k).is_some_and(|v| !v.is_empty()))
            .collect(),
        pool: crate::harness::accounts::Cli::parse(c.id).and_then(|cli| pool_for(m, cli)),
    }
}

/// `cli`'s own accounts pool, where its roster file sits beside `accounts.toml` and the harness
/// signs it in (`accounts renew --cli <id>`). `None` where there is no such roster or sign-in,
/// and for Claude Code, whose pool is [`ClaudeAccounts`].
#[must_use]
pub fn pool_for(m: &Machine<'_>, cli: crate::harness::accounts::Cli) -> Option<CliAccounts> {
    use crate::harness::accounts::{self, Cli};
    if cli == Cli::Claude {
        return None;
    }
    cli.accounts().signin.as_ref()?;
    let roster_path = m
        .roster
        .as_ref()?
        .parent()?
        .join(cli.accounts().roster_file);
    if !roster_path.is_file() {
        return None;
    }
    let roster = match accounts::Roster::load_for(cli, &roster_path) {
        Ok(roster) => roster,
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
            store
                .load(m.now)
                .and_then(|(state, _)| store.load_logins(m.now).map(|(logins, _)| (state, logins)))
        }
        _ => Err("account store is unavailable".to_owned()),
    };
    let (state, logins) = match cache {
        Ok(cache) => cache,
        Err(why) => {
            return Some(CliAccounts {
                roster: roster_path,
                refused: Some(why),
                ..CliAccounts::default()
            });
        }
    };
    let bound = accounts::bound(&roster, &logins);
    let enrolled = accounts::enrolled_for(cli, &bound);
    let with_email = || bound.rows.iter().filter(|r| r.email.is_some());
    let due = accounts::renew_due(&bound, &logins, &state, &enrolled, m.now);
    let next = with_email()
        .find(|r| !enrolled.contains(&r.label))
        .map(|r| r.label.clone())
        .or_else(|| due.first().cloned())
        .or_else(|| with_email().next().map(|r| r.label.clone()));
    Some(CliAccounts {
        roster: roster_path,
        refused: None,
        next,
        rows: bound.rows.len(),
        enrolled: bound
            .rows
            .iter()
            .filter(|r| enrolled.contains(&r.label))
            .count(),
        due,
    })
}

#[cfg(test)]
#[path = "facts_tests.rs"]
mod tests;
