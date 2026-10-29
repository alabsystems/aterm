// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Interactive vendor CLI sign-in flows.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::connector::{Connector, Login, Status};
use super::facts::{Facts, Job, Ran, RunFailed, Runner};
use super::run::RunDir;
use super::state::{Row, State};
use super::store::{Ended, How, Outcome, Store};

/// The status read's time limit.
pub const STATUS_BUDGET: Duration = Duration::from_secs(20);
/// The hang detector of aterm's own renewal run with nobody at this Mac ([`Host::attended`]):
/// past a sign-in's own ([`crate::harness::login_drive::HANG`]) and the sign-in lock's wait.
pub const RENEW_BUDGET: Duration = Duration::from_secs(30 * 60);
/// The label accounts v2 gives the machine's own row (`harness::cli::ACCOUNTS_OWN_LABEL`).
pub const OWN_LABEL: &str = "main";

pub use aterm_types::domain::{AGENT_MARKERS, agent_marker};

/// What a sign-in came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// The status read says it is signed in (`Some(method)`), or accounts v2 signed it in (`None`:
    /// its `accounts renew`, or a pool's `accounts login`).
    SignedIn(Option<&'static str>),
    /// The flow ended and nothing could read whether a login was made.
    NotChecked,
    /// The status read says nobody is signed in.
    SignedOut,
    /// The flow failed; why.
    Failed(String),
    /// It was not started; why.
    Refused(String),
}

/// What a sign-in needs of the machine.
pub struct Host<'a> {
    /// The child processes.
    pub runner: &'a dyn Runner,
    /// One variable of this process's environment.
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// This process's whole environment.
    pub vars: &'a [(String, String)],
    /// This program, for `aterm harness accounts …`.
    pub aterm: &'a Path,
    /// The AI store.
    pub store: &'a Store,
    /// Unix seconds, read when asked.
    pub now: &'a dyn Fn() -> i64,
    /// Asks the person a question on the terminal and answers the line typed.
    pub ask: &'a dyn Fn(&str) -> Option<String>,
    /// The sandbox a CLI's own sign-in runs under over SSH where its
    /// [`super::connector::SignIn::no_desk`] names this system
    /// ([`crate::harness::login_drive::SANDBOX_EXEC`]); `None`, or one that cannot run here,
    /// refuses that sign-in ([`crate::harness::login_drive::off_this_desk`]).
    pub no_desk: Option<&'a Path>,
    /// Whether a person is at this run: a terminal on stdin and no agent's marker. With nobody
    /// here aterm's own renewals run with stdin closed, so they take only their headless routes
    /// and never a browser on the desk, and only where nothing bars or holds them
    /// ([`super::state::bar`], [`super::state::held`]).
    pub attended: bool,
}

/// The environment a sign-in runs with.
#[must_use]
pub fn sign_in_env(
    c: &Connector,
    vars: &[(String, String)],
    var: &dyn Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vars
        .iter()
        .filter(|(k, _)| {
            !aterm_types::domain::is_ai_env_var(k) && !c.credential_env.contains(&k.as_str())
        })
        .filter(|(k, _)| !AGENT_MARKERS.contains(&k.as_str()))
        .cloned()
        .collect();
    if let Some(name) = c.home.var
        && let Some(v) = var(name).filter(|v| !v.is_empty())
    {
        env.retain(|(k, _)| k != name);
        env.push((name.to_string(), v));
    }
    env
}

/// What a status read said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Signed in, this way.
    In(&'static str),
    /// Nobody is signed in.
    Out,
    /// Nothing it can be read as; why.
    Unread(String),
}

/// Reads a status run against its closed grammar, error output first (Codex prints there).
#[must_use]
pub fn read_status(status: &Status, ran: &Result<Ran, RunFailed>) -> Said {
    let ran = match ran {
        Ok(ran) => ran,
        Err(RunFailed::Timeout) => return Said::Unread("timed out".to_string()),
        Err(RunFailed::NotRun(why)) => return Said::Unread(crate::harness::one_line(why, 80)),
    };
    for line in ran.stderr.lines().chain(ran.stdout.lines()) {
        let line = line.trim();
        for (prefix, method) in status.lines {
            if line.starts_with(prefix) {
                return match method {
                    Some(m) => Said::In(m),
                    None => Said::Out,
                };
            }
        }
    }
    Said::Unread(format!(
        "an answer this build does not read (exit {:?})",
        ran.exit
    ))
}

fn as_args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// Why accounts v2's sign-in of Claude Code starts nothing, whoever runs it and wherever, where its
/// facts alone decide that ([`claude_mend`]), and what a person does at this Mac instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mend {
    /// Why nothing starts, one line: `accounts.toml is refused (…)`.
    pub why: String,
    /// What to do, said after `why` in the refusal ([`Mend::refusal`]): fix it (…), then run
    /// `aterm ai login claude`.
    pub then: String,
    /// What a person does, said alone (a row's [`super::state::Row::mend`], a `NEEDS YOU:`
    /// line), never naming a command that starts nothing until it is done: mend accounts.toml
    /// (…), then sign claude in.
    pub step: String,
}

impl Mend {
    /// The refusal `aterm ai login claude` and `aterm ai doctor` print, with a terminal or none.
    #[must_use]
    pub fn refusal(&self) -> String {
        format!("{}: {}", self.why, self.then)
    }
}

/// What keeps accounts v2's sign-in of Claude Code (`f`'s, whose login is
/// [`Login::Accounts`]) from starting anything, whoever runs it and wherever, where its facts
/// alone decide that: its home's variable is set ([`Facts::home_var`]), so the home is not the
/// default one accounts v2 signs in; accounts.toml is refused; accounts v2's records cannot be
/// read; or accounts.toml lists this machine's own login with no email and no row a sign-in
/// moves to. `None` for any other CLI and where none of those holds.
#[must_use]
pub fn claude_mend(f: &Facts) -> Option<Mend> {
    if !matches!(f.connector.login, Login::Accounts) {
        return None;
    }
    let id = f.connector.id;
    if let Some(dir) = &f.home_var {
        let name = f.connector.home.var.unwrap_or("CLAUDE_CONFIG_DIR");
        let dir = crate::supervise::limit::one_line(dir);
        return Some(Mend {
            why: format!("{name} is set ({dir})"),
            then: "aterm signs in only the default home's Claude login, through accounts v2; sign \
                   in under that home with `claude` and its /login"
                .to_string(),
            step: format!("sign {id} in under {name}'s home ({dir}) with `claude` and its /login"),
        });
    }
    let a = f.accounts.as_ref()?;
    if let Some(why) = a.refused.as_deref() {
        return Some(Mend {
            why: format!(
                "accounts.toml is refused ({})",
                crate::harness::one_line(why, 160)
            ),
            then: format!(
                "fix it (`aterm harness accounts` says how), then run `aterm ai login {id}`"
            ),
            step: format!(
                "mend accounts.toml (`aterm harness accounts` says how), then sign {id} in"
            ),
        });
    }
    if let Some(why) = a.unread.as_deref() {
        return Some(Mend {
            why: format!("accounts v2's records cannot be read ({why})"),
            then: format!(
                "make them readable again (`aterm harness accounts` says why), then run `aterm ai \
                 login {id}`"
            ),
            step: format!(
                "make accounts v2's records readable again ({why}; `aterm harness accounts` says \
                 why), then sign {id} in"
            ),
        });
    }
    (a.listed && !a.renews && a.next_sign_in.is_none()).then(|| Mend {
        why: format!(
            "accounts.toml lists this machine's own login as `{}` with no email",
            a.label
        ),
        then: format!("add `email = \"…\"` to that row, then `aterm ai login {id}` renews it"),
        step: format!(
            "add `email = \"…\"` to `{}`'s row in accounts.toml, then sign {id} in",
            a.label
        ),
    })
}

fn claude(h: &Host<'_>, f: &Facts, out: &mut dyn Write) -> Done {
    // A home other than the default one (`CLAUDE_CONFIG_DIR`), a refused accounts.toml,
    // unreadable records, or this machine's own login listed with no email was refused before
    // this ([`claude_mend`], through [`refusal`]).
    let renews = f
        .accounts
        .as_ref()
        .is_some_and(|a| a.renews || a.next_sign_in.is_some());
    let label = f
        .accounts
        .as_ref()
        .and_then(|a| a.next_sign_in.clone().or_else(|| Some(a.label.clone())))
        .unwrap_or_else(|| OWN_LABEL.to_string());
    let env: Vec<(String, String)> = h.vars.to_vec();
    if !renews && let Some(marker) = agent_marker(h.var) {
        return Done::Refused(format!(
            "{marker} is set: accounts v2 renews this login once it knows the Claude account's \
             email, which a person gives (`aterm ai login claude` at a terminal)"
        ));
    }
    if !renews {
        let _ = writeln!(
            out,
            "Claude Code signs in through accounts v2's renewal: a fresh login, checked against \
             the account's email before any session uses it."
        );
        let Some(email) = (h.ask)("The Claude account's email (Enter skips): ")
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty())
        else {
            return Done::Refused("no email was given".to_string());
        };
        if !email.contains('@') || email.chars().any(char::is_whitespace) {
            return Done::Refused("that is not an email address".to_string());
        }
        let args = as_args(&[
            "harness",
            "accounts",
            "adopt-default",
            &label,
            "--email",
            &email,
        ]);
        match h.runner.terminal(h.aterm, &args, &env, None) {
            Ok(0) => {}
            Ok(code) => {
                return Done::Failed(format!(
                    "`aterm harness accounts adopt-default` exited {code}"
                ));
            }
            Err(why) => return Done::Failed(why),
        }
    }
    if renews && let Some(why) = unattended_refusal(h, f) {
        return Done::Refused(why);
    }
    let args = as_args(&["harness", "accounts", "renew", &label]);
    match renew_run(h, &args, &env, out) {
        Ok(0) => Done::SignedIn(None),
        Ok(code) => Done::Failed(format!(
            "`aterm harness accounts renew {label}` exited {code}"
        )),
        Err(why) => Done::Failed(why),
    }
}

/// Whether `pool`'s next sign-in is Codex's own in this machine's default home, not the pool's:
/// its next row ([`super::facts::CliAccounts::next`]) is the row on this machine's own login,
/// which has no current login here ([`crate::harness::codex_alab::DefaultRow::SignedOut`]), on
/// a Codex program that keeps no row's login its own ([`super::facts::CliAccounts::unread`]),
/// where a pool sign-in would bind that row to a login of its own that the program never runs.
#[must_use]
pub fn default_home_sign_in(pool: &super::facts::CliAccounts) -> bool {
    pool.unread
        && pool.refused.is_none()
        && pool.next.is_some()
        && pool.next == pool.default_label
        && matches!(
            pool.default_row,
            Some(crate::harness::codex_alab::DefaultRow::SignedOut { .. })
        )
}

/// Why aterm's own renewal of `f`'s row does not start with nobody at this run
/// ([`Host::attended`]), where it does not: something bars it from running unattended, so it is
/// a person's ([`super::state::bar`]), or the window's upkeep holds it ([`super::state::held`]).
fn unattended_refusal(h: &Host<'_>, f: &Facts) -> Option<String> {
    if h.attended {
        return None;
    }
    if let Some(bar) = super::state::bar(f) {
        return Some(format!(
            "{}, so a person does it, once, at this Mac (`{}`)",
            bar.why, bar.step
        ));
    }
    super::state::held(f, (h.now)())
}

/// Runs aterm's own renewal, `aterm harness accounts renew …`: on this terminal for a person,
/// where it may fall back to a browser the person signs in to; with nobody here
/// ([`Host::attended`]), with stdin closed and its output copied to `out`, so it takes only its
/// headless routes and never opens a browser on the desk.
///
/// # Errors
///
/// Why it did not run to an exit.
fn renew_run(
    h: &Host<'_>,
    args: &[String],
    env: &[(String, String)],
    out: &mut dyn Write,
) -> Result<i32, String> {
    if h.attended {
        return h.runner.terminal(h.aterm, args, env, None);
    }
    let job = Job {
        program: h.aterm,
        args,
        env,
        stdin: None,
        cwd: None,
        budget: RENEW_BUDGET,
    };
    match h.runner.run(&job) {
        Ok(ran) => {
            let _ = out.write_all(ran.stdout.as_bytes());
            let _ = out.write_all(ran.stderr.as_bytes());
            ran.exit
                .ok_or_else(|| "the renewal was ended by a signal".to_string())
        }
        Err(RunFailed::Timeout) => Err(format!(
            "the renewal ran past {} min and was ended",
            RENEW_BUDGET.as_secs() / 60
        )),
        Err(RunFailed::NotRun(why)) => Err(crate::harness::one_line(&why, 160)),
    }
}

/// The row a sign-in of `cli`'s pool signs in and its verb
/// ([`super::facts::CliAccounts::sign_in_target`]), or why the pool's sign-in has nothing to run,
/// whoever runs it.
///
/// # Errors
///
/// That reason, as the refusal says it: the roster is refused; the row is this machine's own
/// login's on a Codex build that keeps no row's login its own ([`default_home_sign_in`]), whose
/// sign-in is Codex's own, which [`sign_in`] runs; that build serves that row alone
/// ([`super::facts::CliAccounts::default_only`]) and its login still works, or that row cannot
/// take a launch here for another reason the pool names (held for another Mac's warm set, leased
/// elsewhere, unclaimed where this Mac cannot record its claims:
/// [`crate::harness::codex_alab::DefaultRow::Other`]), or its renewal is due while its
/// login still works; a launch the pool refuses as the records stand
/// ([`super::facts::CliAccounts::launch_refused`]), in its words; no row to sign in now where a
/// row carries an email; or the roster lists no row (or no email) to sign in.
pub fn pool_target(
    cli: crate::harness::accounts::Cli,
    pool: &super::facts::CliAccounts,
) -> Result<(&str, &'static str), String> {
    let id = cli.id();
    let roster = pool.roster.display();
    if let Some(why) = &pool.refused {
        return Err(format!(
            "{roster} is refused ({}): fix it (`aterm harness accounts --cli {id}` says how), then \
             run `aterm ai login {id}`",
            crate::harness::one_line(why, 160),
        ));
    }
    if default_home_sign_in(pool)
        && let Some(label) = &pool.default_label
    {
        return Err(format!(
            "`{label}`, the row on this machine's own login, signs in by Codex's own sign-in in \
             its default home on this Codex build, which `{}` runs",
            crate::harness::codex_alab::DEFAULT_SIGN_IN
        ));
    }
    if let Some(target) = pool.sign_in_target(cli) {
        return Ok(target);
    }
    if pool.unread
        && let Some(label) = &pool.default_label
    {
        if let Some(crate::harness::codex_alab::DefaultRow::Other { why, .. }) = &pool.default_row {
            return Err(format!(
                "`{label}`, the row on this machine's own login, is the only one this Codex \
                 build serves, and it cannot take a launch here now ({why}), so nothing here \
                 signs it in"
            ));
        }
        if pool.due.contains(label) {
            return Err(format!(
                "`{label}`, the row on this machine's own login, is the only one this Codex \
                 build serves, and its login still works: only Codex's own sign-in in its \
                 default home renews it here, and that signs the login out (revokes it) \
                 before it starts, so `{}` runs it once that login has ended",
                crate::harness::codex_alab::DEFAULT_SIGN_IN
            ));
        }
        if pool.default_only {
            return Err(format!(
                "`{label}`, the row on this machine's own login, is the only one this Codex \
                 build serves, and it needs no sign-in"
            ));
        }
    }
    // A launch the pool refuses as the records stand (the owner's pin on a row it cannot take,
    // among others) is what keeps the pool from running, not a missing row or email.
    if let Some(why) = &pool.launch_refused {
        let why = why
            .strip_suffix(crate::harness::accounts_pool::NO_WORKER)
            .unwrap_or(why);
        return Err(format!(
            "{roster} names no row `aterm ai login {id}` signs in, and a launch of {id} now is \
             refused: {why}"
        ));
    }
    Err(if pool.emails > 0 {
        format!(
            "{roster} names no row `aterm ai login {id}` signs in now: `aterm harness accounts \
             --cli {id}` says each row's state"
        )
    } else if cli.renews_headlessly() {
        format!(
            "{roster} lists no email: run `aterm harness accounts add <label> --cli {id} \
             --email <e>` (kimi also `--region <r>`)"
        )
    } else {
        format!("{roster} lists no row: run `aterm harness accounts add <label> --cli {id}`")
    })
}

/// A CLI with its own accounts pool signs in through it, for the pool's next row (a row with an
/// email, or for Grok Build's any row, [`super::facts::CliAccounts::next`]): `aterm harness
/// accounts <verb> <label> --cli <id>`, the verb that signs the CLI's row in
/// ([`crate::harness::accounts::Cli::sign_in_verb`]). That is `renew`, a fresh login in its own
/// staging home, its page pinned and approved headlessly and its account checked before any
/// session uses it, else `login`, the same sign-in on this terminal for the person to approve
/// (Grok Build's, whose page answers a headless browser with a block page), which is refused
/// under an agent's marker. A `renew` with nobody at this run ([`Host::attended`]) runs headless
/// only, where nothing bars or holds it. Where the pool has no row to sign in ([`pool_target`]:
/// among others, the next row is this machine's own login's on a Codex build that keeps no row's
/// login its own, whose sign-in [`sign_in`] runs, or that build's only row is one nothing here
/// signs in), it runs nothing and says why.
pub fn pool_sign_in(
    h: &Host<'_>,
    f: &Facts,
    cli: crate::harness::accounts::Cli,
    pool: &super::facts::CliAccounts,
    out: &mut dyn Write,
) -> Done {
    let id = cli.id();
    let (label, verb) = match pool_target(cli, pool) {
        Ok(target) => target,
        Err(why) => return Done::Refused(why),
    };
    if verb != "renew"
        && let Some(marker) = agent_marker(h.var)
    {
        return Done::Refused(format!(
            "{marker} is set: `aterm harness accounts {verb} {label} --cli {id}` is a person's \
             approval at this Mac, not an agent's"
        ));
    }
    if verb == "renew"
        && let Some(why) = unattended_refusal(h, f)
    {
        return Done::Refused(why);
    }
    let args = as_args(&["harness", "accounts", verb, label, "--cli", id]);
    let env: Vec<(String, String)> = h.vars.to_vec();
    let ran = if verb == "renew" {
        renew_run(h, &args, &env, out)
    } else {
        h.runner.terminal(h.aterm, &args, &env, None)
    };
    match ran {
        Ok(0) => Done::SignedIn(None),
        Ok(code) => Done::Failed(format!(
            "`aterm harness accounts {verb} {label} --cli {id}` exited {code}"
        )),
        Err(why) => Done::Failed(why),
    }
}

/// Why [`sign_in`] runs nothing for `row`, whoever runs it and wherever, where `row` and its
/// facts alone decide that: the row is off (its fact) or not installed, a sign-in through its
/// pool has nothing to run ([`pool_target`]), accounts v2's sign-in of Claude Code has nothing
/// to run ([`claude_mend`]: a home other than the default one, a refused accounts.toml,
/// unreadable records, or this machine's own login listed with no email), or the CLI's own
/// login would sign a working login out first. `None` where a person's run at a terminal would
/// start something; the run may still refuse for what only it knows (an agent's marker, SSH with
/// no sign-in kept off this desk).
#[must_use]
pub fn refusal(f: &Facts, row: &Row) -> Option<String> {
    let c = f.connector;
    if matches!(row.state, State::Off) {
        return Some(row.fact.clone());
    }
    if matches!(row.state, State::NotInstalled) {
        return Some(format!(
            "not installed (`aterm ai login {}` and `aterm ai doctor` install it first)",
            c.id
        ));
    }
    let pooled_default = match (&f.pool, crate::harness::accounts::Cli::parse(c.id)) {
        (Some(pool), Some(cli)) if !default_home_sign_in(pool) => {
            return pool_target(cli, pool).err();
        }
        (Some(_), Some(_)) => true,
        _ => false,
    };
    // A pool's row on this machine's own login reaches this only while no login of it here is
    // current ([`super::facts::CliAccounts::next`]).
    match c.login {
        Login::Run {
            signs_out_first: true,
            ..
        } if !pooled_default && !matches!(row.state, State::SignedOut) => Some(format!(
            "{} is signed in; its own login signs that login out (revokes it) before it starts",
            c.product
        )),
        Login::Accounts => claude_mend(f).map(|m| m.refusal()),
        Login::Run { .. } => None,
    }
}

/// Signs `row`'s CLI in: Claude Code through accounts v2, a CLI with a roster through its pool
/// ([`pool_sign_in`]), else the CLI's own login on this terminal, kept off this machine's desk
/// over SSH ([`crate::harness::login_drive::off_this_desk`]: a browser it opened there would be
/// on a desk nobody is at, so on macOS Grok Build's and Codex's run under [`Host::no_desk`] and
/// are refused where it cannot run). The renewals are aterm's own and run under an agent too, with
/// nobody here headless only and only where nothing bars or holds them ([`Host::attended`]); what
/// only a person gives (the CLI's own sign-in, approved in a browser, and the email Claude's
/// adoption of the default login asks) is refused under an agent's marker. The row on this
/// machine's own login of a Codex pool whose program keeps no row's login its own
/// ([`default_home_sign_in`]) signs in by Codex's own login too, which rewrites the default
/// home's login and leaves the row on it; once Codex's status reads signed in, the pool's
/// records say that row is signed in again ([`crate::harness::accounts::note_signed_back`]).
#[must_use]
pub fn sign_in(h: &Host<'_>, f: &Facts, row: &Row, out: &mut dyn Write) -> Done {
    let c = f.connector;
    if let Some(why) = refusal(f, row) {
        return Done::Refused(why);
    }
    // Where Codex's own login signs in the pool's row on this machine's own login: that row's
    // label and the pool's store.
    let default_row = match (&f.pool, crate::harness::accounts::Cli::parse(c.id)) {
        (Some(pool), Some(cli)) if !default_home_sign_in(pool) => {
            return pool_sign_in(h, f, cli, pool, out);
        }
        (Some(pool), Some(_)) => pool.default_label.as_deref().zip(pool.store.as_ref()),
        _ => None,
    };
    let (args, remote, tell, after) = match &c.login {
        Login::Accounts => return claude(h, f, out),
        Login::Run {
            args,
            remote,
            tell,
            after,
            ..
        } => (args, remote, tell, after),
    };
    if let Some(marker) = agent_marker(h.var) {
        return Done::Refused(format!(
            "{marker} is set: {}'s own sign-in is a person's approval in a browser, not an \
             agent's",
            c.product
        ));
    }
    let Some(managed) = &f.managed else {
        return Done::Refused(format!(
            "not installed (`aterm ai login {}` and `aterm ai doctor` install it first)",
            c.id
        ));
    };
    let run = match RunDir::make(h.store.dir()) {
        Ok(run) => run,
        Err(e) => return Done::Failed(format!("no private run directory: {e}")),
    };
    let under_ssh = (h.var)("SSH_CONNECTION").is_some_and(|v| !v.is_empty());
    let words = match (under_ssh, remote) {
        (true, Some(r)) => *r,
        _ => *args,
    };
    let kept = match crate::harness::login_drive::off_this_desk(
        c.accounts.and_then(|a| a.signin.as_ref()),
        c.product,
        (
            managed.path.clone(),
            words.iter().copied().map(OsString::from).collect(),
        ),
        (under_ssh, h.no_desk),
        h.store.dir(),
        &|k| (h.var)(k).map(OsString::from),
    ) {
        Ok(kept) => kept,
        Err(why) => return Done::Refused(why),
    };
    let (Some(program_args), Some(path)) = (
        kept.args
            .iter()
            .map(|a| a.to_str().map(str::to_owned))
            .collect::<Option<Vec<String>>>(),
        kept.path.to_str(),
    ) else {
        return Done::Refused(format!(
            "{}'s sign-in command or its PATH is not UTF-8 here",
            c.product
        ));
    };
    let mut env = sign_in_env(c, h.vars, h.var);
    if kept.held.is_some() {
        env.retain(|(k, _)| k != "PATH");
        env.push(("PATH".to_string(), path.to_string()));
    }
    let _ = writeln!(out, "{}: {tell}", c.product);
    let _ = out.flush();
    let exited = h
        .runner
        .terminal(&kept.program, &program_args, &env, Some(run.path()));
    if let Err(why) = &exited {
        return Done::Failed(format!("{} could not be started: {why}", c.id));
    }
    let Some(status) = after else {
        return Done::NotChecked;
    };
    let ran = h.runner.run(&Job {
        program: &managed.path,
        args: &as_args(status.args),
        env: &env,
        stdin: None,
        cwd: Some(run.path()),
        budget: STATUS_BUDGET,
    });
    let now = (h.now)();
    let default_home = c
        .home
        .var
        .is_none_or(|v| (h.var)(v).is_none_or(|s| s.is_empty()));
    let note = |outcome| {
        if default_home {
            let _ = h.store.note(c.id, How::Login, outcome, now);
        }
    };
    match read_status(status, &ran) {
        Said::In(method) => {
            note(Outcome::Worked);
            if default_home && let Some((label, pool_store)) = default_row {
                let _ = pool_store.transact_logins(now, &mut None, |state, logins| {
                    Ok(crate::harness::accounts::note_signed_back(
                        state, logins, label, now,
                    ))
                });
            }
            Done::SignedIn(Some(method))
        }
        Said::Out => {
            note(Outcome::Ended(Ended::StatusSignedOut));
            Done::SignedOut
        }
        Said::Unread(_) => match exited {
            Ok(0) => Done::NotChecked,
            Ok(code) => Done::Failed(format!("{} exited {code}", c.id)),
            Err(why) => Done::Failed(why),
        },
    }
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
