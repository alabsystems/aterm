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
/// The label accounts v2 gives the machine's own row (`harness::cli::ACCOUNTS_OWN_LABEL`).
pub const OWN_LABEL: &str = "main";

pub use aterm_types::domain::{AGENT_MARKERS, agent_marker};

/// What a sign-in came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// The status read says it is signed in (`Some(method)`), or accounts v2 renewed it (`None`).
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

fn claude(h: &Host<'_>, f: &Facts, out: &mut dyn Write) -> Done {
    if let Some(dir) = (h.var)(
        super::connector::CLAUDE
            .home
            .var
            .unwrap_or("CLAUDE_CONFIG_DIR"),
    )
    .filter(|v| !v.is_empty())
    {
        return Done::Refused(format!(
            "CLAUDE_CONFIG_DIR is set ({}): aterm signs in only the default home's Claude login, \
             through accounts v2; sign in under that home with `claude` and its /login",
            crate::harness::one_line(&dir, 80)
        ));
    }
    if let Some(why) = f.accounts.as_ref().and_then(|a| a.refused.as_deref()) {
        return Done::Refused(format!(
            "accounts.toml is refused ({why}): fix it (`aterm harness accounts` says how), then run `aterm ai login claude`"
        ));
    }
    if let Some(a) = f
        .accounts
        .as_ref()
        .filter(|a| a.listed && !a.renews && a.next_sign_in.is_none())
    {
        return Done::Refused(format!(
            "accounts.toml lists this machine's own login as `{}` with no email: add `email = \"…\"` to that row, then `aterm ai login claude` renews it",
            a.label
        ));
    }
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
    let args = as_args(&["harness", "accounts", "renew", &label]);
    match h.runner.terminal(h.aterm, &args, &env, None) {
        Ok(0) => Done::SignedIn(None),
        Ok(code) => Done::Failed(format!(
            "`aterm harness accounts renew {label}` exited {code}"
        )),
        Err(why) => Done::Failed(why),
    }
}

/// The CLI a pool sign-in names: its id (`--cli`), the one field of a [`Connector`] it reads.
#[derive(Debug, Clone, Copy)]
pub struct PoolCli<'a> {
    /// `codex`, `grok`, `kimi`.
    pub id: &'a str,
}

/// A CLI with its own accounts pool signs in through it: `aterm harness accounts renew <label>
/// --cli <id>` for the pool's next row, a fresh login in its own staging home, its page pinned
/// and its account checked before any session uses it.
pub fn pool_sign_in(h: &Host<'_>, c: PoolCli<'_>, pool: &super::facts::CliAccounts) -> Done {
    let roster = pool.roster.display();
    if let Some(why) = &pool.refused {
        return Done::Refused(format!(
            "{roster} is refused ({}): fix it (`aterm harness accounts --cli {}` says how), then \
             run `aterm ai login {}`",
            crate::harness::one_line(why, 160),
            c.id,
            c.id
        ));
    }
    let Some(label) = &pool.next else {
        return Done::Refused(format!(
            "{roster} lists no email: run `aterm harness accounts add <label> --cli {} --email \
             <e>` (kimi also `--region <r>`)",
            c.id
        ));
    };
    let args = as_args(&["harness", "accounts", "renew", label, "--cli", c.id]);
    let env: Vec<(String, String)> = h.vars.to_vec();
    match h.runner.terminal(h.aterm, &args, &env, None) {
        Ok(0) => Done::SignedIn(None),
        Ok(code) => Done::Failed(format!(
            "`aterm harness accounts renew {label} --cli {}` exited {code}",
            c.id
        )),
        Err(why) => Done::Failed(why),
    }
}

/// Signs `row`'s CLI in: Claude Code through accounts v2, a CLI with a roster through its pool
/// ([`pool_sign_in`]), else the CLI's own login on this terminal, kept off this machine's desk
/// over SSH ([`crate::harness::login_drive::off_this_desk`]: a browser it opened there would be
/// on a desk nobody is at, so on macOS Grok Build's and Codex's run under [`Host::no_desk`] and
/// are refused where it cannot run).
#[must_use]
pub fn sign_in(h: &Host<'_>, f: &Facts, row: &Row, out: &mut dyn Write) -> Done {
    let c = f.connector;
    if let Some(marker) = agent_marker(h.var) {
        return Done::Refused(format!(
            "{marker} is set: a sign-in is a person's act, not an agent's"
        ));
    }
    if matches!(row.state, State::Off) {
        return Done::Refused(row.fact.clone());
    }
    if matches!(row.state, State::NotInstalled) {
        return Done::Refused(format!(
            "not installed: run `aterm pkg install {}` first",
            c.id
        ));
    }
    if let Some(pool) = &f.pool {
        return pool_sign_in(h, PoolCli { id: c.id }, pool);
    }
    let (args, remote, tell, signs_out_first, after) = match &c.login {
        Login::Accounts => return claude(h, f, out),
        Login::Run {
            args,
            remote,
            tell,
            signs_out_first,
            after,
        } => (args, remote, tell, *signs_out_first, after),
    };
    if signs_out_first && !matches!(row.state, State::SignedOut) {
        return Done::Refused(format!(
            "{} is signed in; its own login signs that login out (revokes it) before it starts",
            c.product
        ));
    }
    let Some(managed) = &f.managed else {
        return Done::Refused(format!(
            "not installed: run `aterm pkg install {}` first",
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
