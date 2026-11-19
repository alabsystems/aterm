// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets`' verbs of a member that reads its vault, on every host the vault client builds
//! on: `status`, `join`, `sync`, `mirrors`, `ls` and `get`. Unix runs them among its other verbs
//! (`super::unix`); a Windows member runs these alone (`super::windows`). What differs by
//! host comes in as a [`StatusHost`] or a [`JoinHost`]: the lines of the services a host runs for
//! its vault, its terminal's prompts and the subscriber that follows the vault.

use crate::client::{
    self, JoinEvent, JoinOptions, JoinOutcome, JoinPassword, Membership, Paths, SyncOutcome,
};
use crate::transport::{Source, describe};
use crate::{Error, SecretBytes, envelope, join};
use std::cell::Cell;
use std::io::{IsTerminal, Read, Write};
use std::process::ExitCode;
use std::time::Duration;
use zeroize::Zeroize;

/// What `status` asks of the host it runs on, each read where the Unix `status` read it.
pub(super) struct StatusHost<'a> {
    /// Who writes this machine's vault: `vault writes:`, `admin key:` and any per-host copies.
    pub(super) writes: &'a dyn Fn(&Paths) -> Vec<String>,
    /// The release-signing line, where this host has one to say.
    pub(super) release: &'a dyn Fn(&Paths) -> Option<String>,
    /// The last lines: what this host runs for its vault (the node service, the subscriber and
    /// its approver, the server) and where it is on the network.
    pub(super) services: &'a dyn Fn(&Paths, &client::Status),
}

/// A host's prompt for one line typed at the terminal without echo.
pub(super) type AskHidden<'a> = &'a dyn Fn(&str) -> Option<SecretBytes>;

/// What `join` asks of the host it runs on: its terminal's prompts, and the subscriber that
/// follows the vault once a join is asked, where the host runs one.
pub(super) struct JoinHost<'a> {
    /// One line from the terminal itself, echo on (`None` with no terminal).
    pub(super) ask: &'a dyn Fn(&str) -> Option<String>,
    /// One line typed at the terminal without echo (`None` with no terminal). A host that has
    /// no hidden prompt (`None` here) asks for no join password: one comes from
    /// `--password-stdin` alone.
    pub(super) ask_hidden: Option<AskHidden<'a>>,
    /// Install the subscriber (`aterm secrets watch`): the line it says, or why not. A host that
    /// runs none (`None`) installs nothing.
    pub(super) subscribe: Option<&'a dyn Fn() -> crate::Result<String>>,
    /// Record `--no-agent` as the person's choice.
    pub(super) no_agent: &'a dyn Fn(),
}

/// `ls`: the names this machine's store holds, with descriptions and file pointers
/// (`describe`) or as metadata JSON (`json`), and its Claude Code tokens' rows; never a value.
pub(super) fn ls(paths: &Paths, describe: bool, json: bool) -> crate::Result<ExitCode> {
    let names = client::registered_names(paths)?;
    // The vault's Claude Code tokens, value-free: best effort, so a vault that cannot
    // be read just now still lists what the registry names.
    let tokens = match crate::claude_token::listed(paths) {
        Ok(crate::claude_token::Lookup::Found(standings)) => {
            super::inventory::token_rows(&standings, aterm_time::unix_secs().unwrap_or(0))
        }
        _ => Vec::new(),
    };
    print!(
        "{}",
        super::inventory::listing(&names, &tokens, describe, json)?
    );
    Ok(ExitCode::SUCCESS)
}

/// `status`: this machine's vault, join code, membership and secrets, each read in place, then
/// what `here` says of the host.
pub(super) fn status(paths: &Paths, here: &StatusHost<'_>) -> crate::Result<ExitCode> {
    let status = client::status(paths)?;
    match &status.config {
        None => {
            println!(
                "no vault configured: aterm secrets join [<url>|<path>] (neither: {})",
                crate::ALAB_DEFAULT_URL
            );
        }
        Some(config) => {
            println!("vault: {} at {}", config.name, source_text(&config.source));
            if !config.mirrors.is_empty() {
                let shown: Vec<String> = config.mirrors.iter().map(|url| describe(url)).collect();
                println!("mirrors: {}", shown.join(", "));
            }
            for note in client::mirror_notes(paths) {
                println!("mirror {note}");
            }
            match &config.signer {
                Some(signer) => println!(
                    "signer: {} (pinned), key {}",
                    envelope::fingerprint(signer),
                    crate::b64::encode_padded(signer)
                ),
                None => println!("signer: none (a plain text file)"),
            }
        }
    }
    if let Some((recipient, code)) = &status.machine {
        println!(
            "{}",
            machine_line(status.config.as_ref(), &recipient.to_bech32(), code)
        );
    }
    match status.membership {
        Membership::Unconfigured => {}
        Membership::Pending => {
            let approve = status
                .approve_command()
                .unwrap_or_else(|| "aterm secrets approve ?".to_owned());
            let hint = status
                .config
                .as_ref()
                .filter(|config| matches!(config.source, Source::Url(_)))
                .filter(|_| client::asked_with_password(paths))
                .and_then(|config| {
                    let named = (config.host != client::default_host_name())
                        .then_some(config.host.as_str());
                    retype(&config.source, named, false)
                });
            println!("{}", pending_membership(&approve, hint.as_deref()));
        }
        // A locker opened, but no seat of the log binds this Mac: it is no member, and its
        // subscriber asks for a seat (one-mesh S1).
        Membership::In { seq, heard } if client::seat_asks(paths) => {
            println!(
                "membership: {}, seq {seq}, last heard {}",
                client::Seat::SEATLESS,
                ago(heard)
            );
        }
        Membership::In { seq, heard } => {
            println!("membership: in, seq {seq}, last heard {}", ago(heard));
        }
        Membership::Revoked { seq } => {
            println!("membership: {}", client::out_words(paths, seq))
        }
        Membership::Plain => println!("membership: reading a plain text file"),
    }
    if !status.names.is_empty() {
        println!(
            "secrets: {} ({})",
            status.names.len(),
            status.names.join(", ")
        );
    }
    if status.config.is_some() {
        println!("{STORE_LINE}");
        for line in (here.writes)(paths) {
            println!("{line}");
        }
        // How full the membership log is, and which classes of write it still takes.
        match client::log_fill(paths) {
            Ok(Some(fill)) => println!("{}", fill.line()),
            Ok(None) => {}
            Err(error) => println!("log: unreadable ({error})"),
        }
        // What this Mac's last format-2 log pass did; no line where none kept a record.
        if let Some(line) = client::log_pass_line(paths, ago) {
            println!("{line}");
        }
    }
    match client::swarm_lease_holder(paths) {
        Ok(Some(holder)) => println!("vault steward: {holder}"),
        Ok(None) => println!("vault steward: none recorded"),
        Err(error) => println!("vault steward: unknown ({error})"),
    }
    if let Some(line) = (here.release)(paths) {
        println!("{line}");
    }
    match client::conflicts(paths) {
        Ok(conflicts) => {
            let (plain, tokens) = client::Conflict::split(&conflicts);
            if !plain.is_empty() {
                println!(
                    "conflict: {} (saved with different values while apart; every Mac reads the same one, and {})",
                    plain.join(", "),
                    client::CONFLICT_PICK
                );
            }
            if !tokens.is_empty() {
                println!(
                    "conflict: {} (saved with different values while apart; every Mac reads the same one, and {})",
                    tokens.join(", "),
                    client::TOKEN_CONFLICT_PICK
                );
            }
        }
        Err(error) => println!("conflict: unreadable ({error})"),
    }
    match client::reseal_waits(paths) {
        Ok(lines) => {
            for line in lines {
                println!("waiting: {line}");
            }
        }
        Err(error) => println!("waiting: unreadable ({error})"),
    }
    match client::read_after_cutoff_lines(paths) {
        Ok(lines) => {
            for line in lines {
                println!("read while out: {line}");
            }
        }
        Err(error) => println!("read while out: unreadable ({error})"),
    }
    match client::lab_set_aside(paths) {
        Ok(Some(set)) => println!("{}", set.status_line()),
        Ok(None) => {}
        Err(error) => println!("set aside: unreadable ({error})"),
    }
    match crate::need::peek(paths) {
        Ok(needs) => {
            if let Some(line) = crate::need::status_line(&needs) {
                println!("{line}");
            }
        }
        Err(error) => println!("needs: unreadable ({error})"),
    }
    match crate::need::full::standing(paths) {
        Ok(Some(full)) => println!("needs: {}", full.line()),
        Ok(None) => {}
        Err(error) => println!("needs: the lab table's last refusal is unreadable ({error})"),
    }
    (here.services)(paths, &status);
    Ok(ExitCode::SUCCESS)
}

/// `join [<url>|<path>] [--name <host>] [--password-stdin] [--no-agent] [--no-wait]` on a host
/// whose prompts and subscriber `here` gives.
pub(super) fn join(
    paths: &Paths,
    source: &str,
    name: Option<String>,
    agent: bool,
    wait: bool,
    password_stdin: bool,
    here: &JoinHost<'_>,
) -> crate::Result<ExitCode> {
    let interactive = interactive();
    let piped = if password_stdin {
        Some(piped_password("--password-stdin")?)
    } else {
        None
    };
    let confirm = |fingerprint: &str| {
        yes((here.ask)(&format!(
            "The vault is signed by {fingerprint}. Compare it with `aterm secrets status` on an admin Mac. Trust it? [y/N] "
        )))
    };
    let from_pipe = piped.is_some();
    let given = Cell::new(from_pipe);
    let ask_password = || -> Option<SecretBytes> {
        let password = here
            .ask_hidden
            .and_then(|ask_hidden| ask_hidden(
                "Join password (an admin Mac shows it: aterm secrets password; Enter joins without one and waits for Allow on any member, or for approval by code; at the ALab vault, a member on the home ALab LAN lets this Mac in by its proof): ",
            ))
            .filter(|typed| !typed.as_slice().trim_ascii().is_empty());
        given.set(password.is_some());
        password
    };
    let password = match piped {
        Some(piped) => JoinPassword::Given(Some(piped)),
        None if interactive && here.ask_hidden.is_some() => JoinPassword::Ask(&ask_password),
        None => JoinPassword::Given(None),
    };
    let installed = Cell::new(false);
    let install_failed = Cell::new(false);
    let install = || {
        if !agent || installed.replace(true) {
            return;
        }
        let Some(subscribe) = here.subscribe else {
            return;
        };
        match subscribe() {
            Ok(line) => eprintln!("{line}"),
            Err(error) => {
                install_failed.set(true);
                eprintln!(
                    "the subscriber was not installed ({error}); run aterm secrets agent on --program <aterm> to keep this machine in sync"
                );
            }
        }
    };
    let say = |event: &JoinEvent<'_>| eprintln!("{event}");
    let before = client::config(paths).ok().flatten();
    let outcome = client::join(
        paths,
        &JoinOptions {
            source: source.to_string(),
            host: name.clone(),
            interactive,
            wait: join_wait(wait, interactive),
            confirm: &confirm,
            password,
            fetched: None,
            requested: &install,
            say: &say,
        },
    )
    .inspect_err(|_| {
        // A join that fails after saving the source (the server gone before the request
        // was queued) leaves this Mac waiting with its request due at once, so the
        // subscriber that asks goes in now, as once a request is queued.
        if client::config(paths)
            .ok()
            .flatten()
            .is_some_and(|after| before.as_ref() != Some(&after))
        {
            install();
        }
    })?;
    if !agent {
        (here.no_agent)();
    }
    match &outcome {
        JoinOutcome::Joined { seq, names } => {
            println!("joined: seq {seq}, {} secrets read in place", names.len());
        }
        JoinOutcome::Pending { code, host } => {
            let hint = if given.get() {
                client::config(paths)
                    .ok()
                    .flatten()
                    .and_then(|config| retype(&config.source, name.as_deref(), from_pipe))
            } else {
                None
            };
            let approve = client::status(paths)
                .ok()
                .and_then(|status| status.approve_command())
                .unwrap_or_else(|| format!("aterm secrets approve {code}"));
            println!("{}", waiting_line(host, code, &approve, hint.as_deref()));
        }
        JoinOutcome::Plain { names } => {
            println!(
                "reading a plain text file: {} secrets read in place",
                names.len()
            );
        }
    }
    install();
    Ok(if install_failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

pub(super) fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// How long `join` waits for the approval: ten minutes at a terminal, unless `--no-wait`.
/// Without one (a script, an agent's tool) it returns at once with the approve command, and
/// the subscriber finishes the join.
pub(super) fn join_wait(wait: bool, interactive: bool) -> Duration {
    if wait && interactive {
        Duration::from_secs(600)
    } else {
        Duration::ZERO
    }
}

pub(super) fn yes(answer: Option<String>) -> bool {
    answer.is_some_and(|a| a.eq_ignore_ascii_case("y") || a.eq_ignore_ascii_case("yes"))
}

/// A password piped in for `--password-stdin` or `password set`: one line, blanks
/// around it dropped ([`read_piped_password`]).
pub(super) fn piped_password(what: &str) -> crate::Result<SecretBytes> {
    if std::io::stdin().is_terminal() {
        return Err(Error::new(format!("{what} reads the password from a pipe")));
    }
    read_piped_password(what, &mut std::io::stdin().lock())
}

/// The most a piped password's input may hold, blanks around the password included: past it
/// the input is refused whole, never cut to a password the person did not type.
const PIPED_PASSWORD_INPUT_MAX: usize = 4096;

/// [`piped_password`]'s reading of `input`: one line of at most [`join::MAX_LEN`] bytes, blanks
/// around it dropped. A longer password, an input past [`PIPED_PASSWORD_INPUT_MAX`], or one
/// that breaks its line with a CR or LF is refused.
pub(super) fn read_piped_password(what: &str, input: &mut dyn Read) -> crate::Result<SecretBytes> {
    let too_long = || {
        Error::new(format!(
            "{what}: a join password is at most {} bytes",
            join::MAX_LEN
        ))
    };
    let mut bytes = Vec::with_capacity(PIPED_PASSWORD_INPUT_MAX + 1);
    crate::secret::read_to_end_wiping_on_error(
        input.take(PIPED_PASSWORD_INPUT_MAX as u64 + 1),
        &mut bytes,
    )?;
    if bytes.len() > PIPED_PASSWORD_INPUT_MAX {
        bytes.zeroize();
        return Err(too_long());
    }
    let trimmed = SecretBytes::from(bytes.trim_ascii());
    bytes.zeroize();
    if trimmed.as_slice().is_empty() {
        return Err(Error::new(format!("{what}: nothing was piped in")));
    }
    if trimmed.as_slice().contains(&b'\n') || trimmed.as_slice().contains(&b'\r') {
        return Err(Error::new(format!("{what}: the password is one line")));
    }
    if trimmed.as_slice().len() > join::MAX_LEN {
        return Err(too_long());
    }
    Ok(trimmed)
}

pub(super) fn ago(then: u64) -> String {
    let now = aterm_time::unix_secs().unwrap_or(then);
    let secs = now.saturating_sub(then);
    match secs {
        0..=119 => format!("{secs} s ago"),
        120..=7199 => format!("{} min ago", secs / 60),
        7200..=172_799 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

pub(super) fn source_text(source: &Source) -> String {
    match source {
        Source::Url(url) => crate::transport::describe(url),
        Source::Path(path) => path.display().to_string(),
    }
}

/// How a Mac waiting to join the vault at `source` fixes a mistyped join password.
pub(super) fn retype(source: &Source, name: Option<&str>, piped: bool) -> Option<String> {
    let Source::Url(url) = source else {
        return None;
    };
    let mut join = String::from("aterm secrets join");
    if !crate::is_alab_default_url(url) {
        join.push(' ');
        join.push_str("<vault-url>");
    }
    if let Some(name) = name {
        join.push_str(" --name ");
        join.push_str(name);
    }
    let source_note = if crate::is_alab_default_url(url) {
        ""
    } else {
        "; replace <vault-url> with this vault's full configured URL, including any query or signer pin"
    };
    Some(if piped {
        format!(
            "a mistyped join password is fixed by piping it again to {join} --password-stdin, or by running {join} in a terminal, which asks for it{source_note}"
        )
    } else {
        format!(
            "a mistyped join password is fixed by running {join} again (it asks for the password again){source_note}"
        )
    })
}

/// `join`'s status line for a Mac awaiting approval of `code`, with the [`retype`] hint if any.
pub(super) fn waiting_line(host: &str, code: &str, approve: &str, retype: Option<&str>) -> String {
    let line =
        format!("waiting for approval of {host} (code {code}): on an admin Mac run {approve}");
    match retype {
        Some(retype) => format!("{line}; {retype}"),
        None => line,
    }
}

/// `status`'s membership line for a Mac waiting for an admin to run `approve`
/// ([`Status::approve_command`](client::Status::approve_command)), with [`retype`] for a
/// vault with a join queue.
pub(super) fn pending_membership(approve: &str, retype: Option<&str>) -> String {
    let line = format!("membership: waiting for approval (on an admin Mac: {approve})");
    match retype {
        Some(retype) => format!("{line}; {retype}"),
        None => line,
    }
}

/// `status`'s line for this machine, `key` its `age1…` key and `code` its join code.
pub(super) fn machine_line(config: Option<&client::Config>, key: &str, code: &str) -> String {
    let line = format!("machine: code {code}, key {key}");
    match config {
        Some(config) if config.signer.is_some() => {
            format!("{line}, asked to join as {}", config.host)
        }
        Some(config) => format!("machine: {}, code {code}, key {key}", config.host),
        None => line,
    }
}

pub(super) fn sync_diagnostic(paths: &Paths) -> crate::sync_diagnostic::SyncDiagnostic {
    use crate::sync_diagnostic::SyncDiagnostic;
    match sync_outcome(paths) {
        // An apply writes no file, so none fails.
        Ok((SyncOutcome::Applied { seq, .. }, _)) => SyncDiagnostic::Applied { seq, failed: 0 },
        Ok((SyncOutcome::Unchanged { seq }, _)) => SyncDiagnostic::Unchanged(seq),
        Ok((SyncOutcome::PutBack { seq, .. }, _)) => SyncDiagnostic::PutBack(seq),
        Ok((SyncOutcome::NotIn, _)) => SyncDiagnostic::NotIn,
        Err(error) => SyncDiagnostic::Failed(error.kind()),
    }
}

fn sync_outcome(paths: &Paths) -> crate::Result<(SyncOutcome, Option<Option<u64>>)> {
    let mut outcome = client::sync(paths, None)?;
    // Restore local files after putting the held version back on its source.
    let mut put_back = None;
    if let SyncOutcome::PutBack { served, .. } = outcome
        && !client::materialization_ready(paths)?
    {
        put_back = Some(served);
        outcome = client::sync(paths, None)?;
    }
    Ok((outcome, put_back))
}

/// `aterm secrets mirrors`: list this Mac's other addresses of its vault, after adding or
/// removing (`change`) the `typed` ones. An added mirror is read once first: one serving a
/// version this Mac's pin does not verify is refused, and nothing is saved.
pub(super) fn mirrors(
    paths: &Paths,
    change: Option<bool>,
    typed: &[String],
) -> crate::Result<ExitCode> {
    let config = client::config(paths)?.ok_or_else(|| {
        Error::new("no vault configured: run aterm secrets join <url> (or aterm init)")
    })?;
    let config = match change {
        None => config,
        Some(add) => {
            let mut list = config.mirrors.clone();
            let mut heard = Vec::new();
            for typed in typed {
                let at = |list: &[String], url: &str| {
                    list.iter()
                        .position(|kept| crate::discover::same_endpoint(kept, url))
                };
                if add {
                    let url = client::mirror_typed(&config, typed)?;
                    if at(&list, &url).is_some() {
                        heard.push(format!("{}: listed already", describe(&url)));
                        continue;
                    }
                    let said = match client::probe_mirror(&config, &url)? {
                        client::MirrorProbe::Serves(seq) => format!("serves seq {seq}"),
                        client::MirrorProbe::Empty => {
                            "holds no version of the vault yet; it is read once its server does"
                                .to_owned()
                        }
                        client::MirrorProbe::Unreached(why) => {
                            format!("did not answer ({why})")
                        }
                    };
                    heard.push(format!("{}: {said}", describe(&url)));
                    list.push(url);
                } else {
                    let base = typed.split_once('#').map_or(typed.as_str(), |(b, _)| b);
                    let Some(found) = at(&list, base.trim()) else {
                        return Err(Error::new(format!(
                            "{}: not a mirror of this vault",
                            describe(typed.trim())
                        )));
                    };
                    list.remove(found);
                }
            }
            let saved = client::set_mirrors(paths, &config, &list)?;
            for line in heard {
                println!("{line}");
            }
            saved
        }
    };
    match &config.source {
        Source::Url(url) => println!(
            "url: {} (the source: joins and admin publishes start here)",
            describe(url)
        ),
        Source::Path(path) => println!("path: {} (a path has no mirrors)", path.display()),
    }
    for url in &config.mirrors {
        println!("mirror: {} (read, never written to)", describe(url));
    }
    if config.mirrors.is_empty() && matches!(config.source, Source::Url(_)) {
        println!("no mirrors: aterm secrets mirrors add <url> lists one");
    }
    for note in client::mirror_notes(paths) {
        println!("mirror {note}");
    }
    Ok(ExitCode::SUCCESS)
}

pub(super) fn sync(paths: &Paths, diagnostic: bool) -> crate::Result<ExitCode> {
    if diagnostic {
        let outcome = sync_diagnostic(paths);
        println!("{}", outcome.line());
        return Ok(if outcome.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let held = |served: Option<u64>| {
        served.map_or("no vault".to_string(), |served| {
            format!("seq {served}, older than this Mac's")
        })
    };
    let (outcome, put_back) = sync_outcome(paths)?;
    // A put-back writes no file here: files still unsettled are applied from the copy the
    // source holds now. The first line says what the sync did, as before (a caller reads it).
    let first_put_back = || {
        if let Some(served) = put_back {
            println!(
                "  the source held {}: put this Mac's copy back first",
                held(served)
            );
        }
    };
    match outcome {
        SyncOutcome::Applied { seq, report, .. } => {
            // The harness reads this line's seq and its failed count (`Vault::of_sync`). An
            // apply writes no file, so none fails.
            println!(
                "applied seq {seq}: {} read in place, 0 failed",
                report.held.len()
            );
            first_put_back();
            for (name, why) in &report.refused {
                println!("  {name}: not read: {why}");
            }
            Ok(ExitCode::SUCCESS)
        }
        SyncOutcome::Unchanged { seq } => {
            println!("up to date at seq {seq}");
            first_put_back();
            Ok(ExitCode::SUCCESS)
        }
        SyncOutcome::PutBack { seq, served } => {
            println!(
                "the source held {}: put this Mac's copy back; up to date at seq {seq}",
                held(served)
            );
            Ok(ExitCode::SUCCESS)
        }
        SyncOutcome::NotIn => {
            println!("this machine is not in the vault (yet): aterm secrets status");
            Ok(ExitCode::FAILURE)
        }
    }
}

pub(super) fn get(paths: &Paths, name: &str) -> crate::Result<ExitCode> {
    let value = get_value(paths, name)?;
    let mut out = std::io::stdout().lock();
    out.write_all(value.as_slice())?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

pub(super) fn get_value(paths: &Paths, name: &str) -> crate::Result<crate::SecretBytes> {
    if crate::entry_class::claude_token(&name.to_ascii_lowercase()) {
        return Err(Error::new(format!("{name}: {CLAUDE_TOKEN_GET}")));
    }
    let expected = client::config(paths)?
        .ok_or_else(|| Error::new("no vault configured; run aterm secrets join"))?;
    if expected.signer.is_none() {
        let status = client::status(paths)?;
        if status.config.as_ref() != Some(&expected)
            || !matches!(status.membership, client::Membership::Plain)
        {
            return Err(Error::new(
                "no applied plaintext source; run aterm secrets sync",
            ));
        }
    }
    client::read_in_place(paths, name)?.ok_or_else(|| {
        Error::new(format!(
            "{name} is not among this machine's verified secrets (aterm secrets ls); run aterm secrets sync"
        ))
    })
}

/// What `secrets status` says of where this Mac's values are, once a vault is configured: the
/// verified store is the only copy, and no apply writes a file
/// (`docs/DESIGN-secrets-2026-10-20.md` §4).
pub(super) const STORE_LINE: &str = "store: this Mac reads the store in place and writes no file";

/// Why `secrets get` refuses a Claude Code token entry (DESIGN-claude-setup-token §4.2).
pub(super) const CLAUDE_TOKEN_GET: &str = "a Claude Code token, which aterm hands only to Claude Code and never prints; aterm secrets ls --describe shows its state";
