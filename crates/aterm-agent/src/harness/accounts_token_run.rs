// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm harness accounts token-run (<label> | <email> | --auto) -- <program> <args…>`: aterm's
//! token wrapper (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.3, slice S8).
//!
//! A relaunch line for a token decision types it with the row's label (or a row-less account's
//! email) before the store build; the managed twin's token block types it with `--auto` before a
//! `claude` a person typed in a tab (`atpkg::launch_tap::with_token_block`). It asks the one judgment
//! ([`token_route::judge`]) again, and:
//!
//! - **falls back** wherever the judgment does not say [`Decision::Token`]: it execs the program
//!   exactly as typed, with no descriptor, and journals `token-fallback` with the reason word
//!   where the account has a token entry (a launch on an account with none says nothing);
//! - **serves** otherwise: it unsets every [`accounts::CREDENTIAL_KEYS`] variable (today's
//!   judgment left only empty ones) and any inherited marker, sets
//!   [`token_route::MARKER_ENV`] to the entry, writes the token and a newline into one end of a
//!   socketpair and closes it, and execs the program holding the other end as an inherited guard
//!   numbered in [`token_route::DESCRIPTOR_ENV`]. The token is never in argv, the environment, a
//!   file, the scrollback or a pipe another process reads.
//!
//! It writes no table and takes no harness lock: a relaunch holds `accounts.lock` from admission
//! through dispatch, and an unlabelled run reads the claims ring from a snapshot. A labelled
//! line was admitted under that lock before it was typed, so it reads the claims ring not at all.

use super::accounts::{self, ProcessLogin, Roster, Store, TokenEntry};
use super::token_route::{self, Account, Ask, Decision, Facts, Fallback, Named, TokenFact};
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// What the verb was typed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The label or email, or `None` for `--auto`.
    pub named: Option<String>,
    /// The program to run.
    pub program: OsString,
    /// Its arguments.
    pub args: Vec<OsString>,
}

impl Run {
    /// How the judgment names the account.
    #[must_use]
    pub fn named(&self) -> Named<'_> {
        self.named.as_deref().map_or(Named::Auto, Named::Label)
    }
}

/// The verb's words after `accounts token-run`: `(<label> | <email> | --auto) -- <program>
/// <args…>`.
///
/// # Errors
/// Any other shape.
pub fn parse(args: &[OsString]) -> Result<Run, String> {
    const USAGE: &str =
        "usage: aterm harness accounts token-run (<label> | <email> | --auto) -- <program> <args…>";
    let [named, dash, program, rest @ ..] = args else {
        return Err(USAGE.to_owned());
    };
    if dash != "--" {
        return Err(USAGE.to_owned());
    }
    let named = named.to_str().ok_or(USAGE)?;
    let named = match named {
        token_route::AUTO => None,
        "" => return Err(USAGE.to_owned()),
        name if name.starts_with('-') => return Err(USAGE.to_owned()),
        name => Some(name.to_owned()),
    };
    Ok(Run {
        named,
        program: program.clone(),
        args: rest.to_vec(),
    })
}

/// What the wrapper reads of its world: the process's in a run ([`Ctx::from_process`]), a test's
/// own otherwise.
pub struct Ctx {
    /// The environment the program would run with.
    pub env: Vec<(String, String)>,
    /// The user's home (`HOME`).
    pub home: Option<PathBuf>,
    /// The launch directory.
    pub cwd: PathBuf,
    /// Claude Code's roster, where one is there.
    pub roster: Option<Roster>,
    /// Claude Code's accounts store.
    pub store: Option<Store>,
    /// atpkg's own store of Claude Code builds (`<prefix>/store/claude`), the only builds the
    /// wrapper serves ([`token_route::store_build_in`]).
    pub builds: Option<PathBuf>,
    /// This Mac's vault, where it has one.
    #[cfg(unix)]
    pub vault: Option<aterm_secrets::client::Paths>,
    /// Unix seconds.
    pub now: i64,
    /// This is macOS.
    pub macos: bool,
}

impl Ctx {
    /// The process's own: its environment, `HOME`, directory, the configured roster and store,
    /// this Mac's vault and the clock.
    #[must_use]
    pub fn from_process() -> Self {
        let env: Vec<(String, String)> = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let found = super::cli::Env::from_process().ok();
        let roster = found
            .as_ref()
            .and_then(|env| env.accounts_roster_for(accounts::Cli::Claude))
            .filter(|path| path.exists())
            .and_then(|path| Roster::load_for(accounts::Cli::Claude, &path).ok());
        Self {
            env,
            home: found.as_ref().and_then(|env| env.home.clone()),
            cwd: std::env::current_dir().unwrap_or_default(),
            roster,
            store: found.as_ref().map(super::cli::Env::accounts_store),
            builds: token_route::configured_builds(),
            #[cfg(unix)]
            vault: aterm_secrets::client::Paths::live().ok(),
            now: found.as_ref().map_or(0, |env| env.now),
            macos: cfg!(target_os = "macos"),
        }
    }
}

/// How long a labelled line waits for a vault writer: a relaunch typed it on purpose.
pub const LABELLED_WAIT: Duration = Duration::from_secs(2);

/// [`Ask`] over a [`Ctx`]: today's judgment of the launch, the claims snapshot, the vault.
struct Asked<'a> {
    run: &'a Run,
    ctx: &'a Ctx,
    argv: &'a [String],
    env: &'a [(&'a str, &'a str)],
    /// The entry today's judgment resolved, for the fallback's journal.
    entry: Cell<Option<TokenEntry>>,
    /// Whether the token step found an entry (anything but `absent`).
    found: Cell<bool>,
    /// The read the token step made, to serve from.
    read: RefCell<Option<Read>>,
}

/// What the token step read, to serve from (nothing off Unix, where no vault is read).
#[cfg(unix)]
type Read = aterm_secrets::claude_token::TokenRead;
#[cfg(not(unix))]
type Read = ();

impl Ask for Asked<'_> {
    fn account(&self) -> Result<(Account, Option<TokenEntry>), String> {
        let home = self
            .ctx
            .home
            .as_deref()
            .ok_or_else(|| "no `HOME`, so its settings cannot be read".to_owned())?;
        let cwd = self.ctx.cwd.to_string_lossy();
        let settings = super::upgrade_drive::tap_settings_under(home, self.argv, &cwd);
        let profile = super::upgrade_drive::tap_profile_store_at(home);
        let who = ProcessLogin::of_env(self.env.iter().copied())
            .with_settings(&settings)
            .with_profile_store(profile);
        let found = token_route::account_of(
            &who,
            self.ctx.roster.as_ref(),
            self.run.named(),
            &|login| super::profile::login_record(home, login).map(|record| record.email),
            &|| token_route::outside_why(self.env, &settings, profile),
        )?;
        self.entry.set(found.1);
        Ok(found)
    }

    fn claims(&self, account: &Account) -> Option<Fallback> {
        if self.run.named.is_some() {
            return None;
        }
        let label = account.label.as_deref()?;
        let row = self.ctx.roster.as_ref()?.row(label)?;
        if let Some(holder) = &row.held_by {
            return Some(Fallback::Held(format!("{label} is held by {holder}")));
        }
        let Some(store) = &self.ctx.store else {
            return Some(Fallback::Claimed("the accounts store is not known"));
        };
        match store.load(self.ctx.now) {
            Ok((state, _)) => state
                .lease_verdict(label, self.ctx.now)
                .err()
                .map(|why| Fallback::Claimed(why.as_str())),
            Err(_) => Some(Fallback::Claimed("the accounts state cannot be read")),
        }
    }

    #[cfg(unix)]
    fn token(&self, entry: TokenEntry, account: &Account) -> TokenFact {
        let (Some(vault), Some(store)) = (&self.ctx.vault, &self.ctx.store) else {
            return TokenFact::Absent;
        };
        let wait = if self.run.named.is_some() {
            LABELLED_WAIT
        } else {
            Duration::ZERO
        };
        let now = u64::try_from(self.ctx.now).unwrap_or(0);
        let (fact, read) = token_route::standing_fact(vault, store, entry, account, now, wait);
        self.found.set(fact != TokenFact::Absent);
        *self.read.borrow_mut() = read;
        fact
    }

    #[cfg(not(unix))]
    fn token(&self, _entry: TokenEntry, _account: &Account) -> TokenFact {
        TokenFact::Absent
    }
}

/// What the wrapper decided for `run` in `ctx`: the judgment, for [`Decision::Token`] the read
/// to serve from, and the build in atpkg's store `run.program` resolved to, the one file it
/// serves. A fallback where the account has a token entry is journaled (`token-fallback`). Off
/// macOS the judgment ends at `platform`, so it never decides `Token`.
#[must_use]
fn decide(run: &Run, ctx: &Ctx) -> (Decision, Option<Read>, Option<PathBuf>) {
    let (decision, read, build, noted) = decide_with(run, ctx);
    if let Some((entry, why)) = noted {
        note_fallback(entry, why, ctx.now);
    }
    (decision, read, build)
}

/// [`decide`]'s judgment, its read, the resolved build, and the fallback it journals.
type Decided = (
    Decision,
    Option<Read>,
    Option<PathBuf>,
    Option<(TokenEntry, &'static str)>,
);

/// [`decide`]'s judgment, and the fallback it would journal.
fn decide_with(run: &Run, ctx: &Ctx) -> Decided {
    let argv: Vec<String> = std::iter::once(token_route::PROGRAM.to_owned())
        .chain(
            run.args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        )
        .collect();
    // An inherited marker is set aside: the wrapper makes it afresh, and a tool child of a token
    // session inherits it with no descriptor.
    let env: Vec<(&str, &str)> = ctx
        .env
        .iter()
        .filter(|(name, _)| name != token_route::MARKER_ENV)
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let no_harness = env
        .iter()
        .any(|(name, value)| *name == "ATERM_NO_HARNESS" && !value.is_empty());
    // Resolved once: the file judged here is the file served ([`main_entry`]).
    let build = ctx
        .builds
        .as_deref()
        .and_then(|builds| token_route::store_build_in(builds, Path::new(&run.program)));
    let facts = Facts {
        macos: ctx.macos,
        no_harness,
        argv: &argv,
        store_build: build.is_some(),
    };
    let asked = Asked {
        run,
        ctx,
        argv: &argv,
        env: &env,
        entry: Cell::new(None),
        found: Cell::new(false),
        read: RefCell::new(None),
    };
    let decision = token_route::judge(&facts, &asked);
    let noted = match &decision {
        Decision::Stored(why @ (Fallback::Claimed(_) | Fallback::Held(_))) => {
            asked.entry.get().map(|entry| (entry, why.word()))
        }
        Decision::Stored(why) if asked.found.get() => {
            asked.entry.get().map(|entry| (entry, why.word()))
        }
        _ => None,
    };
    (decision, asked.read.into_inner(), build, noted)
}

/// Journal that a launch on `entry`'s account fell back (`token-fallback`), with the reason.
fn note_fallback(entry: TokenEntry, why: &'static str, now: i64) {
    let note = super::journal::token_note(
        super::journal::TokenEvent::Fallback,
        entry.key(),
        u64::try_from(now).unwrap_or(0),
        Some(why),
    );
    super::journal::note(&note);
}

/// `aterm harness accounts token-run …` (the words after `accounts token-run`): the module's
/// summary says what it does. It returns only where the program could not be started (127), or
/// on a platform with no exec, the program's own exit.
pub fn main_entry(args: &[OsString]) -> ExitCode {
    let run = match parse(args) {
        Ok(run) => run,
        Err(usage) => {
            eprintln!("aterm: {usage}");
            return ExitCode::from(2);
        }
    };
    let decided = decide(&run, &Ctx::from_process());
    #[cfg(unix)]
    {
        let error = match decided {
            (Decision::Token { entry, .. }, Some(read), Some(build)) => match read.token() {
                Some(token) => serve(build.as_os_str(), &run.args, token, entry),
                None => as_typed(&run.program, &run.args),
            },
            _ => as_typed(&run.program, &run.args),
        };
        eprintln!("aterm: {}: {error}", Path::new(&run.program).display());
        ExitCode::from(127)
    }
    #[cfg(not(unix))]
    {
        // Off Unix the judgment ends at `platform`: the program runs as typed.
        debug_assert!(!matches!(decided.0, Decision::Token { .. }));
        match aterm_uds::spawnfd::transparent(&run.program)
            .args(&run.args)
            .status()
        {
            Ok(status) => {
                ExitCode::from(u8::try_from(status.code().unwrap_or(1).clamp(0, 255)).unwrap_or(1))
            }
            Err(error) => {
                eprintln!("aterm: {}: {error}", Path::new(&run.program).display());
                ExitCode::from(127)
            }
        }
    }
}

/// Exec `program` exactly as typed: this process's environment, descriptors and stdio.
#[cfg(unix)]
fn as_typed(program: &std::ffi::OsStr, args: &[OsString]) -> std::io::Error {
    aterm_uds::spawnfd::transparent(program).args(args).exec()
}

/// Exec `program` on `token` (§5.3, serving): the credential variables and any inherited marker
/// unset, the marker naming `entry`, and the token behind an inherited socket numbered in the
/// descriptor variable. Returns only where the exec failed.
#[cfg(unix)]
fn serve(
    program: &std::ffi::OsStr,
    args: &[OsString],
    token: &aterm_secrets::SecretBytes,
    entry: TokenEntry,
) -> std::io::Error {
    use std::io::Write as _;
    let (mut ours, theirs) = match std::os::unix::net::UnixStream::pair() {
        Ok(pair) => pair,
        Err(error) => return error,
    };
    let mut line = zeroize::Zeroizing::new(Vec::with_capacity(token.len() + 1));
    line.extend_from_slice(token.as_slice());
    line.push(b'\n');
    let written = ours.write_all(&line);
    drop(line);
    drop(ours);
    if let Err(error) = written {
        return error;
    }
    let mut command = aterm_uds::spawnfd::command(program);
    command.args(args);
    for name in accounts::CREDENTIAL_KEYS
        .iter()
        .chain(&[token_route::MARKER_ENV])
    {
        command.env_remove(name);
    }
    #[cfg(not(any(test, debug_assertions)))]
    let guards = vec![std::os::fd::OwnedFd::from(theirs)];
    #[cfg(any(test, debug_assertions))]
    let guards = match with_spare(std::os::fd::OwnedFd::from(theirs), token, &mut command) {
        Ok(guards) => guards,
        Err(error) => return error,
    };
    if let Err(error) = command.inherit_guards(guards) {
        return error;
    }
    let Some(number) = command.guard_targets().next() else {
        return std::io::Error::other("the token's descriptor was not placed");
    };
    command.env(token_route::DESCRIPTOR_ENV, number.to_string());
    command.env(token_route::MARKER_ENV, entry.name());
    command.exec()
}

/// The lab's probe of what Claude Code's tool children inherit (S9 lane C,
/// `crates/aterm-lab/src/setup_token_lab.rs`): set to `1`, a debug build's wrapper also hands the
/// program a second socket holding the token, numbered in no variable, so Claude Code never reads
/// or closes it and any child that inherited a descriptor of the session's past stdio would find
/// it (2.1.318 hands its children stdio alone, so none does). A release build never names it.
#[cfg(all(unix, any(test, debug_assertions)))]
pub const SPARE_SEAM: &str = "ATERM_DEBUG_TOKEN_RUN_SPARE";

/// The guards a served program inherits: `descriptor` first (its number goes in
/// [`token_route::DESCRIPTOR_ENV`]), then, where [`SPARE_SEAM`] is `1`, a second socket holding
/// the token, which `command` no longer names the seam to.
#[cfg(all(unix, any(test, debug_assertions)))]
fn with_spare(
    descriptor: std::os::fd::OwnedFd,
    token: &aterm_secrets::SecretBytes,
    command: &mut aterm_uds::spawnfd::Command,
) -> std::io::Result<Vec<std::os::fd::OwnedFd>> {
    use std::io::Write as _;
    if !std::env::var_os(SPARE_SEAM).is_some_and(|value| value == "1") {
        return Ok(vec![descriptor]);
    }
    command.env_remove(SPARE_SEAM);
    let (mut ours, theirs) = std::os::unix::net::UnixStream::pair()?;
    let mut line = zeroize::Zeroizing::new(Vec::with_capacity(token.len() + 1));
    line.extend_from_slice(token.as_slice());
    line.push(b'\n');
    ours.write_all(&line)?;
    Ok(vec![descriptor, std::os::fd::OwnedFd::from(theirs)])
}

// The tests read a scratch seated vault, which is Unix's; off Unix the judgment ends at
// `platform`, which `token_route`'s tests hold.
#[cfg(all(test, unix))]
#[path = "accounts_token_run_tests.rs"]
mod tests;
