// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric`: fabric status, live traffic (`tail`), session-log brief (`brief`), lifecycle (`on`, `off`, `doctor` in [`crate::enable`]), and person commands (`mint-for`, `halt`, `inbox`, `post` in [`crate::person`]).

use aterm_uds::spawnfd::Stdio;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::bridge::{read_cap_file, Config};
use crate::ctl::Ctl;
use crate::render::{safe, trust_of, SCREEN};
use crate::transport::{self, Conn};

/// Usage, printed for `help` (exit 0) and after a usage error (exit 2).
pub const USAGE: &str = "\
usage: aterm fabric [status] [--json]
       aterm fabric tail [--bodies] [--from <offset>] [--filter <subject-filter>]
       aterm fabric brief [@<sid>[@<node>]] [--window <records>] [--budget <tokens>]
                          [--cap-file <path>] [--json]
       aterm fabric on  [--dry-run] [--fleet <F>] [--service launchd|systemd|none]
                        [--durability strict|ordered|relaxed]
       aterm fabric off [--dry-run] [--service launchd|systemd|none]
       aterm fabric doctor [--retire-ghosts [--yes]]
       aterm fabric mint-for h-<name>|a-<name> [--out <cap>] [--fleet <F>]
                             [--expires <N>d|<N>h]
       aterm fabric halt  --cap-file <person-cap> <reason>|off
       aterm fabric inbox --cap-file <person-cap> [--peek]
       aterm fabric post  --cap-file <person-cap> to=@<sid>@<node> kind=<k> [re=<off>]
                          [--] <text>
       aterm fabric help

Inspect and manage the local aterm fabric. `status` reads the fleet, broker, cap
file and state dir from `[fabric] command` in aterm.toml, falling back to the
rendezvous file (`fabric.toml`) written by `aterm fabric on`.

  status           default view with seven sections:
                   - CONFIG: command source, fleet, node id, cap grants, state dir
                   - BROKER: live connect/attach/head probe, capability guard check,
                     PID/service job, `subjects` spent of 4096, `acks` durability,
                     `health` (poison status and flush counts), and `holds`
                     (`<log>.health` snapshot)
                   - NODES: bus presence rows (`this`, `local`, or `remote` for an
                     unhosted/ghost node on this machine; retire via `aterm fabric
                     doctor --retire-ghosts`)
                   - BRIDGES: local aterm instances and their bridge processes
                   - SESSIONS: per-session bus state (`live`, `away`, `gone`), hold,
                     inbox counts, topics, role, detail, phase, context and title
                     (`rows=4096+ truncated` when cut at 4096 rows)
                   - TRAFFIC: last 10 bus records (metadata only)
                   - WARNINGS: issues that block mail or make `connected` misleading
  --json           status and brief: emit one JSON object (`brief --json` emits one
                   object per session with node, sid, this, ep, need, detail, open,
                   mail_in, mail_out, last_seq, age_s, partial)
  tail             stream bus records live until Ctrl-C
  --from <offset>  tail only: replay from bus offset `<offset>` first
  --bodies         tail only: print each record's text after its trust label
  --filter <f>     tail only: subscribe to one subject pattern (`/f/<F>/…`, `*` for
                   one segment, `>` for the rest) instead of the whole fleet
  on               idempotently provision the fabric root (`~/.local/share/aterm-fabric`,
                   0700), node id, mint secret and 8 node grants; install and start
                   the broker under launchd (macOS) or systemd --user (Linux); write
                   `[fabric] command` to aterm.toml (keeping `.bak`) and `fabric.toml`
                   (0600); attach running aterm instances; verify end-to-end delivery
                   with a self-test note; and print `status`
  --dry-run        on/off only: print planned steps without modifying anything
  --fleet <F>      on and mint-for: fleet name (`/f/<F>/`; default `local`)
  --service <s>    on/off only: `launchd`, `systemd` or `none` (`none` uses an
                   already-running externally managed broker)
  --durability strict|ordered|relaxed
                   on only: broker ack durability tier (default `ordered`).
                   - `ordered`: acks after a disk ordering barrier and flushes in
                     full within 100 ms (served `strict` where the disk cannot
                     guarantee ordering).
                   - `strict`: acks after a full drive-cache flush; loses nothing
                     acked on power loss.
                   - `relaxed`: acks after the page-cache write; survives broker
                     restarts, not power loss or kernel panic.
                   Halts, a person's words, mail to a person and read marks always
                   flush in full before ack. Clean stops flush and close the log; an
                   unclean reboot advances the log generation at the agreement offset
                   so bridges and notifiers resume without skipping post-restart mail
  off              stop and remove the broker job, `[fabric]` in aterm.toml (keeping
                   `.bak`), and `fabric.toml`, while preserving the node id, secret,
                   cap and bus log
  doctor           print `status` warnings with their remedies (`--retire-ghosts`
                   lists unhosted live presence rows on this node and, after y/N or
                   `--yes`, publishes `exited` for each; dry by default)
  mint-for <id>    mint a capability ring for human `h-<name>` or agent `a-<name>`
                   under this machine's mint secret (human rings include `halt`)
  --out <cap>      mint-for only: write cap to `<cap>` (0600, refusing symlinks or
                   different existing caps; default prints cap to stdout and guidance
                   to stderr)
  --expires <N>d|<N>h
                   mint-for only: stamp grants with `exp=` so the broker refuses the
                   ring after `<N>d` or `<N>h`
  halt <reason>    as human `--cap-file`: hold every session on the fleet (`hold=1
                   origin=fleet`); `halt off` withdraws this human's halt
  inbox            as `--cap-file`: print unread mail to `<person>` and commit the
                   consumer group cursor (`--peek` leaves mail unread)
  post             as `--cap-file`: send `to=@<sid>@<node>` with `kind=ask|answer|
                   task|report|note` (default `note`) and optional `re=<off>`
  --cap-file <c>   halt/inbox/post/brief: capability file (`mint-for h-<name> --out`)

Message bodies are never printed without `tail --bodies`. `status`, `tail` and
`doctor` write nothing to disk. Exit status: 0 healthy, 1 warning (or failed proof),
2 fabric off, unreadable config, or usage error.
";

const FABRIC_COMMAND_ENV: &str = "ATERM_FABRIC_COMMAND";

/// The [`FABRIC_COMMAND_ENV`] seam's value: `None` in a shipped build, which compiles the read out.
pub(crate) fn command_seam() -> Option<String> {
    aterm_types::dev_seam!(FABRIC_COMMAND_ENV).map(|v| v.to_string_lossy().into_owned())
}

/// How many bus records TRAFFIC shows.
pub const TRAFFIC_ROWS: usize = 10;

const TRAFFIC_SCAN_MAX: u64 = 1 << 20;

/// An unhandled `task` or `ask` older than this is a warning: somebody was given work and nobody has taken it.
pub const STALE_WORK_MS: u64 = 10 * 60 * 1000;

/// The bound on every socket this report and the operator's one-shot broker commands open: the dial, and each read and write ([`transport::connect`]).
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(3);

const WORK_KINDS: [&str; 2] = ["task", "ask"];

/// What argv asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// `aterm fabric help`.
    Help,
    /// `aterm fabric [status] [--json]`.
    Status {
        /// `--json`.
        json: bool,
    },
    /// `aterm fabric tail [--bodies] [--from <offset>] [--filter <subject>]`.
    Tail {
        /// `--bodies`.
        bodies: bool,
        /// `--from <offset>`; `None` is "new records only".
        from: Option<u64>,
        /// `--filter <subject-filter>`; `None` is the whole fleet.
        filter: Option<String>,
    },
    /// `aterm fabric brief …` ([`crate::fabric_brief`]).
    Brief(crate::fabric_brief::BriefOpts),
    /// `aterm fabric on …` ([`crate::enable::on`]).
    On(crate::enable::OnOpts),
    /// `aterm fabric off …` ([`crate::enable::off`]).
    Off(crate::enable::OffOpts),
    /// `aterm fabric doctor [--retire-ghosts [--yes]]` ([`crate::enable::doctor`]).
    Doctor {
        /// `--retire-ghosts`: publish `exited` for every presence row this node advertises live that no local instance hosts.
        retire_ghosts: bool,
        /// `--yes`: skip the y/N confirmation (requires `--retire-ghosts`).
        yes: bool,
    },
    /// `aterm fabric mint-for h-<name>|a-<name> …` ([`crate::person::mint_for`]).
    MintFor(crate::person::MintForOpts),
    /// `aterm fabric halt|inbox|post --cap-file <cap> …` ([`crate::person`]): the verb, and what it was told.
    Person(String, crate::person::PersonCmd),
}

/// Parse argv (everything after `fabric`).
pub fn parse_args(args: &[String]) -> Result<Cmd, String> {
    let (verb, rest): (&str, &[String]) = match args.first().map(String::as_str) {
        None => ("status", &[]),
        Some(v @ ("status" | "tail" | "on" | "off" | "doctor" | "mint-for")) => (v, &args[1..]),
        Some(v @ ("halt" | "inbox" | "post")) => {
            return Ok(match crate::person::parse(v, &args[1..])? {
                Some(cmd) => Cmd::Person(v.to_string(), cmd),
                None => Cmd::Help,
            });
        }
        Some("help" | "-h" | "--help") => return Ok(Cmd::Help),
        Some("brief") => return crate::fabric_brief::BriefOpts::parse(&args[1..]).map(Cmd::Brief),
        Some(flag) if flag.starts_with('-') => ("status", args),
        Some(other) => {
            return Err(format!(
                "unknown subcommand `{}` (status, tail, brief, on, off, doctor, mint-for, halt, \
                 inbox, post or help)",
                safe(other, 64)
            ));
        }
    };
    let mut json = false;
    let mut bodies = false;
    let mut retire_ghosts = false;
    let mut yes = false;
    let mut from = None;
    let mut filter: Option<String> = None;
    let mut on = crate::enable::OnOpts::default();
    let mut off = crate::enable::OffOpts::default();
    let mut mint = crate::person::MintForOpts::default();
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match (verb, flag.as_str()) {
            (_, "-h" | "--help") => return Ok(Cmd::Help),
            ("status", "--json") => json = true,
            ("tail", "--bodies") => bodies = true,
            ("tail", "--filter") => {
                let f = value()?;
                if !is_subject_filter(&f) {
                    return Err(format!(
                        "--filter {} is not a subject filter (`/f/<F>/…`, with `*` for one \
                         segment and `>` for the rest)",
                        safe(&f, 128)
                    ));
                }
                filter = Some(f);
            }
            ("tail", "--from") => {
                let v = value()?;
                let n = v.strip_prefix('@').unwrap_or(&v);
                from = Some(
                    n.parse::<u64>()
                        .map_err(|_| format!("--from {} is not a bus offset", safe(&v, 64)))?,
                );
            }
            ("mint-for", "--out") => mint.out = Some(value()?),
            ("mint-for", "--expires") => {
                mint.expires = Some(crate::person::parse_lifetime(&value()?)?);
            }
            ("mint-for", "--fleet") => {
                let f = value()?;
                if !crate::subject::is_fleet(&f) {
                    return Err(format!(
                        "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
                        safe(&f, 64)
                    ));
                }
                mint.fleet = Some(f);
            }
            ("mint-for", word) if !word.starts_with('-') => {
                if !mint.person.is_empty() {
                    return Err(format!(
                        "mint-for takes ONE person; `{}` is a second",
                        safe(word, 64)
                    ));
                }
                mint.person = word.to_string();
            }
            ("doctor", "--retire-ghosts") => retire_ghosts = true,
            ("doctor", "--yes") => yes = true,
            ("on", "--dry-run") => on.dry_run = true,
            ("off", "--dry-run") => off.dry_run = true,
            ("on", "--service") => on.service = Some(crate::enable::Service::parse(&value()?)?),
            ("off", "--service") => off.service = Some(crate::enable::Service::parse(&value()?)?),
            ("on", "--fleet") => {
                let f = value()?;
                if !crate::subject::is_fleet(&f) {
                    return Err(format!(
                        "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
                        safe(&f, 64)
                    ));
                }
                on.fleet = Some(f);
            }
            ("on", "--durability") => {
                on.durability = Some(crate::durability::Durability::parse(&value()?)?);
            }
            ("status", f @ ("--bodies" | "--from" | "--filter")) => {
                return Err(format!("{f} is a `tail` flag (`aterm fabric tail {f}`)"));
            }
            (_, "--filter") => {
                return Err(
                    "--filter is a `tail` flag (`aterm fabric tail --filter /f/<F>/pub/>`)"
                        .to_string(),
                );
            }
            ("tail", "--json") => {
                return Err("--json is a `status` flag (`aterm fabric --json`)".to_string());
            }
            (_, f @ ("--dry-run" | "--service")) => {
                return Err(format!("{f} is an `on`/`off` flag (`aterm fabric on {f}`)"));
            }
            (_, "--fleet") => {
                return Err(
                    "--fleet is an `on` or `mint-for` flag (`aterm fabric on --fleet <F>`)"
                        .to_string(),
                );
            }
            (_, "--durability") => {
                return Err(
                    "--durability is an `on` flag (`aterm fabric on --durability relaxed`)"
                        .to_string(),
                );
            }
            (_, "--out" | "--expires") => {
                return Err(format!(
                    "{flag} is a `mint-for` flag (`aterm fabric mint-for h-<name> {flag} …`)"
                ));
            }
            (_, other) => return Err(format!("unknown flag {}", safe(other, 64))),
        }
    }
    if verb == "mint-for" && !crate::person::is_person_id(&mint.person) {
        return Err(format!(
            "mint-for needs a person: h-<name> for a human, a-<name> for an agent with a cap of \
             its own{}",
            if mint.person.is_empty() {
                String::new()
            } else {
                format!(" (not `{}`)", safe(&mint.person, 64))
            }
        ));
    }
    Ok(match verb {
        "tail" => Cmd::Tail {
            bodies,
            from,
            filter,
        },
        "on" => Cmd::On(on),
        "off" => Cmd::Off(off),
        "mint-for" => Cmd::MintFor(mint),
        "doctor" => {
            // Accepted alone, `--yes` would let an operator believe a retirement had run.
            if yes && !retire_ghosts {
                return Err("--yes means nothing without --retire-ghosts".to_string());
            }
            Cmd::Doctor { retire_ghosts, yes }
        }
        _ => Cmd::Status { json },
    })
}

/// `aterm fabric …`, the entry for both `aterm fabric` and `aterm-link fabric`.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    match parse_args(args) {
        Ok(Cmd::Help) => aterm_uds::stdout::answer_exit("aterm fabric", USAGE, 0),
        Ok(Cmd::Status { json }) => status_main(json),
        Ok(Cmd::Tail {
            bodies,
            from,
            filter,
        }) => tail_main(bodies, from, filter.as_deref()),
        Ok(Cmd::Brief(opts)) => crate::fabric_brief::brief_main(&opts),
        Ok(Cmd::On(opts)) => crate::enable::on(&opts),
        Ok(Cmd::Off(opts)) => crate::enable::off(&opts),
        Ok(Cmd::Doctor { retire_ghosts, yes }) => crate::enable::doctor(retire_ghosts, yes),
        Ok(Cmd::MintFor(opts)) => crate::person::mint_for(&opts),
        Ok(Cmd::Person(verb, cmd)) => match configured() {
            Ok(cfg) => crate::person::main(&verb, &cmd, &cfg),
            Err(code) => code,
        },
        Err(e) => {
            eprintln!("aterm fabric: {e}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Where the bridge command was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `$ATERM_FABRIC_COMMAND` (a development build's seam).
    Env,
    /// `[fabric] command` in this file.
    File(PathBuf),
    /// `command` in the rendezvous file at this path (`aterm fabric on` wrote it; aterm.toml has no command).
    Rendezvous(PathBuf),
    /// None of those: the command a running instance (this pid) was armed with.
    Instance(u32),
}

impl Source {
    fn describe(&self) -> String {
        match self {
            Source::Env => format!(
                "${FABRIC_COMMAND_ENV} (a development build's seam; it overrides aterm.toml)"
            ),
            Source::File(p) => format!("[fabric] command in {}", p.display()),
            Source::Rendezvous(p) => format!(
                "the rendezvous file {} (no [fabric] command in aterm.toml)",
                p.display()
            ),
            Source::Instance(pid) => {
                format!("instance {pid}'s running bridge (no [fabric] command anywhere)")
            }
        }
    }

    fn token(&self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::File(_) => "config",
            Source::Rendezvous(_) => "rendezvous",
            Source::Instance(_) => "instance",
        }
    }
}

/// The aterm config file: `$XDG_CONFIG_HOME/aterm/aterm.toml`, else `$HOME/.config/aterm/aterm.toml` — the path `aterm-gui`'s `app_config::config_path` resolves on every Unix.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    // `aterm_types::dirs::config_home`: `$XDG_CONFIG_HOME`, or a test binary's own root.
    if let Some(x) = aterm_types::dirs::config_home() {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// `[fabric] command` out of an aterm.toml's TEXT, through the parser the app loads the file with, as the ARGV it names — from either spelling ([`aterm_types::fabric_command`]): a string split on whitespace, or an array of strings taken word for word.
pub fn command_in_toml(text: &str) -> Result<Option<Vec<String>>, String> {
    fabric_key(text, "command", command_of)
}

/// [`command_in_toml`]'s reading of one `command` value — aterm.toml's or the rendezvous file's.
pub(crate) fn command_of(v: &aterm_toml::Value) -> Result<Option<Vec<String>>, String> {
    use aterm_types::fabric_command::{check_word, split_line, EXPECTED};
    let argv = match v {
        aterm_toml::Value::String(line) => split_line(line),
        aterm_toml::Value::Array(elements) => {
            let mut argv = Vec::with_capacity(elements.len());
            for (i, element) in elements.iter().enumerate() {
                let Some(word) = element.as_str() else {
                    return Err(format!(
                        "`[fabric] command` element {} is not a string (it is a TOML {})",
                        i.saturating_add(1),
                        element.type_str()
                    ));
                };
                check_word(i, word)?;
                argv.push(word.to_string());
            }
            argv
        }
        other => {
            return Err(format!(
                "`[fabric] command` is a TOML {}; it must be {EXPECTED}",
                other.type_str()
            ));
        }
    };
    Ok((!argv.is_empty()).then_some(argv))
}

/// How a bridge argv is SHOWN to a person: its words joined by spaces, a word a shell would not read as one word single-quoted (`'\''` for a quote in it), so the line reads like the command it is and a path with a space in it is visibly one word.
#[must_use]
pub fn shown_argv<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|w| shell_word(w.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One word of [`shown_argv`]: as it is when every character is a letter, a digit or one of `/-_.,:=+@%` (no shell reads any of those), else single-quoted, with a quote in it spelled `'\''`.
#[must_use]
pub fn shell_word(w: &str) -> String {
    let plain = !w.is_empty()
        && w.chars()
            .all(|c| c.is_alphanumeric() || "/-_.,:=+@%".contains(c));
    if plain {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', "'\\''"))
    }
}

/// The argv a `fabric status` header names.
#[must_use]
pub fn status_argv(header: &str) -> Option<Vec<String>> {
    let argv = match kv(header, "argv") {
        Some("-") => return None,
        Some(token) => aterm_codec::pct::split_words(&crate::pct::decode(token)),
        None => match kv(header, "command")? {
            "-" => return None,
            token => aterm_types::fabric_command::split_line(&crate::pct::decode(token)),
        },
    };
    (!argv.is_empty()).then_some(argv)
}

fn fabric_key<T>(
    text: &str,
    key: &str,
    value: impl FnOnce(&aterm_toml::Value) -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
    let table: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let Some(fabric) = table.get("fabric") else {
        return Ok(None);
    };
    let Some(fabric) = fabric.as_table() else {
        return Err("`fabric` is not a table".to_string());
    };
    fabric.get(key).map_or(Ok(None), value)
}

/// `[fabric] presence` out of an aterm.toml's text: `Ok(None)` when the table or the key is absent (the bridge's default, `meta`, applies).
pub fn presence_in_toml(text: &str) -> Result<Option<crate::presence::Mode>, String> {
    fabric_key(text, "presence", |v| match v.as_str() {
        Some(s) => crate::presence::Mode::parse(s).map(Some).ok_or_else(|| {
            format!("`[fabric] presence = {s:?}` is neither \"meta\" nor \"minimal\"")
        }),
        None => Err("`[fabric] presence` is not a string".to_string()),
    })
}

/// `[fabric] presence` from the aterm.toml [`config_path`] resolves, for a `serve` whose command line did not say.
#[must_use]
pub fn presence_from_config() -> crate::presence::Mode {
    key_from_config(
        presence_in_toml,
        crate::presence::Mode::Meta,
        "presence defaults to meta",
    )
}

fn key_from_config<T: Copy>(
    in_toml: fn(&str) -> Result<Option<T>, String>,
    default: T,
    fallback: &str,
) -> T {
    key_from_config_or(in_toml, default, default, fallback)
}

fn key_from_config_or<T>(
    in_toml: fn(&str) -> Result<Option<T>, String>,
    absent: T,
    broken: T,
    fallback: &str,
) -> T {
    let Some(path) = config_path() else {
        return absent;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return absent,
        Err(e) => {
            eprintln!(
                "aterm-link: {} could not be read ({e}); {fallback}",
                path.display()
            );
            return broken;
        }
    };
    match in_toml(&text) {
        Ok(v) => v.unwrap_or(absent),
        Err(e) => {
            eprintln!("aterm-link: {}: {e}; {fallback}", path.display());
            broken
        }
    }
}

/// `[fabric] receipts` out of an aterm.toml's text: `Ok(None)` when the table or the key is absent.
pub fn receipts_in_toml(text: &str) -> Result<Option<bool>, String> {
    fabric_key(text, "receipts", |v| {
        v.as_bool()
            .map(Some)
            .ok_or_else(|| "`[fabric] receipts` is not a boolean".to_string())
    })
}

/// `[fabric] receipts` from the aterm.toml [`config_path`] resolves, for a `serve` whose command line said neither `--receipts` nor `--no-receipts`.
#[must_use]
pub fn receipts_from_config() -> bool {
    key_from_config(receipts_in_toml, true, "receipts default to on")
}

/// `[fabric] log` in `text`: `"auto"`, `"off"` or `"turns"`.
pub fn log_in_toml(text: &str) -> Result<Option<crate::mirror::LogMode>, String> {
    fabric_key(text, "log", |v| match v.as_str() {
        Some(s) => crate::mirror::LogMode::parse(s).map(Some).ok_or_else(|| {
            format!("`[fabric] log = {s:?}` is none of \"auto\", \"off\" and \"turns\"")
        }),
        None => Err("`[fabric] log` is not a string".to_string()),
    })
}

/// `[fabric] log` from `aterm.toml` (`auto` when absent; `off` with a stderr warning when unreadable or invalid).
#[must_use]
pub fn log_from_config() -> crate::mirror::LogMode {
    key_from_config_or(
        log_in_toml,
        crate::mirror::LogMode::Auto,
        crate::mirror::LogMode::Off,
        "the session-log mirror stays off",
    )
}

/// The configured command's argv, env first — the app's own precedence.
pub fn resolve_command(
    env: Option<&str>,
    path: Option<&Path>,
) -> Result<Option<(Source, Vec<String>)>, String> {
    if let Some(v) = env.filter(|v| !v.trim().is_empty()) {
        return Ok(Some((
            Source::Env,
            aterm_types::fabric_command::split_line(v),
        )));
    }
    let Some(path) = path else {
        return Ok(None);
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    Ok(command_in_toml(&text)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .map(|c| (Source::File(path.to_path_buf()), c)))
}

fn rendezvous_command() -> Result<Option<(Source, Vec<String>)>, String> {
    Ok(crate::enable::Rendezvous::read()?
        .and_then(|r| crate::enable::rendezvous_path().map(|p| (Source::Rendezvous(p), r.command))))
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_link_verb<S: AsRef<str>>(words: &[S], i: usize, verb: &str) -> bool {
    i >= 1
        && words[i].as_ref() == verb
        && (words[i - 1].as_ref() == "link" || basename(words[i - 1].as_ref()) == "aterm-link")
}

/// The flags of an `aterm link serve …` / `aterm-link serve …` argv, or `None` when the argv is not a bridge at all.
#[must_use]
pub fn serve_flags<S: AsRef<str>>(argv: &[S]) -> Option<Vec<String>> {
    let at = (0..argv.len()).find(|&i| is_link_verb(argv, i, "serve"))?;
    Some(
        argv[at + 1..]
            .iter()
            .map(|w| w.as_ref().to_string())
            .collect(),
    )
}

/// The program prefix of a bridge command (`argv` before `link serve` / `serve`), with `argv[0]` resolved on `$PATH` and canonicalized, formatted with [`shown_argv`].
#[must_use]
pub fn bridge_program<S: AsRef<str>>(argv: &[S]) -> Option<String> {
    let at = (0..argv.len()).find(|&i| is_link_verb(argv, i, "serve"))?;
    let end = if argv[at - 1].as_ref() == "link" && at >= 2 {
        at - 1
    } else {
        at
    };
    let mut program = vec![resolve_program(argv[0].as_ref())];
    program.extend(argv[1..end].iter().map(|w| w.as_ref().to_string()));
    Some(shown_argv(&program))
}

fn resolve_program(word: &str) -> String {
    let path = if word.contains('/') {
        Some(PathBuf::from(word))
    } else {
        std::env::var_os("PATH")
            .and_then(|p| crate::enable::which_in(std::ffi::OsStr::new(word), &p))
    };
    path.and_then(|p| std::fs::canonicalize(p).ok())
        .map_or_else(|| word.to_string(), |p| p.to_string_lossy().into_owned())
}

/// The bridge command's `Config`, through the bridge's own parser.
pub(crate) fn bridge_config<S: AsRef<str>>(argv: &[S]) -> Result<Config, String> {
    let flags = serve_flags(argv).ok_or_else(|| {
        format!(
            "the command is not a bridge (`aterm link serve …` or `aterm-link serve …`): {}",
            safe(&shown_argv(argv), 512)
        )
    })?;
    crate::cli::parse(&flags).map(|p| p.cfg)
}

/// One process, as `ps` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    /// Its pid.
    pub pid: u32,
    /// Its parent's pid.
    pub ppid: u32,
    /// Its command line, space-joined as `ps` prints it.
    pub cmd: String,
}

/// Parse `ps -o pid= -o ppid= -o command=` output.
#[must_use]
pub fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.trim_start().splitn(2, char::is_whitespace);
            let pid = it.next()?.parse().ok()?;
            let rest = it.next()?.trim_start();
            let mut it = rest.splitn(2, char::is_whitespace);
            let ppid = it.next()?.parse().ok()?;
            let cmd = it.next().unwrap_or("").trim().to_string();
            Some(Proc { pid, ppid, cmd })
        })
        .collect()
}

/// The machine's process table, or empty when it cannot be read (a sandbox) — every consumer then says "not found" rather than guessing.
pub(crate) fn process_table() -> Vec<Proc> {
    #[cfg(target_os = "macos")]
    {
        use aterm_sysprobe::kproc;
        kproc::records()
            .unwrap_or_default()
            .iter()
            .map(|r| Proc {
                pid: r.pid,
                ppid: r.ppid,
                cmd: kproc::ps_args(r, kproc::Locale::Utf8).trim().to_string(),
            })
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ps_table()
    }
}

#[cfg(not(target_os = "macos"))]
fn ps_table() -> Vec<Proc> {
    aterm_uds::spawnfd::command("ps")
        .args(["-A", "-ww", "-o", "pid=", "-o", "ppid=", "-o", "command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// The process serving the broker at `sock`: `aterm link broker <sock> …` or `aterm-link broker <sock> …`.
#[must_use]
pub fn broker_pid(procs: &[Proc], sock: &str) -> Option<u32> {
    procs.iter().find_map(|p| {
        let words: Vec<&str> = p.cmd.split_whitespace().collect();
        (0..words.len())
            .any(|i| is_link_verb(&words, i, "broker") && words.get(i + 1) == Some(&sock))
            .then_some(p.pid)
    })
}

/// The launchd job label whose running pid is `pid`, from `launchctl list`'s `PID  Status  Label` rows.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn parse_launchctl(text: &str, pid: u32) -> Option<String> {
    text.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let p: u32 = cols.next()?.parse().ok()?;
        let _status = cols.next()?;
        let label = cols.next()?;
        (p == pid).then(|| label.to_string())
    })
}

#[cfg(target_os = "macos")]
fn launchd_label(pid: u32) -> Option<String> {
    parse_launchctl(&crate::enable::launchctl_list(), pid)
}

#[cfg(not(target_os = "macos"))]
fn launchd_label(_pid: u32) -> Option<String> {
    None
}

/// What the broker probe found.
#[derive(Debug, Clone, Default)]
pub struct BrokerView {
    /// The broker socket the command names.
    pub endpoint: String,
    /// Whether a broker ANSWERED — a connect and a `Hello` round trip, in this build's astream protocol.
    pub reachable: bool,
    /// Why it is not reachable, or why the cap file was refused.
    pub error: Option<String>,
    /// A broker that answered the `Hello` in ANOTHER astream protocol (astream's `ProtocolMismatch`): nothing either side sends can be read by the other, so it is not `reachable`, and this names why.
    pub protocol: Option<astream_broker::ProtocolMismatch>,
    /// The last line the broker's supervisor captured from its stderr (`<root>/broker.err`) and how old it is, read only when the broker does not answer: the broker's own reason it is not running — a log it refuses to open, an address in use — which `fabric on` otherwise only points at ([`crate::enable::supervisor_last_word`]).
    pub last_word: Option<String>,
    /// Whether every cap was attached, so the bus can be read.
    pub attached: bool,
    /// Whether the broker refused the ring as LAPSED (astream's [`astream_broker::BrokerRefusal::is_expired`]): genuine grants whose `exp=` has passed, at the attach or on the head query.
    pub cap_expired: bool,
    /// The pid serving it, from the process table.
    pub pid: Option<u32>,
    /// The launchd job running that pid (macOS).
    pub launchd: Option<String>,
    /// The bus head offset — the next offset the log will assign.
    pub head: Option<u64>,
    /// How long connect + hello + attach + head took.
    pub rtt_ms: Option<u64>,
    /// `None` when the report reads the whole fleet (`/f/<F>/>`); `Some(faces)` when the broker is GUARDED and the cap files do not grant that — every broker `on` runs is — so every read is made over the faces the caps DO grant, and TRAFFIC says so.
    pub faces: Option<Vec<String>>,
    /// Whether the broker checks capabilities, measured by attaching one grant minted under [`GUARD_PROBE_SECRET`]: `Some(true)` when it is refused; `Some(false)` when acknowledged, so the cap-forced `<src>`, `trust=human` and per-human halts rest on nothing.
    pub guarded: Option<bool>,
    /// The broker's argv0 when that path now names a different file than the image it runs: an update swapped the build and this broker predates it.
    pub replaced: Option<String>,
    /// What the broker's ack promises, read off the serving process's argv (a bounded, byte-preserving kernel argv read bracketed by matching process-start records; [`crate::durability`]): no wire request names the tier, and the argv is what the process was started with, not what a job file says it would be.
    pub durability: Option<crate::durability::Durability>,
    /// What the broker SAYS it serves: its `Hello` answer's tag 0x01 ([`astream_broker::Client::served_durability`]) — the tier it was opened at, the one in force and why they differ.
    pub served: Option<astream_broker::ServedInfo>,
    /// The log's health as the broker SAYS it: its `Hello` answer's entry 0x04 ([`astream_broker::Client::broker_health`]) — whether the log is POISONED (it refuses every write until it is reopened) and astream's reason, and the flushes it has issued since it opened.
    pub health: Option<astream_broker::HelloHealth>,
    /// The log the serving process's argv names ([`crate::broker_args::BrokerArgs::log_path`]): where astream keeps its health snapshot.
    pub log: Option<PathBuf>,
    /// astream's health snapshot beside that log (`<log>.health`, R5: what the log holds, who is attached, its flushes and its poison), as astream's own reader found it ([`astream_broker::read_health`]) — with whether the process that wrote it still serves the log.
    pub snapshot: Option<Result<astream_broker::HealthRead, String>>,
}

const GUARD_PROBE_SECRET: &[u8] = b"aterm fabric guard probe: no broker's secret";

impl BrokerView {
    /// Whether the bus was read: connect, `Hello`, every attach and the head query all answered.
    #[must_use]
    pub fn read_ok(&self) -> bool {
        self.reachable && self.attached && self.head.is_some()
    }

    /// The tier the report names: the one the serving process's argv names ([`Self::durability`]), else — a broker this host cannot read the argv of — the one its `Hello` says it was opened at ([`Self::served`]).
    #[must_use]
    pub fn tier(&self) -> Option<crate::durability::Durability> {
        self.durability.or_else(|| {
            self.served
                .and_then(|s| crate::durability::Durability::of_tier(s.requested))
        })
    }

    /// astream's health snapshot ([`Self::snapshot`]) only when it is the serving broker's own NOW: astream's reader says its writer still serves the log, and that writer is [`Self::pid`] ([`crate::broker_health::is_current`]).
    #[must_use]
    pub fn snapshot_now(&self) -> Option<&astream_broker::BrokerHealth> {
        match &self.snapshot {
            Some(Ok(read)) if crate::broker_health::is_current(read, self.pid) => {
                Some(&read.health)
            }
            _ => None,
        }
    }

    /// Whether the broker SAYS its log is poisoned, and why: the `Hello`'s entry 0x04 ([`Self::health`]), the in-band word, before the snapshot beside the log — which a poisoned broker has written too, but as old as its period.
    #[must_use]
    pub fn poisoned(&self) -> Option<&str> {
        self.health
            .as_ref()
            .and_then(|h| h.poisoned.as_deref())
            .or_else(|| self.snapshot_now().and_then(|h| h.poisoned.as_deref()))
    }

    /// Name the pid serving [`Self::endpoint`] in `procs`, and its launchd job.
    pub(crate) fn locate(&mut self, procs: &[Proc]) {
        self.locate_with(procs, launchd_label, replaced_build, |pid| {
            aterm_uds::process::read_process_argv(i32::try_from(pid).ok()?)
        });
    }

    fn locate_with(
        &mut self,
        procs: &[Proc],
        mut label: impl FnMut(u32) -> Option<String>,
        mut replaced: impl FnMut(u32, &str) -> Option<String>,
        mut argv: impl FnMut(u32) -> Option<Vec<std::ffi::OsString>>,
    ) {
        self.pid = broker_pid(procs, &self.endpoint);
        self.launchd = self.pid.and_then(&mut label);
        let cmd = self
            .pid
            .and_then(|pid| procs.iter().find(|p| p.pid == pid))
            .map(|p| p.cmd.as_str());
        let words = self.pid.and_then(&mut argv);
        let exe = words
            .as_ref()
            .and_then(|w| w.first())
            .map(|w| w.to_string_lossy().into_owned())
            .or_else(|| {
                cmd.and_then(|c| c.split_whitespace().next())
                    .map(str::to_string)
            });
        self.replaced = self.pid.zip(exe).and_then(|(pid, exe)| replaced(pid, &exe));
        let parsed = words
            .as_deref()
            .and_then(crate::broker_args::BrokerArgs::from_invocation)
            .filter(|parsed| parsed.serves_endpoint(&self.endpoint));
        self.durability = parsed
            .as_ref()
            .and_then(crate::broker_args::BrokerArgs::serving_durability);
        self.log = parsed
            .as_ref()
            .and_then(crate::broker_args::BrokerArgs::log_path);
    }
}

fn replaced_build(pid: u32, exe: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let exe = Some(exe).filter(|w| w.starts_with('/'))?;
    let running = running_image_inode(pid)?;
    let now = std::fs::metadata(exe).ok()?.ino();
    (now != running).then(|| exe.to_string())
}

/// The inode of the executable image `pid` runs, whatever has happened to its path since: `/proc/<pid>/exe` follows to it even once it is unlinked. macOS reads its lowest vnode-backed mapping in this process ([`aterm_sysprobe::kproc::image_inode`]), the file `lsof -d txt` lists first, which keeps the inode the process mapped after a new file is renamed over its path: an `lsof` child cost 0.1-0.16 s of every broker start and `aterm fabric` step.
#[cfg(target_os = "linux")]
pub(crate) fn running_image_inode(pid: u32) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}/exe"))
        .ok()
        .map(|m| m.ino())
}

/// See the Linux twin: macOS's in-process read.
#[cfg(target_os = "macos")]
pub(crate) fn running_image_inode(pid: u32) -> Option<u64> {
    aterm_sysprobe::kproc::image_inode(pid)
}

/// See the Linux twin: another unix asks `lsof` ([`lsof_image_inode`]).
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn running_image_inode(pid: u32) -> Option<u64> {
    lsof_image_inode(pid)
}

#[cfg(any(
    all(target_os = "macos", test),
    not(any(target_os = "linux", target_os = "macos"))
))]
fn lsof_image_inode(pid: u32) -> Option<u64> {
    let out = aterm_uds::spawnfd::command("/usr/sbin/lsof")
        .args([
            "-n",
            "-P",
            "-a",
            "-p",
            &pid.to_string(),
            "-d",
            "txt",
            "-F",
            "i",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    lsof_first_inode(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
fn lsof_first_inode(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix('i').and_then(|n| n.parse().ok()))
}

/// The faces a report may read when the broker is guarded and `/f/<F>/>` is not granted: every grant's filter under `/f/<F>/` in the cap files, both modes (a guarded broker authorizes a read by any grant containing it), except the consumer-group names under `/f/<F>/cur/`, which are never published to.
#[must_use]
pub fn readable_faces(fleet: &str, cap_files: &[String]) -> Vec<String> {
    let root = format!("/f/{fleet}/");
    let groups = format!("/f/{fleet}/cur/");
    let mut faces = BTreeSet::new();
    for path in cap_files {
        for cap in read_cap_file(path).unwrap_or_default() {
            if let Ok(g) = astream_cap::Grant::parse(&cap.grant) {
                if g.filter.starts_with(&root) && !g.filter.starts_with(&groups) {
                    faces.insert(g.filter);
                }
            }
        }
    }
    faces.into_iter().collect()
}

fn refused(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some()
}

fn lapsed(e: &io::Error) -> bool {
    astream_broker::broker_refusal(e).is_some_and(astream_broker::BrokerRefusal::is_expired)
}

/// The words `aterm fabric`'s warning for a lapsed ring is built from ([`BrokerView::cap_expired`]), and that `doctor` picks the renewal by ([`crate::enable::fix_for`]).
pub const CAP_LAPSED: &str = "refuses this node's cap as lapsed";

/// The words `aterm fabric`'s warning for a poisoned log is built from ([`BrokerView::poisoned`]), and that `doctor` picks its remedy by ([`crate::enable::fix_for`]).
pub const POISONED_WARNING: &str = "says its log is POISONED";

/// The words of the warning for a broker whose flushes have failed without poisoning its log, which `doctor` picks the disk's remedy by.
pub const FLUSH_FAILED_WARNING: &str = "says a flush of its log failed";

/// Reach the broker FOR REAL: connect, `Hello` (does a broker answer at all), the whole ring attached ([`transport::attach_ring`]), head query.
pub(crate) fn probe_broker(cfg: &Config, procs: &[Proc]) -> (BrokerView, Option<Conn>) {
    let mut view = BrokerView {
        endpoint: cfg.broker.clone(),
        ..BrokerView::default()
    };
    view.locate(procs);
    view.snapshot = view
        .log
        .as_deref()
        .and_then(|log| match astream_broker::read_health(log) {
            Ok(read) => read.map(Ok),
            Err(e) => Some(Err(e.to_string())),
        });
    let started = Instant::now();
    let mut conn = match transport::connect(&cfg.broker, IO_TIMEOUT) {
        Ok((c, _)) => c,
        Err(e) => {
            view.error = Some(format!("connect: {e}"));
            view.last_word = supervisor_last_word(cfg);
            return (view, None);
        }
    };
    if let Err(e) = conn.hello() {
        match astream_broker::protocol_mismatch(&e) {
            Some(skew) => {
                view.protocol = Some(*skew);
                view.error = Some(format!(
                    "a broker answered in another astream protocol: {e}"
                ));
            }
            None => {
                view.error = Some(format!("the socket accepted but no broker answered: {e}"));
                view.last_word = supervisor_last_word(cfg);
            }
        }
        return (view, None);
    }
    view.reachable = true;
    view.served = conn.served_durability();
    view.health = conn.broker_health().cloned();
    let mut ring = Vec::new();
    for path in &cfg.cap_files {
        match read_cap_file(path) {
            Ok(caps) => ring.extend(caps),
            Err(e) => {
                view.error = Some(format!("cap file {path}: {e}"));
                return (view, None);
            }
        }
    }
    if let Err(e) = transport::attach_ring(&mut conn, &ring) {
        view.cap_expired = lapsed(&e);
        view.error = Some(format!(
            "the broker refused the ring in {}: {e}",
            cfg.cap_files.join(" ")
        ));
        return (view, None);
    }
    view.attached = true;
    match conn.fetch(0, &fleet_root(&cfg.fleet), 0) {
        Ok((_, (_, head))) => view.head = Some(head),
        Err(e) if refused(&e) => {
            let faces = readable_faces(&cfg.fleet, &cfg.cap_files);
            let head = faces
                .iter()
                .find_map(|f| conn.fetch(0, f, 0).ok().map(|(_, (_, h))| h));
            match head {
                Some(h) => {
                    view.head = Some(h);
                    view.faces = Some(faces);
                }
                None => {
                    view.cap_expired = lapsed(&e);
                    view.error = Some(format!("head query: {e}"));
                    return (view, None);
                }
            }
        }
        Err(e) => {
            view.error = Some(format!("head query: {e}"));
            return (view, None);
        }
    }
    view.rtt_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    if !cfg.cap_files.is_empty() {
        if let Ok(probe) = astream_cap::mint(
            GUARD_PROBE_SECRET,
            &format!("ro:{}", crate::subject::fleet_filter(&cfg.fleet)),
        ) {
            view.guarded = match conn.attach(&probe.filter, &probe.tag) {
                Ok(_) => Some(false),
                Err(e) if refused(&e) => Some(true),
                Err(_) => None,
            };
        }
    }
    (view, Some(conn))
}

fn supervisor_last_word(cfg: &Config) -> Option<String> {
    let root = crate::enable::root_of(&cfg.broker)?;
    crate::enable::supervisor_last_word(&root)
}

/// The BROKER section's `health` row, and doctor's `health:` line: the log's health as the broker's own `Hello` said it ([`BrokerView::health`], [`crate::broker_health::in_band`]).
#[must_use]
pub fn health_row(b: &BrokerView) -> Option<String> {
    match &b.health {
        Some(h) => Some(crate::broker_health::in_band(h)),
        None if b.reachable => Some(
            "- (the broker's Hello carries no health entry: it predates the astream this aterm \
             speaks, and whether its log is poisoned is not said)"
                .to_string(),
        ),
        None => None,
    }
}

/// The BROKER section's `holds` row at `now_ms`: what the log holds and who is attached, from astream's snapshot beside the log the serving process's argv names ([`BrokerView::snapshot`], [`crate::broker_health::snapshot_row`]), said as the serving broker's own only when astream's reader says so.
#[must_use]
pub fn holds_row(b: &BrokerView, now_ms: u64) -> Option<String> {
    let log = b.log.as_ref()?;
    Some(match &b.snapshot {
        Some(Ok(read)) => crate::broker_health::snapshot_row(read, b.pid, now_ms),
        Some(Err(e)) => format!(
            "- (astream's health snapshot beside {} could not be read: {})",
            safe(&log.display().to_string(), 256),
            safe(e, 256)
        ),
        None => format!(
            "- (no health snapshot beside {}: a broker older than this aterm keeps none, and one \
             of this build that could not write it says why on its stderr, the supervisor's \
             broker.err)",
            safe(&log.display().to_string(), 256)
        ),
    })
}

fn health_json(b: &BrokerView, now_ms: u64) -> J {
    let flushes = |f: &astream_broker::FlushCounts| {
        J::Obj(vec![
            ("full", J::Num(f.full)),
            ("barrier", J::Num(f.barrier)),
            ("skipped", J::Num(f.skipped)),
            ("failed", J::Num(f.failed)),
        ])
    };
    let in_band = b.health.as_ref().map_or(J::Null, |h| {
        J::Obj(vec![
            ("poisoned", J::opt_s(h.poisoned.as_deref())),
            ("flushes", flushes(&h.flushes)),
        ])
    });
    let snapshot = match &b.snapshot {
        None => J::Null,
        Some(Err(e)) => J::Obj(vec![("error", J::s(e))]),
        Some(Ok(read)) => {
            let h = &read.health;
            J::Obj(vec![
                ("pid", J::Num(u64::from(h.pid))),
                (
                    "writer",
                    J::s(match read.writer {
                        astream_broker::HealthWriter::Serving => "serving",
                        astream_broker::HealthWriter::Gone => "gone",
                        astream_broker::HealthWriter::Unknown => "unknown",
                    }),
                ),
                (
                    "current",
                    J::Bool(crate::broker_health::is_current(read, b.pid)),
                ),
                ("age_ms", J::Num(now_ms.saturating_sub(h.written_at_ms))),
                ("poisoned", J::opt_s(h.poisoned.as_deref())),
                ("records", J::Num(h.retained_records)),
                ("bytes", J::Num(h.retained_bytes)),
                ("subjects", J::Num(h.distinct_subjects)),
                ("pending_wills", J::Num(h.pending_wills)),
                ("group_commits", J::Num(h.group_commits)),
                ("connections", J::Num(h.active_conns)),
                ("parked_tails", J::Num(h.parked_tails)),
                ("durable_head", J::Num(h.durable_head)),
                ("flushes", flushes(&h.flushes)),
            ])
        }
    };
    J::Obj(vec![
        (
            "log",
            J::opt_s(b.log.as_ref().map(|l| l.to_string_lossy()).as_deref()),
        ),
        ("in_band", in_band),
        ("snapshot", snapshot),
    ])
}

/// How much the top-level `aterm doctor`'s `fabric:` row asks of its reader ([`DoctorSummary`]), in that report's own three weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryLevel {
    /// The broker answers, the bus reads, and it says its log takes writes.
    Ok,
    /// A fact with nothing to fix here: the fabric is off, or a broker that answers does not say whether its log is poisoned (it predates the entry).
    Note,
    /// Mail does not move: the broker does not answer, its bus cannot be read, or it says its log is poisoned.
    Warn,
}

/// The fabric in ONE ROW, for the top-level `aterm doctor` ([`doctor_summary`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorSummary {
    pub level: SummaryLevel,
    pub text: String,
    /// The observation did not establish health, independently of its display level.
    pub unmeasured: bool,
}

/// Top-level `aterm doctor` `fabric:` row: probes the broker named by `$ATERM_FABRIC_COMMAND`, `aterm.toml`, or `fabric.toml` without querying local instances or scanning the roster.
#[must_use]
pub fn doctor_summary(withheld: Option<&str>) -> DoctorSummary {
    let config = config_path();
    let found = match resolve_command(command_seam().as_deref(), config.as_deref()) {
        Ok(Some(found)) => Ok(Some(found)),
        Ok(None) => rendezvous_command(),
        Err(e) => Err(e),
    };
    summary_for(
        found.map(|found| found.map(|(_, command)| command)),
        withheld,
        process_table,
    )
}

/// [`doctor_summary`] over the bridge command found (`None`: the fabric is off), the process
/// table read through `table` and never while `withheld` says why: then a configured
/// fabric is an unmeasured note that says why, and an off fabric and a config fault answer
/// as ever, since they read none.
fn summary_for(
    found: Result<Option<Vec<String>>, String>,
    withheld: Option<&str>,
    table: impl FnOnce() -> Vec<Proc>,
) -> DoctorSummary {
    let command = match found {
        Ok(Some(command)) => command,
        Ok(None) => {
            return DoctorSummary {
                level: SummaryLevel::Note,
                unmeasured: false,
                text: "off: no [fabric] command in aterm.toml and no rendezvous file \
                       (`aterm fabric on` turns it on; an instance started while it was on keeps \
                       its bridge until it is relaunched, which `aterm fabric` reports)"
                    .to_string(),
            };
        }
        Err(e) => {
            return DoctorSummary {
                level: SummaryLevel::Warn,
                unmeasured: true,
                text: format!(
                    "the fabric's config cannot be read ({}) — `aterm fabric` says where",
                    safe(&e, 256)
                ),
            };
        }
    };
    match (bridge_config(&command), withheld) {
        (Ok(cfg), None) => summary_of(&probe_broker(&cfg, &table()).0),
        (Ok(_), Some(why)) => DoctorSummary {
            level: SummaryLevel::Note,
            unmeasured: true,
            text: format!("not read ({why})"),
        },
        (Err(e), _) => DoctorSummary {
            level: SummaryLevel::Warn,
            unmeasured: true,
            text: format!(
                "the [fabric] command does not parse ({}) — `aterm fabric` says where it is",
                safe(&e, 256)
            ),
        },
    }
}

/// [`doctor_summary`]'s verdict on one probe, pure: the broker's own words (its `Hello`'s served tier, entry 0x01, and its health, entry 0x04), never the argv's.
#[must_use]
pub fn summary_of(b: &BrokerView) -> DoctorSummary {
    let at = safe(&b.endpoint, 256);
    let fix = "`aterm fabric doctor` names the fix";
    let warn = |text: String, unmeasured| DoctorSummary {
        level: SummaryLevel::Warn,
        unmeasured,
        text,
    };
    if let Some(skew) = b.protocol {
        return warn(
            format!(
            "the broker at {at} speaks astream protocol {} and this aterm speaks {}: no record \
             lands and none is delivered — {fix}",
            skew.broker, skew.client
        ),
            false,
        );
    }
    if !b.reachable {
        return warn(
            format!(
                "the broker at {at} does not answer ({}) — {fix}",
                safe(b.error.as_deref().unwrap_or("unknown"), 256)
            ),
            true,
        );
    }
    if let Some(why) = b.poisoned() {
        return warn(
            format!(
                "the broker at {at} {POISONED_WARNING} ({}): it refuses every write until the log \
             is reopened — {fix}",
                safe(why, 256)
            ),
            false,
        );
    }
    let served = match (b.served, b.tier()) {
        (Some(s), Some(asked))
            if crate::durability::Durability::of_tier(s.served) != Some(asked) =>
        {
            format!(
                "serves {} (asked {})",
                crate::durability::tier_name(s.served),
                asked.name()
            )
        }
        (Some(s), _) => format!("serves {}", crate::durability::tier_name(s.served)),
        (None, Some(asked)) => format!("runs {} (it does not say what it serves)", asked.name()),
        (None, None) => "does not say what it serves".to_string(),
    };
    if !b.read_ok() {
        return warn(
            format!(
            "the broker at {at} answers and {served}, but the bus could not be read ({}) — {fix}",
            safe(b.error.as_deref().unwrap_or("unknown"), 256)
        ),
            true,
        );
    }
    match &b.health {
        Some(_) => DoctorSummary {
            level: SummaryLevel::Ok,
            unmeasured: false,
            text: format!(
                "the broker at {at} answers, {served}, and says its log is not poisoned \
                 (`aterm fabric` reads the rest)"
            ),
        },
        None => DoctorSummary {
            level: SummaryLevel::Note,
            unmeasured: true,
            text: format!(
                "the broker at {at} answers and {served}, and does not say whether its log is \
                 poisoned (it predates the health entry; `aterm fabric` reads the rest)"
            ),
        },
    }
}

/// Every record of the fleet: `/f/<F>/>`.
#[must_use]
pub fn fleet_root(fleet: &str) -> String {
    format!("/f/{fleet}/>")
}

/// One bus record, as TRAFFIC and `tail` show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Traffic {
    /// Its bus offset.
    pub off: u64,
    /// The publisher's wall clock (`t=`), ms — informational, `0` when absent.
    pub t_ms: u64,
    /// Who sent it: the cap-forced `<src>` segment, or `<sid>@<node>` when a node attests which of its sessions spoke (the bridge's own rule).
    pub from: String,
    /// Who it is for: `<sid>@<node>`, `fleet`, `all`, or `-` for a node's own face.
    pub to: String,
    /// The record's kind: the `in` kind, or the face's leaf (`presence`, …).
    pub kind: String,
    /// The body's text length in bytes.
    pub len: usize,
    /// The receiver's trust label, computed from the address alone ([`crate::render::trust_of`]).
    pub trust: &'static str,
    /// The text itself — only `tail --bodies` ever prints it.
    pub text: String,
}

fn render_from(src: &str, body_from: Option<&str>) -> String {
    match body_from {
        Some(sid)
            if src.starts_with("n-")
                && sid.starts_with("s-")
                && crate::subject::is_principal(sid) =>
        {
            format!("{sid}@{src}")
        }
        _ => src.to_string(),
    }
}

/// Classify one record.
#[must_use]
pub fn traffic_of(fleet: &str, off: u64, subject: &str, raw: &[u8]) -> Traffic {
    if raw.starts_with(b"v=2 ") {
        if let Some(t) = mirrored_traffic(fleet, off, subject, raw) {
            return t;
        }
    }
    traffic_of_body(fleet, off, subject, Body::decode(raw).0)
}

fn mirrored_traffic(fleet: &str, off: u64, subject: &str, raw: &[u8]) -> Option<Traffic> {
    let text = std::str::from_utf8(raw).ok()?;
    let b = aterm_slog::decode_bus(text).ok()?;
    let mut t = traffic_of_body(fleet, off, subject, Body::default());
    if t.kind != "ev" {
        return None;
    }
    t.t_ms = b.rec.t;
    t.len = raw.len();
    t.trust = b.rec.src.word();
    t.text = text.to_string();
    Some(t)
}

/// [`traffic_of`] over a body already decoded.
pub(crate) fn traffic_of_body(fleet: &str, off: u64, subject: &str, body: Body) -> Traffic {
    let relayed = body.via.is_some();
    let segs: Vec<&str> = subject.split('/').collect();
    let seg = |i: usize| safe(segs.get(i).copied().unwrap_or("-"), 128);
    let face = if segs.len() >= 5 && segs[0].is_empty() && segs[1] == "f" && segs[2] == fleet {
        segs[3]
    } else {
        ""
    };
    let (from, to, kind, trust) = match (face, segs.len()) {
        ("in", 8) => (
            safe(&render_from(segs[6], body.from.as_deref()), 128),
            format!("{}@{}", seg(5), seg(4)),
            seg(7),
            trust_of(segs[6], relayed),
        ),
        ("fleet", 6) => (
            seg(4),
            "fleet".to_string(),
            seg(5),
            trust_of(segs[4], relayed),
        ),
        ("term", 8) if segs[6] == "in" => (
            seg(7),
            format!("{}@{}", seg(5), seg(4)),
            "term/in".to_string(),
            SCREEN,
        ),
        ("term", n) if n >= 7 => (
            format!("{}@{}", seg(5), seg(4)),
            "-".to_string(),
            format!("term/{}", safe(&segs[6..].join("/"), 64)),
            SCREEN,
        ),
        ("pub", 8) if segs[6] == "say" => (
            format!("{}@{}", seg(5), seg(4)),
            format!("say:{}", seg(7)),
            safe(body.kind.as_deref().unwrap_or("note"), 32),
            trust_of(segs[4], relayed),
        ),
        ("pub", n) if n >= 7 => {
            let leaf = safe(&segs[6..].join("/"), 64);
            let from = if segs[5] == "node" {
                seg(4)
            } else {
                format!("{}@{}", seg(5), seg(4))
            };
            let trust = if leaf.starts_with("ev") {
                SCREEN
            } else {
                trust_of(segs[4], relayed)
            };
            (from, "-".to_string(), leaf, trust)
        }
        _ => (
            "-".to_string(),
            safe(subject, 128),
            if face.is_empty() {
                "unknown".to_string()
            } else {
                safe(face, 32)
            },
            SCREEN,
        ),
    };
    Traffic {
        off,
        t_ms: body.t,
        from,
        to,
        kind,
        len: body.text.len(),
        trust,
        text: body.text,
    }
}

fn last_records(
    conn: &mut Conn,
    fleet: &str,
    faces: Option<&[String]>,
    head: u64,
    n: usize,
) -> io::Result<Vec<Traffic>> {
    let filters: Vec<String> = faces.map_or_else(|| vec![fleet_root(fleet)], <[String]>::to_vec);
    let mut merged: BTreeMap<u64, Traffic> = BTreeMap::new();
    let mut window: u64 = 256;
    loop {
        let start = head.saturating_sub(window);
        for filter in &filters {
            let mut next = start;
            loop {
                let ((page, (after, now_head)), _) =
                    transport::fetch_kept(conn, next, filter, transport::POST_PAGE)?;
                for (off, subject, raw) in &page {
                    let older = merged.len() >= n
                        && merged
                            .first_key_value()
                            .is_none_or(|(first, _)| off < first);
                    if older || merged.contains_key(off) {
                        continue;
                    }
                    merged.insert(*off, traffic_of(fleet, *off, subject, raw));
                    if merged.len() > n {
                        merged.pop_first();
                    }
                }
                if after <= next || after >= now_head {
                    break;
                }
                next = after;
            }
        }
        if merged.len() >= n || start == 0 || window >= TRAFFIC_SCAN_MAX {
            return Ok(merged.into_values().collect());
        }
        window = window.saturating_mul(16).min(TRAFFIC_SCAN_MAX);
    }
}

fn record_t(conn: &mut Conn, filter: &str, off: u64) -> Option<u64> {
    let (page, _) = conn.fetch(off, filter, 1).ok()?;
    let (at, _, raw) = page.first()?;
    (*at == off).then(|| Body::decode(raw).0.t)
}

/// An `ask` or `task` whose `dl=` has passed with no `answer`, `report` or `ack` carrying its offset as `re=` anywhere on the fleet's `in` lanes (R8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overdue {
    /// The ask's offset — what a reply would carry as `re=`.
    pub off: u64,
    /// Who asked, as TRAFFIC renders it.
    pub from: String,
    /// Who was asked: `<sid>@<node>`.
    pub to: String,
    /// `ask` or `task`.
    pub kind: String,
    /// The advisory deadline it carried, ms.
    pub dl: u64,
    /// How long past the deadline it is, ms.
    pub late_ms: u64,
    /// Whether the asker's own bridge has already put `expired re=<off>` on the asker's lane — the verdict R8 makes that bridge responsible for.
    pub expired: bool,
}

const OVERDUE_SCAN_SPAN: u64 = 1 << 16;

fn overdue_work(
    conn: &mut Conn,
    fleet: &str,
    faces: Option<&[String]>,
    head: u64,
    now_ms: u64,
) -> io::Result<Vec<Overdue>> {
    let filters = in_filters(fleet, faces);
    let mut scan = OverdueScan::default();
    let mut fed: BTreeSet<u64> = BTreeSet::new();
    for filter in &filters {
        let mut next = head.saturating_sub(OVERDUE_SCAN_SPAN);
        loop {
            let ((page, (after, now_head)), _) =
                transport::fetch_kept(conn, next, filter, transport::POST_PAGE)?;
            for (off, subject, raw) in &page {
                if fed.insert(*off) {
                    scan.feed(fleet, *off, subject, raw);
                }
            }
            if after <= next || after >= now_head {
                break;
            }
            next = after;
        }
    }
    Ok(scan.finish(now_ms))
}

fn in_filters(fleet: &str, faces: Option<&[String]>) -> Vec<String> {
    let all = format!("/f/{fleet}/in/");
    match faces {
        None => vec![format!("{all}>")],
        Some(faces) => faces
            .iter()
            .filter(|f| f.starts_with(&all))
            .cloned()
            .collect(),
    }
}

/// The overdue asks among `records` as of `now_ms`, for tests: a `WORK_KINDS` record whose `t + dl` is past, and no `answer|report|ack` with `re=` naming it.
#[cfg(test)]
#[must_use]
pub(crate) fn overdue_of(
    fleet: &str,
    records: &[(u64, String, Vec<u8>)],
    now_ms: u64,
) -> Vec<Overdue> {
    let mut scan = OverdueScan::default();
    for (off, subject, raw) in records {
        scan.feed(fleet, *off, subject, raw);
    }
    scan.finish(now_ms)
}

#[derive(Default)]
struct OverdueScan {
    settled: BTreeSet<u64>,
    expired: BTreeSet<u64>,
    asks: Vec<(Traffic, u64)>,
}

impl OverdueScan {
    fn feed(&mut self, fleet: &str, off: u64, subject: &str, raw: &[u8]) {
        let (body, _) = Body::decode(raw);
        let (dl, re) = (body.dl, body.re);
        let t = traffic_of_body(fleet, off, subject, body);
        match t.kind.as_str() {
            k if WORK_KINDS.contains(&k) => {
                if let Some(dl) = dl {
                    let t = Traffic {
                        text: String::new(),
                        ..t
                    };
                    self.asks.push((t, dl));
                }
            }
            "answer" | "report" | "ack" => {
                if let Some(re) = re {
                    self.settled.insert(re);
                }
            }
            "expired" => {
                if let Some(re) = re {
                    self.expired.insert(re);
                }
            }
            _ => {}
        }
    }

    fn finish(self, now_ms: u64) -> Vec<Overdue> {
        let Self {
            settled,
            expired,
            asks,
        } = self;
        asks.into_iter()
            .filter(|(t, _)| !settled.contains(&t.off))
            .filter_map(|(t, dl)| {
                let at = t.t_ms;
                let due = at.saturating_add(dl);
                (at > 0 && now_ms > due).then(|| Overdue {
                    off: t.off,
                    expired: expired.contains(&t.off),
                    from: t.from,
                    to: t.to,
                    kind: t.kind,
                    dl,
                    late_ms: now_ms - due,
                })
            })
            .collect()
    }
}

/// One `msg` row this report needs, without its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgMeta {
    /// The per-session row id.
    pub id: u64,
    /// The bus offset it landed at.
    pub off: u64,
    /// The sender, as the endpoint renders it.
    pub from: String,
    /// Its kind.
    pub kind: String,
}

/// What one local instance says about one of its sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalSession {
    /// The session id.
    pub sid: String,
    /// Its title (pct-decoded, not yet escaped).
    pub title: String,
    /// Its `meta role=`, or `-`.
    pub role: String,
    /// `hold=` from the inbox header.
    pub hold: Option<u8>,
    /// Who holds its keyboard, or `-`.
    pub holder: String,
    /// The handled watermark.
    pub seen: Option<u64>,
    /// Rows the bounded ring evicted unhandled.
    pub dropped: Option<u64>,
    /// Delivered rows no `inbox` has listed.
    pub pending: Option<u64>,
    /// Listed but not handled: rows above `seen=` minus `pending`.
    pub unread: Option<u64>,
    /// Outbound posts that have not landed.
    pub posts: Option<u64>,
    /// Every row above the watermark, for the age check.
    pub unhandled: Vec<MsgMeta>,
    /// The last recorded hold transition's `(reason, origins)`, when held ([`last_hold`]): `fleet,local` when both slots stand.
    pub hold_why: Option<(String, String)>,
    /// The broadcast topics this session has opted into (`topic ls`), in topic order.
    pub topics: Vec<String>,
    /// A ctl read that failed for this session.
    pub error: Option<String>,
}

/// What one local instance says about itself.
#[derive(Debug, Clone, Default)]
pub struct InstanceView {
    /// The instance pid.
    pub pid: u32,
    /// Its control socket.
    pub sock: String,
    /// Whether `aterm ctl --pid <pid>` reaches it ([`aterm_ctl::pid_reaches`]).
    pub pid_reaches: bool,
    /// Why it could not be asked, when it could not.
    pub error: Option<String>,
    /// `fabric status`'s `state=`.
    pub fabric: Option<String>,
    /// `fabric status`'s `reason=`: why the bridge's broker link is down, when `state=stalled` (`-` otherwise).
    pub reason: Option<String>,
    /// `fabric status`'s `rtt_ms=`: the bridge's last acknowledged round trip to the broker.
    pub rtt_ms: Option<u64>,
    /// `fabric status`'s `link_age_ms=`: how long ago that ack was.
    pub link_age_ms: Option<u64>,
    /// `fabric status`'s `supervised=`.
    pub supervised: Option<bool>,
    /// `fabric status`'s `build=` — the instance's own build — and `bridge_build=`, the build its bridge named in `hello` (absent from an instance or a bridge from before the handshake).
    pub build: Option<String>,
    pub bridge_build: Option<String>,
    /// `fabric status`'s `lane_faults=`: how many of its bridge's lines the instance could not parse.
    pub lane_faults: Option<u64>,
    /// The command it runs (or was launched with), decoded into its argv ([`status_argv`]).
    pub command: Option<Vec<String>>,
    /// The PROGRAM that command runs ([`bridge_program`]): resolved when the report was read, so a substituted program is named beside its flags.
    pub program: Option<String>,
    /// Its bridge child pid(s), from the process table.
    pub bridge_pids: Vec<u32>,
    /// The node its bridge answers as, from that command's state dir.
    pub node: Option<String>,
    /// The state dir its bridge command names.
    pub state_dir: Option<String>,
    /// Its sessions.
    pub sessions: Vec<LocalSession>,
}

/// The BRIDGES table's FABRIC cell: the state, plus the `stalled` reason or the last acked round trip and its age.
#[must_use]
pub fn fabric_cell(i: &InstanceView) -> String {
    let state = safe(i.fabric.as_deref().unwrap_or("-"), 32);
    match (i.fabric.as_deref(), &i.reason, i.rtt_ms, i.link_age_ms) {
        (Some("stalled"), Some(reason), _, _) => format!("{state} ({})", safe(reason, 32)),
        (Some("stale"), _, _, Some(age_ms)) => {
            format!("{state} (owed, last ack {} ago)", age(age_ms))
        }
        (Some("stale"), _, _, None) => format!("{state} (owed, never acked)"),
        (Some("connected"), _, Some(rtt), Some(age_ms)) => {
            format!("{state} (rtt {rtt} ms, acked {} ago)", age(age_ms))
        }
        _ => state,
    }
}

/// The value of `key=` in a whitespace-token line.
#[must_use]
pub fn kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|tok| {
        tok.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

/// Fill a session's inbox numbers from the two peeked replies: `inbox 0 --peek --meta` (its header counts every unlisted row as `pending=`, and it lists every un-landed post) and `inbox --peek --meta` (every message row).
pub fn fill_inbox(s: &mut LocalSession, header0: &str, posts: &[String], full: &[String]) {
    let num = |k: &str| kv(header0, k).and_then(|v| v.parse::<u64>().ok());
    s.hold = kv(header0, "hold").and_then(|v| v.parse::<u8>().ok());
    s.holder = kv(header0, "holder").unwrap_or("-").to_string();
    s.seen = num("seen");
    s.dropped = num("dropped");
    s.pending = num("pending");
    s.posts = Some(posts.iter().filter(|r| r.starts_with("post ")).count() as u64);
    let seen = s.seen.unwrap_or(0);
    s.unhandled = full
        .iter()
        .filter_map(|r| {
            let mut words = r.split_whitespace();
            if words.next() != Some("msg") {
                return None;
            }
            let id = words.next()?.parse::<u64>().ok()?;
            Some(MsgMeta {
                id,
                off: kv(r, "off")?.parse().ok()?,
                from: kv(r, "from").unwrap_or("-").to_string(),
                kind: kv(r, "kind").unwrap_or("-").to_string(),
            })
        })
        .filter(|m| m.id > seen)
        .collect();
    s.unread = Some((s.unhandled.len() as u64).saturating_sub(s.pending.unwrap_or(0)));
}

/// The `(reason, origins)` of the LAST `kind=hold` transition in a `timeline` reply, when the session was held after it.
#[must_use]
pub fn last_hold(timeline: &[String]) -> Option<(String, String)> {
    let row = timeline
        .iter()
        .rev()
        .find(|r| kv(r, "kind") == Some("hold"))?;
    let words: Vec<&str> = row.split_whitespace().collect();
    let at = words.iter().position(|w| *w == "kind=hold")?;
    if words.get(at + 1) != Some(&"1") {
        return None;
    }
    Some((
        crate::pct::decode(kv(row, "reason").unwrap_or("?")),
        kv(row, "hold_origin")
            .or_else(|| kv(row, "origin"))
            .unwrap_or("?")
            .to_string(),
    ))
}

fn ask(ctl: &mut Ctl, line: &str) -> Result<crate::ctl::Reply, String> {
    let reply = ctl.request(line).map_err(|e| format!("`{line}`: {e}"))?;
    if reply.ok() {
        Ok(reply)
    } else {
        Err(format!("`{line}` answered {}", safe(reply.header(), 160)))
    }
}

fn read_session(ctl: &mut Ctl, sid: &str) -> LocalSession {
    let mut s = LocalSession {
        sid: sid.to_string(),
        role: "-".to_string(),
        holder: "-".to_string(),
        ..LocalSession::default()
    };
    let result = (|| -> Result<(), String> {
        let meta = ask(ctl, &format!("@{sid} meta"))?;
        let meta = meta.header();
        s.title = crate::pct::decode(kv(meta, "title").unwrap_or("-"));
        s.role = crate::pct::decode(kv(meta, "role").unwrap_or("-"));
        let zero = ask(ctl, &format!("@{sid} inbox 0 --peek --meta"))?;
        let full = ask(ctl, &format!("@{sid} inbox --peek --meta"))?;
        fill_inbox(&mut s, zero.header(), zero.rows(), full.rows());
        if s.hold == Some(1) {
            if let Ok(t) = ask(ctl, &format!("@{sid} timeline")) {
                s.hold_why = last_hold(t.rows());
            }
        }
        if let Ok(t) = ask(ctl, &format!("@{sid} topic ls")) {
            s.topics = t
                .rows()
                .iter()
                .filter_map(|row| row.strip_prefix("topic "))
                .filter_map(|rest| rest.split_whitespace().next())
                .map(str::to_string)
                .collect();
        }
        Ok(())
    })();
    if let Err(e) = result {
        s.error = Some(e);
    }
    s
}

fn read_instance(pid: u32, sock: &str, procs: &[Proc], sessions: bool) -> Option<InstanceView> {
    let mut v = InstanceView {
        pid,
        sock: sock.to_string(),
        pid_reaches: aterm_ctl::pid_reaches(pid, sock),
        ..InstanceView::default()
    };
    v.bridge_pids = procs
        .iter()
        .filter(|p| {
            p.ppid == pid
                && pid != 0
                && serve_flags(&aterm_types::fabric_command::split_line(&p.cmd)).is_some()
        })
        .map(|p| p.pid)
        .collect();
    let mut ctl = match open_instance(pid, sock, IO_TIMEOUT) {
        Ok(Some(c)) => c,
        Ok(None) => return None,
        Err(e) => {
            v.error = Some(e);
            return Some(v);
        }
    };
    match ask(&mut ctl, "fabric status") {
        Ok(r) => {
            let h = r.header();
            v.fabric = kv(h, "state").map(str::to_string);
            v.reason = kv(h, "reason").filter(|r| *r != "-").map(str::to_string);
            v.rtt_ms = kv(h, "rtt_ms").and_then(|n| n.parse().ok());
            v.link_age_ms = kv(h, "link_age_ms").and_then(|n| n.parse().ok());
            v.supervised = kv(h, "supervised").map(|s| s == "1");
            v.build = kv(h, "build").filter(|b| *b != "-").map(str::to_string);
            v.bridge_build = kv(h, "bridge_build")
                .filter(|b| *b != "-")
                .map(str::to_string);
            v.lane_faults = kv(h, "lane_faults").and_then(|n| n.parse().ok());
            v.command = status_argv(h);
            v.program = v.command.as_deref().and_then(bridge_program);
        }
        Err(e) => {
            v.error = Some(e);
            return Some(v);
        }
    }
    if let Some(cfg) = v.command.as_deref().and_then(|c| bridge_config(c).ok()) {
        v.node = crate::enable::read_node(Path::new(&cfg.state_dir));
        v.state_dir = Some(cfg.state_dir);
    }
    if !sessions {
        return Some(v);
    }
    let rows = match ask(&mut ctl, "sessions") {
        Ok(r) => r,
        Err(e) => {
            v.error = Some(e);
            return Some(v);
        }
    };
    for sid in session_ids(rows.rows()) {
        v.sessions.push(read_session(&mut ctl, sid));
    }
    Some(v)
}

/// One instance's control socket, opened with its Owner token and bounded by `timeout`.
pub(crate) fn open_instance(
    pid: u32,
    sock: &str,
    timeout: Duration,
) -> Result<Option<Ctl>, String> {
    let (stream, token) = match aterm_ctl::connect_then_read_token(sock) {
        Ok(connected) => connected,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) && !aterm_uds::process::pid_alive(pid) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(format!("connect: {e}")),
    };
    let token = match token {
        Ok(t) => t,
        Err(_) if !aterm_uds::process::pid_alive(pid) => return Ok(None),
        Err(e) => return Err(format!("no Owner token: {e}")),
    };
    let ctl = Ctl::authenticate(stream, &token).map_err(|e| format!("connect: {e}"))?;
    let _ = ctl.get_ref().set_read_timeout(Some(timeout));
    let _ = ctl.get_ref().set_write_timeout(Some(timeout));
    Ok(Some(ctl))
}

/// The session ids in a `sessions` reply's rows (`local <sid> …`), in the instance's order.
pub(crate) fn session_ids(rows: &[String]) -> impl Iterator<Item = &str> {
    rows.iter().filter_map(|row| {
        row.split(' ')
            .nth(1)
            .filter(|s| crate::subject::is_principal(s))
    })
}

/// One SESSIONS row: a local session, a live remote one, or both joined.
#[derive(Debug, Clone, Default)]
pub struct SessionRow {
    /// The session id.
    pub sid: String,
    /// The local instance hosting it, if one here does.
    pub pid: Option<u32>,
    /// The node its presence row (or its instance's bridge) names.
    pub node: String,
    /// The bus presence `state=` as the report READS it — `away` for a `live` row whose node's will fired ([`crate::glance::AWAY`]), `gone` for a retired one still shown — or `-` when the bus has no row for it.
    pub bus: String,
    /// The local ctl view, when a local instance hosts it.
    pub local: Option<LocalSession>,
    /// The presence row's `hold=` (the fleet halt as the node applied it).
    pub bus_hold: Option<String>,
    /// The presence row's `role=`, pct-decoded, when it carries one.
    pub bus_role: Option<String>,
    /// The presence row's `title=`, pct-decoded, when it carries one.
    pub bus_title: Option<String>,
    /// The presence row's `detail=` — the running program — when it carries one.
    pub detail: Option<String>,
    /// The presence row's `phase=`, when it carries one.
    pub phase: Option<String>,
    /// The presence row's `context=` (`<n>%`), when it carries one.
    pub context: Option<String>,
}

fn bus_field(row: &crate::glance::Row, key: &str) -> Option<String> {
    let v = row.field(key);
    (v != crate::glance::ABSENT && !v.is_empty()).then(|| crate::pct::decode(v))
}

/// A presence row of a node (not a session).
#[derive(Debug, Clone, Default)]
pub struct NodeRow {
    /// The node id.
    pub node: String,
    /// Its `host=`.
    pub host: String,
    /// Its `state=` as the node's row carries it (`live`, or `away` once its will fired — `gone` from a bridge that predates the word).
    pub state: String,
    /// Its `fabric=`.
    pub fabric: String,
    /// The astream protocol it speaks (`proto=`), `-` from a node whose bridge predates the token.
    pub proto: String,
    /// The aterm release it runs (`aterm=`), `-` likewise.
    pub aterm: String,
}

impl NodeRow {
    /// The state as the report says it: a will reads [`crate::glance::AWAY`] whichever word the bridge that registered it wrote.
    #[must_use]
    pub fn reads(&self) -> &str {
        if crate::glance::is_will(&self.state) {
            crate::glance::AWAY
        } else {
            &self.state
        }
    }
}

/// The distinct-subject bound one producer — one node — may ever hold (`astream-broker`'s `MAX_SUBJECTS_PER_PRODUCER`).
pub const SUBJECT_BUDGET: usize = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER;

/// How much of [`SUBJECT_BUDGET`] spent is a WARNING: three quarters, while a thousand subjects — some five hundred sessions — are still left.
pub const SUBJECT_BUDGET_WARN: usize = SUBJECT_BUDGET / 4 * 3;

/// Everything `status` found.
#[derive(Debug, Clone)]
pub struct Report {
    /// Where the command came from.
    pub source: Source,
    /// The command itself, as the argv it runs.
    pub command: Vec<String>,
    /// Whether its program lies in a cargo target directory ([`in_cargo_target_dir`]).
    pub program_in_target_dir: bool,
    /// The PROGRAM the command runs ([`bridge_program`]), resolved when the report was read — what each instance's own is compared with.
    pub program: Option<String>,
    /// The config file looked in, if any.
    pub config_path: Option<PathBuf>,
    /// The parsed bridge config.
    pub cfg: Config,
    /// What the bridge's presence rows carry: `--presence` on the command, else `[fabric] presence` in the config file, else `meta`.
    pub presence: crate::presence::Mode,
    /// Whether the bridge publishes receipts (R8): `--receipts`/`--no-receipts` on the command, else `[fabric] receipts` in the config file, else on.
    pub receipts: bool,
    /// The node id in the command's state dir.
    pub node: Option<String>,
    /// `(path, grant count or error)` per cap file.
    pub caps: Vec<(String, Result<usize, String>)>,
    /// The broker probe.
    pub broker: BrokerView,
    /// Every local instance that answered or should have.
    pub instances: Vec<InstanceView>,
    /// Why the local instances could not be listed, if they could not.
    pub discovery_error: Option<String>,
    /// SESSIONS.
    pub sessions: Vec<SessionRow>,
    /// Node presence rows.
    pub nodes: Vec<NodeRow>,
    /// How many presence rows the roster read returned; `None` when it could not be read.
    pub roster_rows: Option<usize>,
    /// Whether the roster read was cut at [`crate::glance::MAX_ROWS`]: past the cut, nodes and sessions exist that no section shows.
    pub roster_truncated: bool,
    /// The distinct subjects THIS node's producer holds on the bus, of [`SUBJECT_BUDGET`] ([`subjects_of`]), or why they could not be counted.
    pub subjects: Option<Result<usize, String>>,
    /// Bus session rows that are not live and not local (not shown in text).
    pub exited: usize,
    /// TRAFFIC, oldest first.
    pub traffic: Vec<Traffic>,
    /// Every `ask`/`task` on the bus past its `dl=` with no reply (R8), each a warning; kept as data for `--json`.
    pub overdue: Vec<Overdue>,
    /// WARNINGS.
    pub warnings: Vec<String>,
    /// When the report was taken, ms since the epoch.
    pub now_ms: u64,
}

/// The fabric is off or its config unreadable: `status` exits 2 with this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Off {
    /// No command in the env, the config, or any running instance.
    NoCommand(Option<PathBuf>),
    /// The config cannot be read, or its command is not a bridge.
    Unreadable(String),
}

impl Off {
    /// The state alone — what `doctor` prints above its own `fix:` line, so the remedy is said once, in the shape every other finding uses.
    #[must_use]
    pub fn state(&self) -> String {
        match self {
            Off::NoCommand(Some(p)) => format!(
                "fabric is off: no [fabric] command in {} (and no rendezvous file)",
                p.display()
            ),
            Off::NoCommand(None) => {
                "fabric is off: no config file (neither $XDG_CONFIG_HOME nor $HOME is set)"
                    .to_string()
            }
            Off::Unreadable(why) => format!("fabric config is unreadable: {why}"),
        }
    }

    /// The one line the operator reads (`status`): the state, and for an off fabric the verb that turns it on.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Off::NoCommand(_) => format!("{}; `aterm fabric on` turns it on", self.state()),
            Off::Unreadable(_) => self.state(),
        }
    }
}

/// Take the whole report.
pub fn gather() -> Result<Report, Off> {
    let now_ms = crate::now_ms();
    let config_path = config_path();
    let env = command_seam();
    let resolved =
        match resolve_command(env.as_deref(), config_path.as_deref()).map_err(Off::Unreadable)? {
            Some(found) => Some(found),
            None => rendezvous_command().map_err(Off::Unreadable)?,
        };
    let procs = process_table();

    let (listed, discovery_error) = match aterm_ctl::local_instances() {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e.reason)),
    };
    let instances: Vec<InstanceView> = listed
        .iter()
        .filter_map(|(pid, sock)| read_instance(*pid, sock, &procs, true))
        .collect();

    let (source, command) = match resolved {
        Some(found) => found,
        None => instances
            .iter()
            .find(|i| i.supervised == Some(true) && i.command.is_some())
            .map(|i| {
                (
                    Source::Instance(i.pid),
                    i.command.clone().unwrap_or_default(),
                )
            })
            .ok_or_else(|| Off::NoCommand(config_path.clone()))?,
    };
    let cfg = bridge_config(&command)
        .map_err(|e| Off::Unreadable(format!("{}: {e}", source.describe())))?;
    let presence = if serve_flags(&command).is_some_and(|f| f.iter().any(|a| a == "--presence")) {
        cfg.presence
    } else {
        presence_from_config()
    };
    let receipts = if serve_flags(&command)
        .is_some_and(|f| f.iter().any(|a| a == "--receipts" || a == "--no-receipts"))
    {
        cfg.receipts
    } else {
        receipts_from_config()
    };
    let node = crate::enable::read_node(Path::new(&cfg.state_dir));
    let caps = cfg
        .cap_files
        .iter()
        .map(|p| {
            (
                p.clone(),
                read_cap_file(p).map(|c| c.len()).map_err(|e| e.to_string()),
            )
        })
        .collect();

    let (broker, conn) = probe_broker(&cfg, &procs);
    let program_in_target_dir = command
        .first()
        .is_some_and(|p| in_cargo_target_dir(Path::new(&resolve_program(p))));
    let program = bridge_program(&command);
    let mut report = Report {
        source,
        command,
        program_in_target_dir,
        program,
        config_path,
        cfg,
        presence,
        receipts,
        node,
        caps,
        broker,
        instances,
        discovery_error,
        sessions: Vec::new(),
        nodes: Vec::new(),
        roster_rows: None,
        roster_truncated: false,
        subjects: None,
        exited: 0,
        traffic: Vec::new(),
        overdue: Vec::new(),
        warnings: Vec::new(),
        now_ms,
    };

    let mut ages: BTreeMap<(String, u64), u64> = BTreeMap::new();
    let mut roster = None;
    let mut halts = Vec::new();
    let mut bus_errors = Vec::new();
    if let (Some(mut conn), Some(head)) = (conn, report.broker.head) {
        match crate::glance::read(&mut conn, &report.cfg.fleet) {
            Ok(g) => {
                report.roster_rows = Some(g.rows.len());
                report.roster_truncated = g.truncated;
                roster = Some(g);
            }
            Err(e) => bus_errors.push(format!("the presence roster could not be read: {e}")),
        }
        if let Some(node) = report.node.clone() {
            report.subjects =
                Some(subjects_of(&mut conn, &report.cfg.fleet, &node).map_err(|e| e.to_string()));
        }
        let faces = report.broker.faces.clone();
        match last_records(
            &mut conn,
            &report.cfg.fleet,
            faces.as_deref(),
            head,
            TRAFFIC_ROWS,
        ) {
            Ok(t) => report.traffic = t,
            Err(e) => bus_errors.push(format!("the traffic could not be read: {e}")),
        }
        halts = fleet_halts(&mut conn, &report.cfg.fleet);
        match overdue_work(&mut conn, &report.cfg.fleet, faces.as_deref(), head, now_ms) {
            Ok(o) => report.overdue = o,
            Err(e) => bus_errors.push(format!("the deadlines could not be read: {e}")),
        }
        let in_filters = in_filters(&report.cfg.fleet, faces.as_deref());
        for inst in &report.instances {
            for s in &inst.sessions {
                for m in s
                    .unhandled
                    .iter()
                    .filter(|m| WORK_KINDS.contains(&m.kind.as_str()))
                {
                    if let Some(t) = in_filters
                        .iter()
                        .find_map(|f| record_t(&mut conn, f, m.off))
                    {
                        ages.insert((s.sid.clone(), m.off), t);
                    }
                }
            }
        }
    }
    join_sessions(&mut report, roster.as_ref());
    report.warnings = warnings(&report, &ages, &halts);
    report.warnings.extend(bus_errors);
    Ok(report)
}

fn in_cargo_target_dir(program: &Path) -> bool {
    program.is_absolute()
        && program
            .ancestors()
            .skip(1)
            .any(|d| d.join("CACHEDIR.TAG").is_file())
}

/// Count distinct subjects `node`'s producer holds in the broker's last-value index across `/f/<fleet>/pub/<node>/>` and `/f/<fleet>/in/*/*/<node>/*`, bounded by [`SUBJECT_BUDGET`].
pub(crate) fn subjects_of(conn: &mut Conn, fleet: &str, node: &str) -> io::Result<usize> {
    let mut n = 0usize;
    for filter in [
        format!("/f/{fleet}/pub/{node}/>"),
        format!("/f/{fleet}/in/*/*/{node}/*"),
    ] {
        let left = u32::try_from(SUBJECT_BUDGET.saturating_sub(n)).unwrap_or(u32::MAX);
        if left == 0 {
            break;
        }
        conn.last_walk(&filter, "", left, |_| {
            n += 1;
            Ok(astream_broker::Walk::Continue)
        })?;
    }
    Ok(n)
}

fn fleet_halts(conn: &mut Conn, fleet: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let filter = format!("/f/{fleet}/fleet/*/halt");
    let _ = transport::walk_last(conn, &filter, |(_, subject, raw)| {
        if let Some(row) = crate::subject::parse_halt(fleet, subject, raw).filter(|r| r.on) {
            out.push((
                safe(row.human, 64),
                crate::pct::decode(row.reason.as_deref().unwrap_or("-")),
            ));
        }
        Ok(())
    });
    out
}

const GONE_SHOW_MS: u64 = 60 * 60 * 1000;

fn join_sessions(report: &mut Report, roster: Option<&crate::glance::Glance>) {
    let mut bus: BTreeMap<String, Vec<&crate::glance::Row>> = BTreeMap::new();
    if let Some(g) = roster {
        for row in &g.rows {
            if row.owner == "node" {
                report.nodes.push(NodeRow {
                    node: row.node.clone(),
                    host: crate::pct::decode(row.field("host")),
                    state: row.field("state").to_string(),
                    fabric: row.field("fabric").to_string(),
                    proto: row.field("proto").to_string(),
                    aterm: crate::pct::decode(row.field("aterm")),
                });
            } else {
                bus.entry(row.owner.clone()).or_default().push(row);
            }
        }
    }
    let away: BTreeSet<String> = report
        .nodes
        .iter()
        .filter(|n| crate::glance::is_will(&n.state))
        .map(|n| n.node.clone())
        .collect();
    let reads = |row: &crate::glance::Row| -> String {
        let state = row.field("state");
        if state == "live" && away.contains(&row.node) {
            crate::glance::AWAY.to_string()
        } else {
            state.to_string()
        }
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for inst in &report.instances {
        let node = inst.node.clone().or_else(|| report.node.clone());
        for s in &inst.sessions {
            let rows = bus.get(&s.sid);
            let row = rows.and_then(|rs| {
                rs.iter()
                    .find(|r| Some(&r.node) == node.as_ref())
                    .or_else(|| rs.first())
            });
            seen.insert(s.sid.clone());
            report.sessions.push(SessionRow {
                sid: s.sid.clone(),
                pid: Some(inst.pid),
                node: row
                    .map(|r| r.node.clone())
                    .or_else(|| node.clone())
                    .unwrap_or_else(|| "-".to_string()),
                bus: row.map_or_else(|| "-".to_string(), |r| reads(r)),
                local: Some(s.clone()),
                ..bus_fields(row.copied())
            });
        }
    }
    let local_nodes: BTreeSet<String> = report
        .instances
        .iter()
        .filter_map(|i| i.node.clone())
        .chain(report.node.clone())
        .collect();
    let now = crate::now_ms();
    for (sid, rows) in &bus {
        if seen.contains(sid) {
            continue;
        }
        for row in rows {
            let live = row.field("state") == "live";
            if !live {
                let recent = row
                    .field("t")
                    .parse::<u64>()
                    .ok()
                    .is_some_and(|t| now.saturating_sub(t) <= GONE_SHOW_MS);
                if !(recent && local_nodes.contains(&row.node)) {
                    report.exited += 1;
                    continue;
                }
            }
            report.sessions.push(SessionRow {
                sid: sid.clone(),
                node: row.node.clone(),
                bus: if live { reads(row) } else { "gone".to_string() },
                ..bus_fields(Some(*row))
            });
        }
    }
}

fn bus_fields(row: Option<&crate::glance::Row>) -> SessionRow {
    SessionRow {
        bus_hold: row.map(|r| r.field("hold").to_string()),
        bus_role: row.and_then(|r| bus_field(r, "role")),
        bus_title: row.and_then(|r| bus_field(r, "title")),
        detail: row.and_then(|r| bus_field(r, "detail")),
        phase: row.and_then(|r| bus_field(r, "phase")),
        context: row.and_then(|r| bus_field(r, "context")),
        ..SessionRow::default()
    }
}

/// A duration, the way a human says it: `42s`, `7m`, `3h`, `2d`.
#[must_use]
pub fn age(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn warnings(
    r: &Report,
    ages: &BTreeMap<(String, u64), u64>,
    halts: &[(String, String)],
) -> Vec<String> {
    let mut w = Vec::new();
    let broker = safe(&r.cfg.broker, 256);

    if let Source::Instance(pid) = r.source {
        let path = r
            .config_path
            .as_ref()
            .map_or_else(|| "aterm.toml".to_string(), |p| p.display().to_string());
        w.push(format!(
            "no [fabric] command in {path}: this report read instance {pid}'s running bridge \
             instead, and the next aterm launch starts none"
        ));
    }
    if r.node.is_none() {
        w.push(format!(
            "no node id in {}/node: the bridge has no identity to answer mail as",
            safe(&r.cfg.state_dir, 256)
        ));
    }
    for (path, caps) in &r.caps {
        if let Err(e) = caps {
            w.push(format!("cap file {}: {}", safe(path, 256), safe(e, 256)));
        }
    }

    if let Some(skew) = r.broker.protocol {
        w.push(format!(
            "the broker at {broker} speaks astream protocol {} and this aterm speaks {}: \
             neither can read what the other sends, so no record lands and none is delivered",
            skew.broker, skew.client
        ));
    } else if !r.broker.reachable {
        let last = r.broker.last_word.as_deref().map_or_else(String::new, |l| {
            format!("; its supervisor's broker.err, {}", safe(l, 256))
        });
        w.push(format!(
            "the broker at {broker} does not answer ({}): no record lands and none is \
             delivered{last}",
            safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
        ));
    } else if r.broker.cap_expired {
        w.push(format!(
            "the broker at {broker} {CAP_LAPSED} (its grants' `exp=` has passed): no record of \
             this node lands and none is delivered to it — {}",
            safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
        ));
    } else if !r.broker.read_ok() {
        w.push(format!(
            "the broker at {broker} answers but the bus could not be read: {}",
            safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
        ));
    }
    if let (Some(pid), Some(exe)) = (r.broker.pid, &r.broker.replaced) {
        w.push(format!(
            "the broker (pid {pid}) runs a replaced build: {} is another file than the one it \
             started from, so the broker-side fixes of the build on disk are not running",
            safe(exe, 256)
        ));
    }
    if r.broker.guarded == Some(false) {
        w.push(format!(
            "the broker at {broker} checks no capability (it accepted one it cannot verify): \
             any process that reaches it can publish as any node or human, and set or lift any \
             fleet halt"
        ));
    }
    if let Some(why) = r.broker.poisoned() {
        w.push(format!(
            "the broker at {broker} {POISONED_WARNING} ({}): it refuses every write until the \
             log is reopened, so no record lands; a broker of this build exits within seconds \
             for its supervisor to reopen it",
            safe(why, 256)
        ));
    } else if let Some(failed) = r
        .broker
        .health
        .as_ref()
        .map(|h| h.flushes.failed)
        .filter(|n| *n > 0)
    {
        w.push(format!(
            "the broker at {broker} {FLUSH_FAILED_WARNING} {failed} time(s) since it opened: \
             the disk under {} is failing writes, and the log was not poisoned when this asked \
             (a failed flush that put acked records at risk poisons it, and the next report \
             says so)",
            safe(
                &r.broker
                    .log
                    .as_ref()
                    .map_or_else(|| "its log".to_string(), |l| l.display().to_string()),
                256
            )
        ));
    }

    if let Some(e) = &r.discovery_error {
        w.push(format!(
            "cannot list this machine's aterm instances: {}",
            safe(e, 512)
        ));
    }
    let config_flags = serve_flags(&r.command);
    for inst in &r.instances {
        let pid = inst.pid;
        if let Some(e) = &inst.error {
            w.push(format!(
                "instance {pid} did not answer ({}): its bridge and sessions are not in \
                 this report",
                safe(e, 256)
            ));
            continue;
        }
        let own = inst.command.as_deref().and_then(|c| bridge_config(c).ok());
        let same_broker = own.as_ref().is_none_or(|c| c.broker == r.cfg.broker);
        match (inst.fabric.as_deref(), inst.supervised) {
            (_, Some(false)) => w.push(format!(
                "instance {pid} has no bridge (fabric={}, not supervised): its sessions \
                 receive no mail and their posts only queue",
                safe(inst.fabric.as_deref().unwrap_or("-"), 32)
            )),
            (Some("disconnected"), _) => w.push(format!(
                "instance {pid}'s bridge is down (fabric=disconnected): its sessions get no \
                 mail and their posts queue; aterm relaunches the bridge"
            )),
            (Some("stalled"), _) if inst.reason.as_deref() == Some("state-locked") => {
                let holder = r
                    .instances
                    .iter()
                    .find(|o| {
                        o.pid != pid
                            && o.state_dir.is_some()
                            && o.state_dir == inst.state_dir
                            && o.reason.as_deref() != Some("state-locked")
                    })
                    .map_or_else(
                        || "another bridge".to_string(),
                        |o| format!("instance {}'s bridge", o.pid),
                    );
                w.push(format!(
                    "instance {pid}'s bridge stands by (fabric=stalled reason=state-locked): \
                     {holder} holds its state dir {}, and one state dir is one node — its \
                     sessions receive no mail and no fleet halt, and their posts queue until \
                     that bridge exits",
                    safe(inst.state_dir.as_deref().unwrap_or("-"), 256)
                ));
            }
            (Some("stalled"), _) if inst.reason.as_deref() == Some("starting") => {
                w.push(format!(
                    "instance {pid}'s bridge is attached and has not reported its broker link \
                     (fabric=stalled reason=starting); if this stays, check the broker at \
                     {broker}"
                ));
            }
            (Some("stalled"), _) => {
                let reason = inst.reason.as_deref().unwrap_or("-");
                let last = match (reason, r.broker.last_word.as_deref()) {
                    ("no-socket" | "refused", Some(l)) if same_broker && !r.broker.reachable => {
                        format!("; its supervisor's broker.err, {}", safe(l, 256))
                    }
                    _ => String::new(),
                };
                w.push(format!(
                    "instance {pid}'s bridge is attached but its broker link is down \
                     (fabric=stalled reason={}, last ack {}): its posts queue and no mail \
                     arrives from the broker at {broker}{last}",
                    safe(reason, 32),
                    inst.link_age_ms
                        .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
                ));
            }
            (Some("stale"), _) => w.push(format!(
                "instance {pid}'s bridge has not answered for the work it was given \
                 (fabric=stale, last ack {}): its posts queue and `post --wait` from its \
                 sessions fails fast",
                inst.link_age_ms
                    .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
            )),
            (Some("connected"), _) if !r.broker.reachable && same_broker => w.push(format!(
                "instance {pid} says fabric=connected (its bridge's last ack was {}), but the \
                 broker at {broker} does not answer this report's own probe: either the broker \
                 died since that ack and the bridge has not exchanged a record with it yet, \
                 or the two are not looking at the same socket",
                inst.link_age_ms
                    .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
            )),
            _ => {}
        }
        if let (Some(theirs), Some(ours)) = (&inst.bridge_build, &inst.build) {
            if theirs != ours {
                w.push(format!(
                    "instance {pid}'s bridge is another build than the instance: bridge {}, \
                     instance {}",
                    safe(theirs, 64),
                    safe(ours, 64)
                ));
            }
        }
        if let Some(n) = inst.lane_faults.filter(|n| *n > 0) {
            w.push(format!(
                "instance {pid} could not parse {n} of its bridge's lines (lane_faults={n}): \
                 the mail they carried is held back on the bus, not lost"
            ));
        }
        let program_differs = inst.program.is_some() && inst.program != r.program;
        if inst.supervised == Some(true)
            && (inst.command.as_deref().and_then(serve_flags) != config_flags || program_differs)
            && r.source != Source::Instance(pid)
        {
            let differs = own.as_ref().map_or_else(
                || "it cannot be parsed as a bridge".to_string(),
                |c| {
                    let mut d = Vec::new();
                    if program_differs {
                        d.push(format!(
                            "program {} (not {})",
                            safe(inst.program.as_deref().unwrap_or("-"), 256),
                            safe(r.program.as_deref().unwrap_or("-"), 256)
                        ));
                    }
                    for (flag, theirs, ours) in [
                        ("--fleet", &c.fleet, &r.cfg.fleet),
                        ("--broker", &c.broker, &r.cfg.broker),
                        ("--state", &c.state_dir, &r.cfg.state_dir),
                    ] {
                        if theirs != ours {
                            d.push(format!(
                                "{flag} {} (not {})",
                                safe(theirs, 256),
                                safe(ours, 256)
                            ));
                        }
                    }
                    if d.is_empty() {
                        "other flags differ".to_string()
                    } else {
                        format!("it runs {}", d.join(", "))
                    }
                },
            );
            w.push(format!(
                "instance {pid}'s bridge was armed with a different command than {}: \
                 {differs} — an instance records its command at launch or `fabric attach`, \
                 so this report and that bridge are not describing the same fabric",
                r.source.describe()
            ));
        }
        if inst.fabric.as_deref() == Some("connected") {
            if let Some(row) = inst
                .node
                .as_deref()
                .and_then(|n| r.nodes.iter().find(|row| row.node == n))
            {
                if row.state != "live" || row.fabric != "connected" {
                    w.push(format!(
                        "instance {pid} says fabric=connected, but the bus's own presence \
                         for its node {} says state={} fabric={}: every peer reading the \
                         fleet's roster sees this node away",
                        safe(row.node.as_str(), 64),
                        safe(row.reads(), 16),
                        safe(&row.fabric, 16)
                    ));
                }
            }
        }
        let bridge_ok = inst.fabric.as_deref() == Some("connected") && r.broker.reachable;
        for s in &inst.sessions {
            let at = format!("@{} (instance {pid})", s.sid);
            if let Some(e) = &s.error {
                w.push(format!("{at}: {}", safe(e, 256)));
                continue;
            }
            if let Some(n) = s.dropped.filter(|n| *n > 0) {
                w.push(format!(
                    "{at} lost {n} message(s): its inbox ring evicted rows nobody handled \
                     (dropped={n})"
                ));
            }
            if s.hold == Some(1) {
                let why = s.hold_why.as_ref().map_or_else(
                    || "reason and origin no longer in its timeline".to_string(),
                    |(reason, origin)| {
                        format!("reason={} origin={}", safe(reason, 128), safe(origin, 16))
                    },
                );
                let at = if inst.pid_reaches {
                    at.clone()
                } else {
                    format!(
                        "@{} (instance {pid}, --sock {})",
                        s.sid,
                        safe(&inst.sock, 256)
                    )
                };
                w.push(format!(
                    "{at} is HELD ({why}): every key and turn verb answers ERR halted"
                ));
            }
            let stale: Vec<(&MsgMeta, u64)> = s
                .unhandled
                .iter()
                .filter(|m| WORK_KINDS.contains(&m.kind.as_str()))
                .filter_map(|m| {
                    let t = *ages.get(&(s.sid.clone(), m.off))?;
                    let old = r.now_ms.saturating_sub(t);
                    (t > 0 && old > STALE_WORK_MS).then_some((m, old))
                })
                .collect();
            if let Some((oldest, old)) = stale.iter().max_by_key(|(_, old)| *old) {
                w.push(format!(
                    "{at} has {} unhandled task/ask older than 10 min — the oldest, {} from \
                     {}, is {} old",
                    stale.len(),
                    safe(&oldest.kind, 16),
                    safe(&oldest.from, 128),
                    age(*old)
                ));
            }
            if let Some(n) = s.posts.filter(|n| *n > 0) {
                if !bridge_ok {
                    w.push(format!(
                        "{at} has {n} queued post(s) and no working bridge to publish them"
                    ));
                }
            }
        }
    }
    for o in &r.overdue {
        w.push(format!(
            "{} off={} from {} to @{} passed its deadline {} ago (dl={} ms) with no answer, \
             report or ack — {}",
            safe(&o.kind, 16),
            o.off,
            safe(&o.from, 128),
            safe(&o.to, 128),
            age(o.late_ms),
            o.dl,
            if o.expired {
                "the asker's bridge recorded it expired"
            } else {
                "not yet recorded expired by the asker's bridge"
            }
        ));
    }
    let answered = every_instance_answered(r);
    let local_nodes = local_nodes(&r.instances, r.node.as_deref());
    if answered {
        for s in ghost_rows(&r.sessions, &local_nodes) {
            w.push(format!(
                "the bus advertises @{} {} on {} — this machine's node — but no \
                 aterm instance here hosts it: mail to it is undeliverable",
                safe(&s.sid, 64),
                safe(&s.bus, 16),
                safe(&s.node, 64)
            ));
        }
    }
    for (who, reason) in halts {
        w.push(format!(
            "{who} has a fleet halt standing (reason={}): every session on the fleet \
             is held",
            safe(reason, 128)
        ));
    }
    if r.roster_truncated {
        w.push(format!(
            "the presence roster has more than {} rows and the read stopped there \
             (rows={}+ truncated): NODES, SESSIONS and every warning drawn from them leave \
             out what lies past it",
            crate::glance::MAX_ROWS,
            crate::glance::MAX_ROWS
        ));
    }
    if let (Some(Ok(used)), Some(node)) = (&r.subjects, r.node.as_deref()) {
        if *used >= SUBJECT_BUDGET_WARN {
            w.push(format!(
                "node {} holds {used} of the {SUBJECT_BUDGET} distinct subjects one node may \
                 ever mint, and none but a closed session's own `ev` face (while the session-log \
                 mirror runs) is ever given back: past the bound a new session gets no presence \
                 row and its posts are refused; the remedy is a new node id",
                safe(node, 64)
            ));
        }
    }
    let kept = if r.cfg.state_dir.is_empty() {
        BTreeMap::new()
    } else {
        crate::state::StateDir::peek(&r.cfg.state_dir).standing_halts(&r.cfg.fleet)
    };
    for (who, halt) in kept.into_iter().filter(|(_, h)| h.kept) {
        w.push(format!(
            "{}'s fleet halt (reason={}) stands on this node with no row on the bus: it was \
             read on a bus log since replaced, and only {0}'s own `state=off` on this log \
             lifts it",
            safe(&who, 64),
            safe(&crate::pct::decode(&halt.reason), 128)
        ));
    }
    w
}

/// The local clock's offset from UTC in seconds, from `date +%z`; `None` when it cannot be run, and then every time prints in UTC and says so.
pub(crate) fn local_offset() -> Option<i64> {
    static OFFSET: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = aterm_uds::spawnfd::command("date")
            .arg("+%z")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        parse_utc_offset(String::from_utf8_lossy(&out.stdout).trim())
    })
}

/// `+0200` / `-0700` as seconds.
#[must_use]
pub fn parse_utc_offset(z: &str) -> Option<i64> {
    let (sign, digits) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hh: i64 = digits[..2].parse().ok()?;
    let mm: i64 = digits[2..].parse().ok()?;
    Some(sign * (hh * 3600 + mm * 60))
}

/// A record's time at the clock's offset: `HH:MM:SS` within a day of `now`, `MM-DDTHH:MM` beyond it, `-` when the record carried none.
#[must_use]
pub fn clock(t_ms: u64, now_ms: u64, offset: i64) -> String {
    if t_ms == 0 {
        return "-".to_string();
    }
    let secs = i64::try_from(t_ms / 1000).unwrap_or(i64::MAX / 2) + offset;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    if now_ms.abs_diff(t_ms) < 20 * 3600 * 1000 {
        format!("{hh:02}:{mm:02}:{ss:02}")
    } else {
        let (_, mo, d) = aterm_types::rfc3339::civil_from_days(days);
        format!("{mo:02}-{d:02}T{hh:02}:{mm:02}")
    }
}

/// How the times in this report are zoned, for the header.
pub(crate) fn zone_label(offset: Option<i64>) -> String {
    match offset {
        None => "times UTC".to_string(),
        Some(o) => {
            let sign = if o < 0 { '-' } else { '+' };
            let o = o.abs();
            format!("times UTC{sign}{:02}:{:02}", o / 3600, (o % 3600) / 60)
        }
    }
}

fn table(out: &mut String, header: &[&str], rows: &[Vec<String>], right: &[bool]) {
    let n = header.len();
    let mut w: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate().take(n) {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let mut line = |cells: &[String]| {
        let mut s = String::from("  ");
        for (i, c) in cells.iter().enumerate().take(n) {
            let pad = w[i].saturating_sub(c.chars().count());
            if i + 1 == n {
                s.push_str(c);
            } else if right.get(i).copied().unwrap_or(false) {
                s.push_str(&" ".repeat(pad));
                s.push_str(c);
                s.push_str("  ");
            } else {
                s.push_str(c);
                s.push_str(&" ".repeat(pad + 2));
            }
        }
        out.push_str(s.trim_end());
        out.push('\n');
    };
    line(&header.iter().map(|h| (*h).to_string()).collect::<Vec<_>>());
    for r in rows {
        line(r);
    }
}

fn num(v: Option<u64>) -> String {
    v.map_or_else(|| "-".to_string(), |n| n.to_string())
}

fn traffic_cells(t: &Traffic, now_ms: u64, offset: i64) -> Vec<String> {
    vec![
        format!("@{}", t.off),
        clock(t.t_ms, now_ms, offset),
        t.kind.clone(),
        t.len.to_string(),
        t.trust.to_string(),
        t.from.clone(),
        t.to.clone(),
    ]
}

const TRAFFIC_HEADER: [&str; 7] = ["OFFSET", "TIME", "KIND", "LEN", "TRUST", "FROM", "TO"];

fn session_role(s: &SessionRow) -> &str {
    s.local
        .as_ref()
        .map(|l| l.role.as_str())
        .filter(|r| *r != "-")
        .or(s.bus_role.as_deref())
        .unwrap_or("-")
}

fn session_title(s: &SessionRow) -> &str {
    s.local
        .as_ref()
        .map(|l| l.title.as_str())
        .filter(|t| *t != "-" && !t.is_empty())
        .or(s.bus_title.as_deref())
        .unwrap_or("-")
}

/// Which node a NODES row is, from where this report stands: `this` — the node the command's state dir names; `local` — another instance on this machine answers as it (its own state dir); `remote` — no instance here answers as it: on this one-machine bus, an unhosted or ghost node of this machine (the module docs, NODES), never another host.
#[must_use]
pub fn node_where(r: &Report, node: &str) -> &'static str {
    if r.node.as_deref() == Some(node) {
        "this"
    } else if r.instances.iter().any(|i| i.node.as_deref() == Some(node)) {
        "local"
    } else {
        "remote"
    }
}

fn report_node(report: &Report) -> io::Result<&str> {
    report
        .node
        .as_deref()
        .ok_or_else(|| io::Error::other("this state dir has no node id"))
}

fn connect_attached(cfg: &Config) -> io::Result<Conn> {
    let (mut conn, _closer) = transport::connect(&cfg.broker, IO_TIMEOUT)?;
    transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
    Ok(conn)
}

/// Publish `state=exited` for each `(sid, node)` by rewriting its existing presence row (keeping `inc=`, `epoch=`, `gen=` intact) under [`crate::state::StateDir::lock_bridge`] and reserving sequences from the current incarnation.
pub fn publish_exited(report: &Report, ghosts: &[(String, String)]) -> io::Result<usize> {
    let cfg = &report.cfg;
    let node = report_node(report)?;
    let state = crate::state::StateDir::open(&cfg.state_dir)?;
    let _held = state.lock_bridge().map_err(|e| {
        if e.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(
                e.kind(),
                format!("{e}: a running bridge retires these itself — re-run so it is asked"),
            )
        } else {
            e
        }
    })?;
    let mut conn = connect_attached(cfg)?;
    let mut live: BTreeMap<String, String> = BTreeMap::new();
    let filter = format!("/f/{}/pub/{}/*/presence", cfg.fleet, node);
    transport::walk_last(&mut conn, &filter, |(_, subject, raw)| {
        if let Some(sid) = subject.split('/').nth(5) {
            live.insert(sid.to_string(), String::from_utf8_lossy(raw).into_owned());
        }
        Ok(())
    })?;

    let producer_id = astream_cap::producer_id_of(node);
    let inc = state.incarnation();
    let mut done = 0;
    for (sid, _) in ghosts {
        let Some(body) = live.get(sid) else { continue };
        let rewritten = retired_body(body);
        let subject = crate::subject::session_face(&cfg.fleet, node, sid, "presence");
        let seq = state.reserve_next_sequence(inc)?;
        let (off, deduped) = conn.publish(producer_id, seq, &subject, rewritten.as_bytes())?;
        if deduped {
            return Err(io::Error::other(format!(
                "the broker deduped the retirement of @{sid} at @{off}: this node's \
                 sequence {seq} was already used under its producer id — by a copy of this \
                 state dir, a holder of the node cap, or this node before a power loss rolled \
                 its sequence file back (start the instance once, so its bridge moves to a \
                 new incarnation, and run this again)"
            )));
        }
        done += 1;
    }
    Ok(done)
}

/// What [`retire_progress`] read back.
#[derive(Debug, Default)]
pub struct RetireProgress {
    /// The `state=` of each named session's presence row under the node asked about; a sid with no row is absent.
    pub states: BTreeMap<String, String>,
    /// Why the bridge refused each named session it refused.
    pub refused: BTreeMap<String, String>,
    /// Whether the node's `ev` face was read through to its head, so an empty `refused` entry means the bridge had refused nothing.
    pub refusals_read: bool,
}

/// What the bus says of each named session of `node` since the doctor asked that node's bridge to retire it, read with the cap files' grants (a node's ring reads every member's `pub/` faces): the `state=` of its presence row under `node`, and why the bridge refused it, for every `presence-retire-refused` on `node`'s `ev` face at or after offset `since`.
pub fn retire_progress(
    report: &Report,
    node: &str,
    sids: &[&str],
    since: Option<u64>,
) -> io::Result<RetireProgress> {
    let cfg = &report.cfg;
    let mut conn = connect_attached(cfg)?;
    let mut out = BTreeMap::new();
    let filter = format!("/f/{}/pub/{}/*/presence", cfg.fleet, node);
    transport::walk_last(&mut conn, &filter, |(_, subject, raw)| {
        if let Some(sid) = subject.split('/').nth(5) {
            if sids.contains(&sid) {
                let (body, _) = Body::decode(raw);
                if let Some(state) = body.unknown.get("state") {
                    out.insert(sid.to_string(), state.clone());
                }
            }
        }
        Ok(())
    })?;
    let mut refused = BTreeMap::new();
    let mut refusals_read = false;
    if let Some(since) = since {
        let ev = crate::subject::node_face(&cfg.fleet, node, "ev");
        let mut next = since;
        while let Ok(((page, (after, now_head)), _)) =
            transport::fetch_kept(&mut conn, next, &ev, transport::ROW_PAGE)
        {
            for (_, _, raw) in &page {
                let (body, _) = Body::decode(raw);
                let Some(ev) = body.unknown.get("ev") else {
                    continue;
                };
                if let Some((sid, why)) = refused_retire_event(&crate::pct::decode(ev)) {
                    if sids.contains(&sid.as_str()) {
                        refused.insert(sid, why);
                    }
                }
            }
            if after <= next || after >= now_head {
                refusals_read = after >= now_head || next >= now_head;
                break;
            }
            next = after;
        }
    }
    Ok(RetireProgress {
        states: out,
        refused,
        refusals_read,
    })
}

/// A bridge's `presence-retire-refused sid=<sid> reason=<r> [pid=<p>]` event as `(sid, why)`, the why in words; `None` for any other event.
#[must_use]
pub fn refused_retire_event(ev: &str) -> Option<(String, String)> {
    let rest = ev.strip_prefix("presence-retire-refused ")?;
    let sid = kv(rest, "sid")?;
    let why = match (kv(rest, "reason"), kv(rest, "pid")) {
        (Some("hosted"), Some(pid)) => format!("the instance at pid {} hosts it", safe(pid, 16)),
        (Some("hosted"), None) => "an instance on this node hosts it".to_string(),
        (Some("unaskable"), _) => "an instance on this node could not be asked".to_string(),
        (reason, _) => format!("reason={}", safe(reason.unwrap_or("-"), 32)),
    };
    Some((sid.to_string(), why))
}

/// One presence body with `state=` set to `exited` and `t=` refreshed; every other token kept in place.
pub(crate) fn retired_body(body: &str) -> String {
    let now = crate::now_ms().to_string();
    let mut out: Vec<String> = Vec::new();
    let (mut saw_state, mut saw_t) = (false, false);
    for tok in body.split_whitespace() {
        if let Some(rest) = tok.strip_prefix("state=") {
            let _ = rest;
            saw_state = true;
            out.push("state=exited".to_string());
        } else if tok.starts_with("t=") {
            saw_t = true;
            out.push(format!("t={now}"));
        } else {
            out.push(tok.to_string());
        }
    }
    if !saw_state {
        out.push("state=exited".to_string());
    }
    if !saw_t {
        out.push(format!("t={now}"));
    }
    out.join(" ")
}

/// Whether every local instance was reached and answered.
pub(crate) fn every_instance_answered(r: &Report) -> bool {
    r.discovery_error.is_none() && r.instances.iter().all(|i| i.error.is_none())
}

/// Which instances did not answer, as `pid: why`, for a refusal that names them.
pub(crate) fn unanswered(r: &Report) -> Vec<String> {
    let mut out: Vec<String> = r
        .instances
        .iter()
        .filter_map(|i| {
            i.error
                .as_ref()
                .map(|e| format!("instance {}: {}", i.pid, safe(e, 160)))
        })
        .collect();
    if let Some(e) = &r.discovery_error {
        out.push(format!(
            "the local instances could not be listed: {}",
            safe(e, 160)
        ));
    }
    out
}

/// THE GHOSTS: presence rows this machine's node advertises LIVE that no local instance hosts — `away` ones included, since a local node whose will fired with no instance here left exactly these behind (a crash, a `kill -9`).
pub(crate) fn ghost_rows<'r>(
    sessions: &'r [SessionRow],
    local: &BTreeSet<&str>,
) -> Vec<&'r SessionRow> {
    sessions
        .iter()
        .filter(|s| s.pid.is_none() && advertised(&s.bus) && local.contains(s.node.as_str()))
        .collect()
}

/// Every session any local instance hosts, by pid, read as the report reads it (each instance's `sessions`).
pub(crate) fn node_hosted() -> Result<BTreeMap<String, u32>, String> {
    let listed = aterm_ctl::local_instances().map_err(|e| e.reason)?;
    let mut hosted = BTreeMap::new();
    for (pid, sock) in listed {
        let Some(mut ctl) =
            open_instance(pid, &sock, IO_TIMEOUT).map_err(|e| format!("instance {pid}: {e}"))?
        else {
            continue;
        };
        let reply = ask(&mut ctl, "sessions").map_err(|e| format!("instance {pid}: {e}"))?;
        for sid in session_ids(reply.rows()) {
            hosted.insert(sid.to_string(), pid);
        }
    }
    Ok(hosted)
}

/// Why a presence row may not be retired on an operator's request: the door the request came through is one instance's, the ghost list is the node's.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RetireRefusal {
    /// A local instance hosts the session.
    Hosted(u32),
    /// The node's hosted set could not be read, so nobody can say it is a ghost.
    Unaskable(String),
}

/// The refusal for `sid` against the node's hosted set, or `None`.
pub(crate) fn retire_refusal(
    sid: &str,
    hosted: &Result<BTreeMap<String, u32>, String>,
) -> Option<RetireRefusal> {
    match hosted {
        Err(e) => Some(RetireRefusal::Unaskable(e.clone())),
        Ok(h) => h.get(sid).map(|pid| RetireRefusal::Hosted(*pid)),
    }
}

/// The nodes this machine speaks for: every local instance's, plus this state dir's own.
pub(crate) fn local_nodes<'r>(
    instances: &'r [InstanceView],
    node: Option<&'r str>,
) -> BTreeSet<&'r str> {
    instances
        .iter()
        .filter_map(|i| i.node.as_deref())
        .chain(node)
        .collect()
}

/// Whether a SESSIONS row's `bus` is one the roster still ADVERTISES: `live`, or `away` with its node.
#[must_use]
pub fn advertised(bus: &str) -> bool {
    bus == "live" || bus == crate::glance::AWAY
}

/// How many sessions the roster advertises on `node` — `live`, or `away` with a node whose will fired ([`advertised`]).
#[must_use]
pub fn live_sessions_on(r: &Report, node: &str) -> usize {
    r.sessions
        .iter()
        .filter(|s| s.node == node && advertised(&s.bus))
        .count()
}

fn render_nodes(r: &Report) -> String {
    let mut out = String::from(
        "\nNODES  every node on the fleet's presence roster · this = the node this command's \
         state dir names\n",
    );
    let mut nodes: Vec<&NodeRow> = r.nodes.iter().collect();
    nodes.sort_by_key(|n| (node_where(r, &n.node) != "this", n.node.clone()));
    let mut rows: Vec<Vec<String>> = nodes
        .iter()
        .map(|n| {
            vec![
                safe(&n.node, 64),
                node_where(r, &n.node).to_string(),
                format!(
                    "host={} state={} fabric={} proto={} aterm={}",
                    safe(&n.host, 64),
                    safe(n.reads(), 16),
                    safe(&n.fabric, 16),
                    safe(&n.proto, 8),
                    safe(&n.aterm, 64)
                ),
                live_sessions_on(r, &n.node).to_string(),
            ]
        })
        .collect();
    if let Some(me) = r.node.as_deref() {
        if r.broker.head.is_some() && !r.nodes.iter().any(|n| n.node == me) {
            rows.insert(
                0,
                vec![
                    safe(me, 64),
                    "this".to_string(),
                    "- (no presence row on the bus: no bridge of this node has attached)"
                        .to_string(),
                    "0".to_string(),
                ],
            );
        }
    }
    if rows.is_empty() {
        out.push_str(if r.broker.head.is_some() {
            "  (the roster lists no node on this fleet)\n"
        } else {
            "  (the bus could not be read)\n"
        });
    } else {
        table(
            &mut out,
            &["NODE", "WHICH", "ON THE BUS", "SESSIONS"],
            &rows,
            &[false, false, false, true],
        );
    }
    if r.nodes.iter().any(|n| node_where(r, &n.node) == "remote") {
        out.push_str(&format!("  {REMOTE_NODE_NOTE}\n"));
    }
    out
}

/// The line NODES adds under its table when a row is `remote` ([`node_where`]).
pub const REMOTE_NODE_NOTE: &str = "remote = no aterm instance here answers as that node: the \
     bus is this machine's, so it is an unhosted or ghost node of this machine (an aterm that \
     quit under another state dir, a scratch instance, one that did not answer), never another \
     host. Only that node can retire its rows: `aterm fabric doctor --retire-ghosts` under a \
     [fabric] command naming its state dir and its cap; with those gone nothing here can, its \
     rows stay and mail to it queues";

/// Render the report as the text a human reads.
#[must_use]
pub fn render_text(r: &Report) -> String {
    let offset = local_offset();
    let off = offset.unwrap_or(0);
    let mut out = String::new();
    out.push_str(&format!(
        "aterm fabric — fleet {} · node {} · {} · {}\n\n",
        safe(&r.cfg.fleet, 64),
        r.node.as_deref().unwrap_or("-"),
        clock(r.now_ms, r.now_ms, off),
        zone_label(offset)
    ));

    out.push_str("CONFIG\n");
    let mut kvs: Vec<(&str, String)> = vec![("source", r.source.describe())];
    if let Some(program) = r.command.first() {
        let program = safe(&shell_word(program), 256);
        kvs.push((
            "program",
            if r.program_in_target_dir {
                format!(
                    "{program} — a DEVELOPMENT build in a cargo target dir: the next build \
                     replaces it, `targo clean` removes it; run `aterm fabric on` from the \
                     installed aterm"
                )
            } else {
                program
            },
        ));
    }
    kvs.push(("fleet", safe(&r.cfg.fleet, 64)));
    kvs.push((
        "node",
        r.node
            .clone()
            .unwrap_or_else(|| format!("- (no node file in {})", safe(&r.cfg.state_dir, 256))),
    ));
    for (path, caps) in &r.caps {
        kvs.push((
            "cap file",
            match caps {
                Ok(n) => format!(
                    "{} ({n} grant{})",
                    safe(path, 256),
                    if *n == 1 { "" } else { "s" }
                ),
                Err(e) => format!("{} (UNREADABLE: {})", safe(path, 256), safe(e, 256)),
            },
        ));
    }
    if r.caps.is_empty() {
        kvs.push((
            "cap file",
            "- (none: the broker must be unguarded)".to_string(),
        ));
    }
    kvs.push(("state dir", safe(&r.cfg.state_dir, 256)));
    if !r.cfg.accept_from.is_empty() {
        kvs.push(("accepts", safe(&r.cfg.accept_from.join(","), 256)));
    }
    kvs.push((
        "presence",
        match r.presence {
            crate::presence::Mode::Meta => {
                "meta (role, detail, phase and title on every session row)".to_string()
            }
            crate::presence::Mode::Minimal => {
                "minimal (state, hold and attention only)".to_string()
            }
        },
    ));
    kvs.push((
        "receipts",
        if r.receipts {
            "on (any verdict on an ask or task acks its sender)".to_string()
        } else {
            let set_by = if serve_flags(&r.command)
                .is_some_and(|f| f.iter().any(|a| a == "--no-receipts"))
            {
                "`--no-receipts` in the bridge command".to_string()
            } else {
                format!(
                    "`[fabric] receipts = false` in {}",
                    r.config_path
                        .as_ref()
                        .map_or_else(|| "aterm.toml".to_string(), |p| p.display().to_string())
                )
            };
            format!(
                "off ({set_by}): an `ask` or `post --wait-ack` sent here waits out its whole \
                 deadline"
            )
        },
    ));
    push_kvs(&mut out, &kvs);

    out.push_str("\nBROKER\n");
    let b = &r.broker;
    let mut kvs: Vec<(&str, String)> = vec![("socket", safe(&b.endpoint, 256))];
    kvs.push((
        "reachable",
        if b.read_ok() {
            format!("yes ({} ms)", b.rtt_ms.unwrap_or(0))
        } else if b.reachable {
            format!(
                "yes, but the bus cannot be read: {}",
                safe(b.error.as_deref().unwrap_or("-"), 256)
            )
        } else {
            format!("NO — {}", safe(b.error.as_deref().unwrap_or("-"), 256))
        },
    ));
    kvs.push((
        "pid",
        format!(
            "{}{}",
            match (b.pid, &b.launchd) {
                (Some(p), Some(l)) => format!("{p} (launchd {})", safe(l, 128)),
                (Some(p), None) => format!("{p}"),
                (None, _) => "- (no `link broker` process serves this socket)".to_string(),
            },
            b.replaced
                .as_deref()
                .map_or_else(String::new, |exe| format!(
                    " — RUNS A REPLACED BUILD: {} is another file now",
                    safe(exe, 256)
                ))
        ),
    ));
    kvs.push((
        "bus head",
        b.head.map_or_else(|| "-".to_string(), |h| format!("@{h}")),
    ));
    if let Some(subjects) = &r.subjects {
        kvs.push((
            "subjects",
            match subjects {
                Ok(n) => format!(
                    "{n} of {SUBJECT_BUDGET} spent by this node — one per presence, `ev` and \
                     `say` face and lane it ever wrote; only a closed session's `ev` face is ever \
                     given back, while the session-log mirror runs"
                ),
                Err(e) => format!("- (could not be counted: {})", safe(e, 256)),
            },
        ));
    }
    if let Some(d) = b.tier() {
        kvs.push((
            "acks",
            format!(
                "{} durability — {}",
                d.name(),
                crate::durability::describe_served(d, b.served, b.guarded)
            ),
        ));
    }
    if let Some(row) = health_row(b) {
        kvs.push(("health", row));
    }
    if let Some(row) = holds_row(b, r.now_ms) {
        kvs.push(("holds", row));
    }
    if let Some(guarded) = b.guarded {
        kvs.push((
            "guard",
            if guarded {
                "yes — it refused a capability it cannot verify, so every attach is checked"
                    .to_string()
            } else {
                "NO — it accepted a capability it cannot verify: it checks none".to_string()
            },
        ));
    }
    if let Some(faces) = &b.faces {
        kvs.push((
            "reads",
            format!(
                "the faces the cap files grant — the broker is GUARDED and they do not grant \
                 /f/{}/>: {}",
                safe(&r.cfg.fleet, 64),
                safe(&faces.join(" "), 512)
            ),
        ));
    }
    push_kvs(&mut out, &kvs);

    out.push_str(&render_nodes(r));

    out.push_str("\nBRIDGES  one row per aterm instance on this machine\n");
    let node_row = |n: &str| r.nodes.iter().find(|row| row.node == n);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for i in &r.instances {
        let node = i.node.clone().unwrap_or_else(|| "-".to_string());
        let bus = node_row(&node).map_or_else(
            || "-".to_string(),
            |n| {
                format!(
                    "state={} fabric={}",
                    safe(n.reads(), 16),
                    safe(&n.fabric, 16)
                )
            },
        );
        rows.push(vec![
            i.pid.to_string(),
            if i.error.is_some() {
                "?".to_string()
            } else {
                fabric_cell(i)
            },
            match i.supervised {
                Some(true) => "yes".to_string(),
                Some(false) => "NO".to_string(),
                None => "?".to_string(),
            },
            if i.bridge_pids.is_empty() {
                "-".to_string()
            } else {
                i.bridge_pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            },
            node,
            bus,
            i.program
                .as_deref()
                .map_or_else(|| "-".to_string(), |p| safe(p, 256)),
        ]);
    }
    if rows.is_empty() {
        out.push_str("  (no aterm instance on this machine publishes a control socket)\n");
    } else {
        table(
            &mut out,
            &[
                "INSTANCE",
                "FABRIC",
                "SUPERVISED",
                "BRIDGE",
                "NODE",
                "ON THE BUS",
                "PROGRAM",
            ],
            &rows,
            &[true, false, false, true, false, false, false],
        );
    }

    out.push_str(
        "\nSESSIONS  unread = listed, not handled · pending = delivered, never listed · \
         posts = queued outbound\n",
    );
    let many_nodes = r.nodes.len() > 1
        || r.sessions
            .iter()
            .map(|s| &s.node)
            .collect::<BTreeSet<_>>()
            .len()
            > 1;
    let here: BTreeSet<&str> = r
        .instances
        .iter()
        .filter_map(|i| i.node.as_deref())
        .chain(r.node.as_deref())
        .collect();
    let mut rows = Vec::new();
    for s in &r.sessions {
        let l = s.local.as_ref();
        let mut row = vec![
            safe(&s.sid, 64),
            s.pid.map_or_else(
                || {
                    if here.contains(s.node.as_str()) {
                        "-".to_string()
                    } else {
                        "remote".to_string()
                    }
                },
                |p| p.to_string(),
            ),
        ];
        if many_nodes {
            row.push(safe(&s.node, 64));
        }
        row.extend([
            safe(&s.bus, 16),
            l.and_then(|l| l.hold)
                .map(|h| h.to_string())
                .or_else(|| s.bus_hold.as_deref().map(|h| safe(h, 8)))
                .unwrap_or_else(|| "-".to_string()),
            num(l.and_then(|l| l.unread)),
            num(l.and_then(|l| l.pending)),
            num(l.and_then(|l| l.dropped)),
            num(l.and_then(|l| l.seen)),
            num(l.and_then(|l| l.posts)),
            l.filter(|l| !l.topics.is_empty())
                .map_or_else(|| "-".to_string(), |l| safe(&l.topics.join(","), 48)),
            safe(session_role(s), 48),
            s.detail
                .as_deref()
                .map_or_else(|| "-".to_string(), |d| safe(d, 32)),
            s.phase
                .as_deref()
                .map_or_else(|| "-".to_string(), |p| safe(p, 12)),
            s.context
                .as_deref()
                .map_or_else(|| "-".to_string(), |c| safe(c, 8)),
            safe(session_title(s), 64),
        ]);
        rows.push(row);
    }
    if rows.is_empty() {
        out.push_str("  (no sessions: no local instance answered and the bus lists none live)\n");
    } else {
        let mut header = vec!["SID", "PID"];
        let mut right = vec![false, true];
        if many_nodes {
            header.push("NODE");
            right.push(false);
        }
        header.extend([
            "BUS", "HOLD", "UNREAD", "PENDING", "DROPPED", "SEEN", "POSTS", "TOPICS", "ROLE",
            "DETAIL", "PHASE", "CTX", "TITLE",
        ]);
        right.extend([
            false, true, true, true, true, true, true, false, false, false, false, true, false,
        ]);
        table(&mut out, &header, &rows, &right);
    }
    if r.exited > 0 {
        out.push_str(&format!(
            "  (+{} exited session{} on the bus not shown; --json lists them)\n",
            r.exited,
            if r.exited == 1 { "" } else { "s" }
        ));
    }
    if r.roster_truncated {
        out.push_str(&format!(
            "  (roster: rows={}+ truncated — the presence read stopped there; WARNINGS)\n",
            crate::glance::MAX_ROWS
        ));
    }

    out.push_str(&format!(
        "\nTRAFFIC  the last {} record{} {} — metadata only; `aterm fabric tail \
         --bodies` shows text\n",
        r.traffic.len(),
        if r.traffic.len() == 1 { "" } else { "s" },
        if r.broker.faces.is_some() {
            "on the faces this node's cap reads (BROKER `reads`)".to_string()
        } else {
            format!("under /f/{}/>", safe(&r.cfg.fleet, 64))
        }
    ));
    if r.traffic.is_empty() {
        out.push_str(if r.broker.head.is_some() {
            "  (the bus holds no record for this fleet)\n"
        } else {
            "  (the bus could not be read)\n"
        });
    } else {
        let rows: Vec<Vec<String>> = r
            .traffic
            .iter()
            .map(|t| traffic_cells(t, r.now_ms, off))
            .collect();
        table(
            &mut out,
            &TRAFFIC_HEADER,
            &rows,
            &[false, false, false, true, false, false, false],
        );
    }

    if r.warnings.is_empty() {
        out.push_str("\nWARNINGS  none\n");
    } else {
        out.push_str(&format!(
            "\nWARNINGS  {} — fixes: `aterm fabric doctor`\n",
            r.warnings.len()
        ));
        for w in &r.warnings {
            out.push_str("  ! ");
            out.push_str(w);
            out.push('\n');
        }
    }
    out
}

fn push_kvs(out: &mut String, kvs: &[(&str, String)]) {
    let w = kvs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in kvs {
        out.push_str(&format!("  {k:<w$}  {v}\n"));
    }
}

/// A JSON value, rendered by hand: this crate carries no JSON writer and the shape is small.
pub(crate) enum J {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    pub(crate) fn s(v: &str) -> J {
        J::Str(v.to_string())
    }
    pub(crate) fn opt_s(v: Option<&str>) -> J {
        v.map_or(J::Null, J::s)
    }
    pub(crate) fn opt_n(v: Option<u64>) -> J {
        v.map_or(J::Null, J::Num)
    }
    pub(crate) fn render(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Num(n) => out.push_str(&n.to_string()),
            J::Str(s) => out.push_str(&crate::glance::json_string(s)),
            J::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.render(out);
                }
                out.push(']');
            }
            J::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&crate::glance::json_string(k));
                    out.push(':');
                    v.render(out);
                }
                out.push('}');
            }
        }
    }
}

/// The exit status a report earns: `1` with any warning, else `0`.
#[must_use]
pub fn exit_of(r: &Report) -> u8 {
    u8::from(!r.warnings.is_empty())
}

/// Render the report as one JSON object — the same data as the text.
#[must_use]
pub fn render_json(r: &Report) -> String {
    let caps = r
        .caps
        .iter()
        .map(|(p, c)| {
            J::Obj(vec![
                ("path", J::s(p)),
                ("grants", J::opt_n(c.as_ref().ok().map(|n| *n as u64))),
                ("error", J::opt_s(c.as_ref().err().map(String::as_str))),
            ])
        })
        .collect();
    let b = &r.broker;
    let bridges = r
        .instances
        .iter()
        .map(|i| {
            J::Obj(vec![
                ("pid", J::Num(u64::from(i.pid))),
                ("sock", J::s(&i.sock)),
                ("fabric", J::opt_s(i.fabric.as_deref())),
                ("reason", J::opt_s(i.reason.as_deref())),
                ("rtt_ms", J::opt_n(i.rtt_ms)),
                ("link_age_ms", J::opt_n(i.link_age_ms)),
                ("supervised", i.supervised.map_or(J::Null, J::Bool)),
                (
                    "bridge_pids",
                    J::Arr(
                        i.bridge_pids
                            .iter()
                            .map(|p| J::Num(u64::from(*p)))
                            .collect(),
                    ),
                ),
                (
                    "command",
                    J::opt_s(i.command.as_deref().map(shown_argv).as_deref()),
                ),
                (
                    "argv",
                    i.command.as_ref().map_or(J::Null, |argv| {
                        J::Arr(argv.iter().map(|w| J::s(w)).collect())
                    }),
                ),
                ("program", J::opt_s(i.program.as_deref())),
                ("node", J::opt_s(i.node.as_deref())),
                ("error", J::opt_s(i.error.as_deref())),
            ])
        })
        .collect();
    let sessions = r
        .sessions
        .iter()
        .map(|s| {
            let l = s.local.as_ref();
            J::Obj(vec![
                ("sid", J::s(&s.sid)),
                ("pid", J::opt_n(s.pid.map(u64::from))),
                ("node", J::s(&s.node)),
                ("bus", J::s(&s.bus)),
                (
                    "title",
                    J::opt_s(Some(session_title(s)).filter(|t| *t != "-")),
                ),
                (
                    "role",
                    J::opt_s(Some(session_role(s)).filter(|r| *r != "-")),
                ),
                ("detail", J::opt_s(s.detail.as_deref())),
                ("phase", J::opt_s(s.phase.as_deref())),
                (
                    "context",
                    J::opt_n(
                        s.context
                            .as_deref()
                            .and_then(|c| c.strip_suffix('%'))
                            .and_then(|n| n.parse().ok()),
                    ),
                ),
                (
                    "hold",
                    J::opt_n(
                        l.and_then(|l| l.hold)
                            .map(u64::from)
                            .or_else(|| s.bus_hold.as_deref().and_then(|h| h.parse().ok())),
                    ),
                ),
                (
                    "hold_reason",
                    J::opt_s(l.and_then(|l| l.hold_why.as_ref()).map(|w| w.0.as_str())),
                ),
                (
                    "hold_origin",
                    J::opt_s(l.and_then(|l| l.hold_why.as_ref()).map(|w| w.1.as_str())),
                ),
                ("holder", J::opt_s(l.map(|l| l.holder.as_str()))),
                ("unread", J::opt_n(l.and_then(|l| l.unread))),
                ("pending", J::opt_n(l.and_then(|l| l.pending))),
                ("dropped", J::opt_n(l.and_then(|l| l.dropped))),
                ("seen", J::opt_n(l.and_then(|l| l.seen))),
                ("posts", J::opt_n(l.and_then(|l| l.posts))),
                (
                    "topics",
                    l.map_or(J::Null, |l| {
                        J::Arr(l.topics.iter().map(|t| J::s(t)).collect())
                    }),
                ),
                ("error", J::opt_s(l.and_then(|l| l.error.as_deref()))),
            ])
        })
        .collect();
    let nodes = r
        .nodes
        .iter()
        .map(|n| {
            J::Obj(vec![
                ("node", J::s(&n.node)),
                ("host", J::s(&n.host)),
                ("state", J::s(&n.state)),
                ("reads", J::s(n.reads())),
                ("fabric", J::s(&n.fabric)),
                ("proto", J::s(&n.proto)),
                ("aterm", J::s(&n.aterm)),
                ("this", J::Bool(r.node.as_deref() == Some(n.node.as_str()))),
                ("where", J::s(node_where(r, &n.node))),
                ("sessions", J::Num(live_sessions_on(r, &n.node) as u64)),
            ])
        })
        .collect();
    let traffic = r
        .traffic
        .iter()
        .map(|t| {
            J::Obj(vec![
                ("off", J::Num(t.off)),
                ("t_ms", J::Num(t.t_ms)),
                (
                    "time",
                    if t.t_ms == 0 {
                        J::Null
                    } else {
                        J::Str(aterm_types::rfc3339::format_rfc3339(t.t_ms / 1000))
                    },
                ),
                ("from", J::s(&t.from)),
                ("to", J::s(&t.to)),
                ("kind", J::s(&t.kind)),
                ("len", J::Num(t.len as u64)),
                ("trust", J::s(t.trust)),
            ])
        })
        .collect();
    let obj = J::Obj(vec![
        ("schema", J::Num(1)),
        ("exit", J::Num(u64::from(exit_of(r)))),
        (
            "config",
            J::Obj(vec![
                ("source", J::s(r.source.token())),
                (
                    "path",
                    J::opt_s(match &r.source {
                        Source::File(p) => p.to_str(),
                        _ => r.config_path.as_deref().and_then(Path::to_str),
                    }),
                ),
                ("command", J::s(&shown_argv(&r.command))),
                ("argv", J::Arr(r.command.iter().map(|w| J::s(w)).collect())),
                ("program", J::opt_s(r.program.as_deref())),
                ("fleet", J::s(&r.cfg.fleet)),
                ("node", J::opt_s(r.node.as_deref())),
                ("presence", J::s(r.presence.name())),
                ("receipts", J::Bool(r.receipts)),
                ("cap_files", J::Arr(caps)),
                ("state_dir", J::s(&r.cfg.state_dir)),
                (
                    "accept_from",
                    J::Arr(r.cfg.accept_from.iter().map(|p| J::s(p)).collect()),
                ),
            ]),
        ),
        (
            "broker",
            J::Obj(vec![
                ("endpoint", J::s(&b.endpoint)),
                ("reachable", J::Bool(b.reachable)),
                ("attached", J::Bool(b.attached)),
                ("cap_expired", J::Bool(b.cap_expired)),
                ("error", J::opt_s(b.error.as_deref())),
                ("pid", J::opt_n(b.pid.map(u64::from))),
                ("launchd", J::opt_s(b.launchd.as_deref())),
                ("head", J::opt_n(b.head)),
                ("rtt_ms", J::opt_n(b.rtt_ms)),
                ("guarded", b.guarded.map_or(J::Null, J::Bool)),
                ("replaced_build", J::opt_s(b.replaced.as_deref())),
                (
                    "durability",
                    J::opt_s(b.tier().map(crate::durability::Durability::name)),
                ),
                (
                    "served",
                    J::opt_s(b.served.map(|s| crate::durability::tier_name(s.served))),
                ),
                (
                    "faces",
                    b.faces
                        .as_ref()
                        .map_or(J::Null, |f| J::Arr(f.iter().map(|x| J::s(x)).collect())),
                ),
                ("health", health_json(b, r.now_ms)),
                (
                    "subjects",
                    r.subjects.as_ref().map_or(J::Null, |s| {
                        J::Obj(vec![
                            ("used", J::opt_n(s.as_ref().ok().map(|n| *n as u64))),
                            ("budget", J::Num(SUBJECT_BUDGET as u64)),
                            ("error", J::opt_s(s.as_ref().err().map(String::as_str))),
                        ])
                    }),
                ),
            ]),
        ),
        ("bridges", J::Arr(bridges)),
        ("discovery_error", J::opt_s(r.discovery_error.as_deref())),
        (
            "roster",
            J::Obj(vec![
                ("rows", J::opt_n(r.roster_rows.map(|n| n as u64))),
                ("truncated", J::Bool(r.roster_truncated)),
            ]),
        ),
        ("nodes", J::Arr(nodes)),
        ("sessions", J::Arr(sessions)),
        ("exited_sessions", J::Num(r.exited as u64)),
        ("traffic", J::Arr(traffic)),
        (
            "overdue",
            J::Arr(
                r.overdue
                    .iter()
                    .map(|o| {
                        J::Obj(vec![
                            ("off", J::Num(o.off)),
                            ("from", J::s(&o.from)),
                            ("to", J::s(&o.to)),
                            ("kind", J::s(&o.kind)),
                            ("dl_ms", J::Num(o.dl)),
                            ("late_ms", J::Num(o.late_ms)),
                            ("expired", J::Bool(o.expired)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "warnings",
            J::Arr(r.warnings.iter().map(|w| J::s(w)).collect()),
        ),
    ]);
    let mut out = String::new();
    obj.render(&mut out);
    out.push('\n');
    out
}

/// The `--json` rendering of "off": the same exit status, as data.
#[must_use]
pub fn render_off_json(off: &Off) -> String {
    let mut out = String::new();
    J::Obj(vec![
        ("schema", J::Num(1)),
        ("exit", J::Num(2)),
        ("off", J::Str(off.message())),
    ])
    .render(&mut out);
    out.push('\n');
    out
}

fn status_main(json: bool) -> ExitCode {
    let (text, code) = match gather() {
        Ok(r) => (
            if json {
                render_json(&r)
            } else {
                render_text(&r)
            },
            exit_of(&r),
        ),
        Err(off) => (
            if json {
                render_off_json(&off)
            } else {
                format!("{}\n", off.message())
            },
            2,
        ),
    };
    aterm_uds::stdout::answer_exit("aterm fabric status", &text, code)
}

/// One `tail` line.
#[must_use]
pub fn tail_line(t: &Traffic, now_ms: u64, offset: i64, bodies: bool) -> String {
    let c = traffic_cells(t, now_ms, offset);
    let mut line = format!(
        "{:<8} {:<8}  {:<10} {:>5}  {:<8} {:<42} {}",
        c[0], c[1], c[2], c[3], c[4], c[5], c[6]
    );
    if bodies && !t.text.is_empty() {
        line.push_str("  text=");
        line.push_str(&safe(&t.text, crate::render::TEXT_CAP));
    }
    line.trim_end().to_string()
}

fn tail_main(bodies: bool, from: Option<u64>, filter: Option<&str>) -> ExitCode {
    let cfg = match configured() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    match tail(&cfg, bodies, from, filter) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm fabric tail: {e}");
            ExitCode::FAILURE
        }
    }
}

fn configured() -> Result<Config, ExitCode> {
    configured_for_reads()
}

/// [`configured`], for the bus readers beside `tail` (`fabric brief`).
pub(crate) fn configured_for_reads() -> Result<Config, ExitCode> {
    let resolved = match resolve_command(command_seam().as_deref(), config_path().as_deref()) {
        Ok(Some((_, c))) => c,
        Ok(None) => {
            let from_rendezvous = match rendezvous_command() {
                Ok(r) => r.map(|(_, c)| c),
                Err(e) => {
                    eprintln!("{}", Off::Unreadable(e).message());
                    return Err(ExitCode::from(2));
                }
            };
            let found = from_rendezvous.or_else(|| {
                aterm_ctl::local_instances()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|(pid, sock)| read_instance(*pid, sock, &[], false))
                    .find(|i| i.supervised == Some(true))
                    .and_then(|i| i.command)
            });
            match found {
                Some(c) => c,
                None => {
                    eprintln!("{}", Off::NoCommand(config_path()).message());
                    return Err(ExitCode::from(2));
                }
            }
        }
        Err(e) => {
            eprintln!("{}", Off::Unreadable(e).message());
            return Err(ExitCode::from(2));
        }
    };
    bridge_config(&resolved).map_err(|e| {
        eprintln!("{}", Off::Unreadable(e).message());
        ExitCode::from(2)
    })
}

fn is_subject_filter(f: &str) -> bool {
    if astream_wire::subject::validate_filter_str(f).is_err() {
        return false;
    }
    if !f.starts_with("/f/") || f.len() > 256 {
        return false;
    }
    f.split('/').skip(1).all(|seg| {
        !seg.is_empty()
            && (seg == "*"
                || seg == ">"
                || seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '+')))
    })
}

fn tail(cfg: &Config, bodies: bool, from: Option<u64>, want: Option<&str>) -> io::Result<()> {
    let broker = safe(&cfg.broker, 256);
    let (mut conn, closer) = transport::connect(&cfg.broker, IO_TIMEOUT)
        .map_err(|e| io::Error::new(e.kind(), format!("the broker at {broker}: {e}")))?;
    transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
    let whole = match want {
        Some(f) => {
            let root = fleet_root(&cfg.fleet);
            let prefix = root.trim_end_matches('>');
            if !f.starts_with(prefix) {
                return Err(io::Error::other(format!(
                    "--filter {} is not under this fleet ({prefix}…)",
                    safe(f, 128)
                )));
            }
            f.to_string()
        }
        None => fleet_root(&cfg.fleet),
    };
    let (faces, head) = match conn.fetch(0, &whole, 0) {
        Ok((_, (_, head))) => (None, head),
        Err(e) if refused(&e) && want.is_some() => return Err(e),
        Err(e) if refused(&e) => {
            let faces = readable_faces(&cfg.fleet, &cfg.cap_files);
            let head = faces
                .iter()
                .find_map(|f| conn.fetch(0, f, 0).ok().map(|(_, (_, h))| h))
                .ok_or(e)?;
            (Some(faces), head)
        }
        Err(e) => return Err(e),
    };
    let filter = faces
        .as_ref()
        .map_or_else(|| whole.clone(), |f| f.join(" "));
    let start = from.unwrap_or(head);
    let offset = local_offset();
    eprintln!(
        "aterm fabric tail — {filter} on {broker}, from @{start}{} ({}); Ctrl-C stops{}",
        if from.is_some() {
            ""
        } else {
            " (new records only)"
        },
        zone_label(offset),
        if bodies {
            ""
        } else {
            "; bodies hidden (--bodies shows them)"
        }
    );
    let mut out = io::stdout().lock();
    let header = format!(
        "{:<8} {:<8}  {:<10} {:>5}  {:<8} {:<42} {}",
        TRAFFIC_HEADER[0],
        TRAFFIC_HEADER[1],
        TRAFFIC_HEADER[2],
        TRAFFIC_HEADER[3],
        TRAFFIC_HEADER[4],
        TRAFFIC_HEADER[5],
        TRAFFIC_HEADER[6]
    );
    if writeln!(out, "{header}")
        .and_then(|()| out.flush())
        .is_err()
    {
        return Ok(());
    }
    if let Some(faces) = faces {
        drop(conn);
        return tail_faces(cfg, &faces, start, bodies, offset.unwrap_or(0), &mut out);
    }
    let mut sub = conn.subscribe(start, &filter)?;
    closer.set_read_timeout(None)?;
    while let Some((off, subject, raw)) = sub
        .recv_live(&closer, astream_broker::IDLE_MARK_DEADLINE)
        .map_err(|e| silent_link(e, &broker))?
    {
        let t = traffic_of(&cfg.fleet, off, &subject, &raw);
        let line = tail_line(&t, crate::now_ms(), offset.unwrap_or(0), bodies);
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            return Ok(());
        }
    }
    Err(io::Error::other(format!(
        "the broker at {broker} closed the stream"
    )))
}

fn silent_link(e: io::Error, broker: &str) -> io::Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => io::Error::new(
            e.kind(),
            format!(
                "the broker at {broker} went silent: nothing in {} s on a link it marks every \
                 {} s, so the link is gone (resume with --from)",
                astream_broker::IDLE_MARK_DEADLINE.as_secs(),
                astream_broker::IDLE_MARK.as_secs()
            ),
        ),
        _ => e,
    }
}

fn tail_faces(
    cfg: &Config,
    faces: &[String],
    start: u64,
    bodies: bool,
    offset: i64,
    out: &mut impl Write,
) -> io::Result<()> {
    use std::sync::mpsc;
    let broker = safe(&cfg.broker, 256);
    let (tx, rx) = mpsc::sync_channel::<io::Result<(u64, String, Vec<u8>)>>(0);
    for face in faces {
        let (mut conn, closer) = transport::connect(&cfg.broker, IO_TIMEOUT)?;
        transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
        let mut sub = conn.subscribe(start, face)?;
        closer.set_read_timeout(None)?;
        let tx = tx.clone();
        let broker = broker.clone();
        std::thread::spawn(move || loop {
            match sub
                .recv_live(&closer, astream_broker::IDLE_MARK_DEADLINE)
                .map_err(|e| silent_link(e, &broker))
            {
                Ok(Some(rec)) => {
                    if tx.send(Ok(rec)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(Err(io::Error::other(format!(
                        "the broker at {broker} closed a face's stream"
                    ))));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        });
    }
    drop(tx);
    let mut printed: BTreeSet<u64> = BTreeSet::new();
    while let Ok(next) = rx.recv() {
        let (off, subject, raw) = next?;
        if !printed.insert(off) {
            continue;
        }
        while printed.len() > 4096 {
            printed.pop_first();
        }
        let t = traffic_of(&cfg.fleet, off, &subject, &raw);
        let line = tail_line(&t, crate::now_ms(), offset, bodies);
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            return Ok(());
        }
    }
    Err(io::Error::other(format!(
        "the broker at {broker} closed a face's stream"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        aterm_types::fabric_command::split_line(line)
    }

    #[test]
    fn a_retire_is_refused_for_a_sibling_hosted_sid_or_an_unaskable_node() {
        let hosted: Result<BTreeMap<String, u32>, String> =
            Ok([("s-aaaaaaaaaaaaaaaaaaaa".to_string(), 4242)]
                .into_iter()
                .collect());
        assert_eq!(
            retire_refusal("s-aaaaaaaaaaaaaaaaaaaa", &hosted),
            Some(RetireRefusal::Hosted(4242))
        );
        assert_eq!(retire_refusal("s-bbbbbbbbbbbbbbbbbbbb", &hosted), None);
        let unaskable: Result<BTreeMap<String, u32>, String> =
            Err("instance 77: connect: Operation timed out".to_string());
        assert_eq!(
            retire_refusal("s-bbbbbbbbbbbbbbbbbbbb", &unaskable),
            Some(RetireRefusal::Unaskable(
                "instance 77: connect: Operation timed out".to_string()
            ))
        );
    }

    #[test]
    fn ghost_rows_are_live_unhosted_and_ours() {
        let row = |sid: &str, pid: Option<u32>, node: &str, bus: &str| SessionRow {
            sid: sid.to_string(),
            pid,
            node: node.to_string(),
            bus: bus.to_string(),
            ..SessionRow::default()
        };
        let sessions = vec![
            row("s-ghost", None, "n-ours", "live"),
            row("s-hosted", Some(42), "n-ours", "live"),
            row("s-gone", None, "n-ours", "exited"),
            row("s-elsewhere", None, "n-theirs", "live"),
        ];
        let local = local_nodes(&[], Some("n-ours"));
        let got: Vec<&str> = ghost_rows(&sessions, &local)
            .iter()
            .map(|s| s.sid.as_str())
            .collect();
        assert_eq!(got, ["s-ghost"]);
    }

    #[test]
    fn a_retired_body_moves_only_state_and_the_stamp() {
        let out = retired_body("v=1 t=1 inc=7 epoch=e1 gen=g1 state=live hold=0 role=worker");
        assert!(out.contains("state=exited"), "{out}");
        assert!(!out.contains("state=live"), "{out}");
        assert!(!out.contains(" t=1 "), "the stamp is refreshed: {out}");
        for kept in [
            "v=1",
            "inc=7",
            "epoch=e1",
            "gen=g1",
            "hold=0",
            "role=worker",
        ] {
            assert!(out.contains(kept), "{kept} must survive: {out}");
        }
        let bare = retired_body("v=1 inc=2");
        assert!(
            bare.contains("state=exited") && bare.contains("t="),
            "{bare}"
        );
    }

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_grammar_is_status_by_default_and_refuses_what_it_does_not_know() {
        assert_eq!(parse_args(&[]), Ok(Cmd::Status { json: false }));
        assert_eq!(parse_args(&argv("--json")), Ok(Cmd::Status { json: true }));
        for bad in [
            "pub/>",
            "/f//pub/>",
            "/f/lab/pub/ /say",
            "/f/lab/\u{1b}[2J",
            "/f/lab/a;rm -rf /",
            "/f/lab/>/x",
        ] {
            let got = parse_args(&[
                "tail".to_string(),
                "--filter".to_string(),
                (*bad).to_string(),
            ]);
            assert!(got.is_err(), "{bad}: {got:?}");
        }
        assert_eq!(
            parse_args(&argv("on --durability relaxed")),
            Ok(Cmd::On(crate::enable::OnOpts {
                durability: Some(crate::durability::Durability::Relaxed),
                ..crate::enable::OnOpts::default()
            }))
        );
        assert_eq!(
            parse_args(&argv("tail --filter /f/lab/pub/*/*/say/>")),
            Ok(Cmd::Tail {
                bodies: false,
                from: None,
                filter: Some("/f/lab/pub/*/*/say/>".to_string())
            })
        );
        assert_eq!(
            parse_args(&argv("doctor --retire-ghosts")),
            Ok(Cmd::Doctor {
                retire_ghosts: true,
                yes: false
            }),
            "dry by default"
        );
        for bad in [
            "--aterm-front-door-routing-probe",
            "doctor --yes",
            "status --retire-ghosts",
            "tail --yes",
            "status --filter /f/lab/>",
            "doctor --filter /f/lab/>",
            "status --bodies",
            "tail --json",
            "tail --from",
            "tail --from x",
            "stats",
            "on --service cron",
            "on --fleet LAB",
            "off --fleet lab",
            "on --durability fast",
            "off --durability relaxed",
            "doctor --dry-run",
            "mint-for",
            "mint-for n-a",
            "mint-for h-a h-b",
            "mint-for h-a --expires soon",
            "join --broker x:1",
            "on --broker x:1",
            "on --cap-file /c",
            "on --tcp 127.0.0.1:7000",
            "status --accept-from n-a",
        ] {
            assert!(parse_args(&argv(bad)).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_tail_filter_passes_astreams_grammar_then_aterms_narrower_one() {
        use astream_wire::subject::validate_filter_str as astream;
        for f in ["/f/lab/>/x", "/f/>/pub/>", "/f/lab/>/>"] {
            assert!(astream(f).is_err(), "astream must refuse {f}");
            assert!(!is_subject_filter(f), "{f} passed astream's half");
        }
        let long = format!("/f/lab/{}", "a".repeat(250));
        assert_eq!(long.len(), 257, "one byte past the cap");
        for f in [
            "/x/lab/>",
            long.as_str(),
            "/f/lab/pub/ /say",
            "/f/lab/a;b",
            "/f/lab/caf\u{e9}",
        ] {
            assert!(astream(f).is_ok(), "astream must accept {f}");
            assert!(!is_subject_filter(f), "{f} passed aterm's half");
        }
        let at_cap = format!("/f/lab/{}", "a".repeat(249));
        for f in [
            "/f/lab/>",
            "/f/lab/fleet/>",
            "/f/lab/fleet/*/halt",
            "/f/lab/pub/*/*/say/>",
            "/f/lab/pub/*/*/presence",
            "/f/lab/pub/n-00000000000000aa/*/presence",
            "/f/lab/pub/*/s-1/presence",
            "/f/lab/pub/*/*/ev",
            "/f/lab/pub/n-a/>",
            "/f/lab/in/n-a/>",
            "/f/lab/in/n-a/s-1/>",
            "/f/lab/in/*/*/n-a/*",
            "/f/lab/in/p/h-x/>",
            "/f/lab/in/p/h-x/*/ask",
            at_cap.as_str(),
        ] {
            assert!(astream(f).is_ok(), "astream must accept {f}");
            assert!(is_subject_filter(f), "{f} must pass");
        }
    }

    #[test]
    fn the_command_is_read_from_every_toml_spelling_and_blank_is_off() {
        let cmd = "aterm link serve --fleet f --broker /tmp/b.sock";
        let want = words(cmd);
        for text in [
            format!("[fabric]\ncommand = \"{cmd}\"\n"),
            format!("fabric.command = \"{cmd}\"\n"),
            format!("fabric = {{ command = \"{cmd}\" }}\n"),
            format!("font_px = 12.0\n\n[fabric]\n# a comment\ncommand = '{cmd}'\n\n[keys]\n"),
            format!("[fabric]\ncommand = \"\"\"{cmd}\"\"\"\n"),
            "[fabric]\ncommand = [\"aterm\", \"link\", \"serve\", \"--fleet\", \"f\", \"--broker\", \"/tmp/b.sock\"]\n"
                .to_string(),
            "[fabric]\ncommand = [\n  \"aterm\", 'link', \"serve\", # the verb\n  \"--fleet\", \"f\",\n  \"--broker\", \"/tmp/b.sock\",\n]\n"
                .to_string(),
        ] {
            assert_eq!(command_in_toml(&text), Ok(Some(want.clone())), "{text}");
        }
        let spaced = "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm";
        assert_eq!(
            command_in_toml(&format!("[fabric]\ncommand = [\"{spaced}\", \"link\"]\n")),
            Ok(Some(vec![spaced.to_string(), "link".to_string()]))
        );
        assert_eq!(
            command_in_toml(&format!("[fabric]\ncommand = \"{spaced} link\"\n")),
            Ok(Some(words(&format!("{spaced} link")))),
        );
        assert_eq!(words(&format!("{spaced} link")).len(), 3);
        for off in [
            "",
            "font_px = 1.0\n",
            "[fabric]\n",
            "[fabric]\ncommand = \"   \"\n",
            "[fabric]\ncommand = []\n",
        ] {
            assert_eq!(command_in_toml(off), Ok(None), "{off:?}");
        }
        for (bad, why) in [
            ("[fabric]\ncommand = 3\n", "is a TOML integer"),
            ("fabric = 1\n", "not a table"),
            ("[fabric\n", "not valid TOML"),
            (
                "[fabric]\ncommand = [\"aterm\", \"\"]\n",
                "element 2 is empty",
            ),
            (
                "[fabric]\ncommand = [\"a\\u0000b\"]\n",
                "element 1 holds a NUL",
            ),
            (
                "[fabric]\ncommand = [\"aterm\", 7]\n",
                "element 2 is not a string",
            ),
            ("[fabric]\ncommand = { a = 1 }\n", "is a TOML table"),
        ] {
            let err = command_in_toml(bad).expect_err(bad);
            assert!(err.contains(why), "{bad:?}: {err}");
        }
    }

    #[test]
    fn the_status_header_names_the_argv_whole() {
        let argv = vec![
            "/a b/aterm".to_string(),
            "link".to_string(),
            "serve".to_string(),
            "--state".to_string(),
            "/s/100%".to_string(),
        ];
        let header = format!(
            "OK state=connected supervised=1 command={} argv={} reason=-",
            crate::pct::encode(&argv.join(" ")),
            crate::pct::encode(&aterm_codec::pct::join_words(&argv))
        );
        assert_eq!(status_argv(&header), Some(argv.clone()));
        let without = header.replace(" argv=", " was=");
        assert_ne!(
            status_argv(&without),
            Some(argv.clone()),
            "command= alone splits"
        );
        let old = words("/usr/bin/aterm link serve --state /s/100%41");
        let old_header = format!(
            "OK state=connected supervised=1 command={} reason=-",
            crate::pct::encode(&old.join(" "))
        );
        assert_eq!(
            status_argv(&old_header),
            Some(old.clone()),
            "an older instance's"
        );
        assert_ne!(
            aterm_codec::pct::split_words(&old.join(" ")),
            old,
            "decoding its words again would change them"
        );
        for none in [
            "OK state=absent supervised=0 command=- argv=- reason=-",
            "OK state=absent supervised=0 command=- reason=-",
            "OK state=absent",
        ] {
            assert_eq!(status_argv(none), None, "{none}");
        }
        assert_eq!(shown_argv(&argv), "'/a b/aterm' link serve --state /s/100%");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(bridge_program(&argv).as_deref(), Some("'/a b/aterm'"));
    }

    #[test]
    fn the_presence_mode_is_read_from_the_toml_and_absent_is_meta() {
        use crate::presence::Mode;
        assert_eq!(
            presence_in_toml("[fabric]\ncommand = \"x\"\npresence = \"minimal\"\n"),
            Ok(Some(Mode::Minimal))
        );
        assert_eq!(
            presence_in_toml("fabric = { presence = \"meta\" }\n"),
            Ok(Some(Mode::Meta))
        );
        for absent in ["", "[fabric]\ncommand = \"x\"\n", "font_px = 1.0\n"] {
            assert_eq!(presence_in_toml(absent), Ok(None), "{absent:?}");
        }
        for bad in [
            "[fabric]\npresence = \"full\"\n",
            "[fabric]\npresence = 1\n",
            "fabric = 1\n",
        ] {
            assert!(presence_in_toml(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sessions_show_the_broadcast_topics_and_a_dash_for_none() {
        let mut r = healthy();
        r.sessions.clear();
        let mut listening = r.instances[0].sessions[0].clone();
        listening.sid = "s-ears".to_string();
        listening.topics = vec!["build.failed".to_string(), "sat-comp".to_string()];
        r.instances[0].sessions.push(listening);
        join_sessions(&mut r, None);
        let json = render_json(&r);
        assert!(
            json.contains("\"topics\":[\"build.failed\",\"sat-comp\"]"),
            "{json}"
        );
        assert!(json.contains("\"topics\":[]"), "{json}");
        r.sessions.push(SessionRow {
            sid: "s-gone".to_string(),
            pid: None,
            node: "n-a".to_string(),
            bus: "gone".to_string(),
            local: None,
            bus_hold: None,
            bus_role: None,
            bus_title: None,
            detail: None,
            phase: None,
            context: None,
        });
        let json = render_json(&r);
        let gone = json
            .split("\"sid\":\"s-gone\"")
            .nth(1)
            .expect("the gone row");
        assert!(gone.contains("\"topics\":null"), "{gone}");
    }

    #[test]
    fn sessions_show_the_presence_rows_meaning_fields() {
        let mut r = healthy();
        let (_, body) = (
            "",
            b"v=1 t=1 inc=1 state=live hold=0 holder=- attention=- role=bus-role \
            detail=claude phase=busy context=12%25 title=bus%20title",
        );
        let local_row = crate::glance::Row::parse("lab", 5, "/f/lab/pub/n-a/s-one/presence", body)
            .expect("a row");
        let remote_row =
            crate::glance::Row::parse("lab", 6, "/f/lab/pub/n-far/s-far/presence", body)
                .expect("a row");
        let old_row = crate::glance::Row::parse(
            "lab",
            7,
            "/f/lab/pub/n-far/s-old/presence",
            b"v=1 t=1 inc=1 state=live hold=0 holder=- attention=-",
        )
        .expect("a row");
        let roster = crate::glance::Glance {
            fleet: "lab".to_string(),
            rows: vec![local_row, remote_row, old_row],
            truncated: false,
        };
        join_sessions(&mut r, Some(&roster));
        assert_eq!(r.sessions.len(), 3, "{:?}", r.sessions);
        let _ = render_text(&r);
        let json = render_json(&r);
        assert!(json.contains("\"phase\":\"busy\""), "{json}");
        assert!(json.contains("\"context\":12"), "{json}");
        assert!(json.contains("\"detail\":\"claude\""), "{json}");
        assert!(json.contains("\"presence\":\"meta\""), "{json}");
        assert!(json.contains("\"title\":\"bus title\""), "{json}");
    }

    #[test]
    fn the_program_row_names_a_development_build_by_cargos_tag() {
        let dir = std::env::temp_dir().join(format!("atfab-prog-{}", std::process::id()));
        let debug = dir.join("target").join("debug");
        std::fs::create_dir_all(&debug).expect("scratch");
        let aterm = debug.join("aterm");
        assert!(!in_cargo_target_dir(&aterm), "untagged");
        std::fs::write(dir.join("target").join("CACHEDIR.TAG"), "Signature: x\n").expect("tag");
        assert!(in_cargo_target_dir(&aterm), "tagged");
        assert!(!in_cargo_target_dir(Path::new("aterm")), "a bare name");
        assert!(
            !in_cargo_target_dir(Path::new("target/debug/aterm")),
            "relative"
        );

        let mut r = healthy();
        r.program_in_target_dir = true;
        r.warnings = warned(&r);
        assert_eq!(exit_of(&r), 0, "informational, never a warning");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_env_overrides_the_file_and_a_missing_file_is_off() {
        let dir = std::env::temp_dir().join(format!("atfab-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("aterm.toml");
        std::fs::write(
            &path,
            "[fabric]\ncommand = \"aterm-link serve --fleet file\"\n",
        )
        .expect("write");
        let (src, c) = resolve_command(None, Some(&path))
            .expect("ok")
            .expect("some");
        assert_eq!(src, Source::File(path.clone()));
        assert!(c.ends_with(&["--fleet".to_string(), "file".to_string()]));
        let (src, c) = resolve_command(Some("aterm-link serve --fleet env"), Some(&path))
            .expect("ok")
            .expect("some");
        assert_eq!(src, Source::Env);
        assert!(c.ends_with(&["--fleet".to_string(), "env".to_string()]));
        let (src, _) = resolve_command(Some("  "), Some(&path))
            .expect("ok")
            .expect("some");
        assert_eq!(src, Source::File(path.clone()), "a blank env is absent");
        assert_eq!(
            resolve_command(None, Some(&dir.join("absent.toml"))),
            Ok(None)
        );
        assert_eq!(resolve_command(None, None), Ok(None));
        std::fs::write(&path, "[fabric\n").expect("write");
        assert!(resolve_command(None, Some(&path)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_spellings_of_the_bridge_command_parse_with_the_bridges_own_parser() {
        let tail = "--fleet lab --broker /tmp/b.sock --cap-file /c --state /s --accept-from n-1";
        for cmd in [
            format!("aterm link serve {tail}"),
            format!("/Users//u/.local/bin/aterm link serve {tail}"),
            format!("/Applications/aterm.app/Contents/MacOS/aterm link serve {tail}"),
            format!("aterm-link serve {tail}"),
            format!("/opt/x/aterm-link   serve  {tail}"),
        ] {
            let cfg = bridge_config(&words(&cmd)).expect(&cmd);
            assert_eq!(cfg.fleet, "lab", "{cmd}");
            assert_eq!(cfg.broker, "/tmp/b.sock");
            assert_eq!(cfg.cap_files, vec!["/c".to_string()]);
            assert_eq!(cfg.state_dir, "/s");
            assert_eq!(cfg.accept_from, vec!["n-1".to_string()]);
        }
        for not_a_bridge in [
            "aterm link broker /tmp/b.sock",
            "serve --fleet x --broker y",
            "/usr/bin/false",
            "aterm-link ls --fleet x --broker y",
        ] {
            assert!(
                serve_flags(&words(not_a_bridge)).is_none(),
                "{not_a_bridge}"
            );
            assert!(bridge_config(&words(not_a_bridge)).is_err());
        }
        assert!(bridge_config(&words("aterm link serve --fleet lab")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_bridges_program_is_the_file_it_runs_under_any_spelling() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-prog")
            .tempdir()
            .expect("scratch");
        let at = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        std::fs::write(at("aterm"), b"#!/bin/sh\n").expect("program");
        std::fs::write(at("other"), b"#!/bin/sh\n").expect("another program");
        std::os::unix::fs::symlink(at("aterm"), at("aterm-link")).expect("alias");
        std::os::unix::fs::symlink(at("aterm"), at("on-path")).expect("link");
        let flags = "--fleet lab --broker /tmp/b.sock --state /s";
        let program = |head: String| bridge_program(&words(&format!("{head} {flags}")));
        let ours = program(format!("{} link serve", at("aterm")));
        assert!(ours.is_some());
        assert_eq!(program(format!("{} serve", at("aterm-link"))), ours);
        assert_eq!(program(format!("{} link serve", at("on-path"))), ours);
        assert_ne!(program(format!("{} link serve", at("other"))), ours);
        let shell = program(format!("/bin/sh -c {} link serve", at("aterm")));
        assert_ne!(shell, ours);
        assert!(
            shell
                .as_deref()
                .is_some_and(|p| p.ends_with(&format!(" -c {}", at("aterm")))),
            "{shell:?}"
        );
        assert_eq!(bridge_program(&words("aterm link ls --fleet lab")), None);
    }

    #[test]
    fn the_process_table_names_the_broker() {
        let ps = "\
    1     0 /sbin/launchd
48078     1 /Users//u/.local/bin/aterm link broker /Users//u/f/bus.sock /Users//u/f/bus.log
15678     1 /x/target/debug/aterm-link broker /tmp/other.sock /tmp/other.log
48224 66439 /Users//u/.local/bin/aterm link serve --fleet local --broker /Users//u/f/bus.sock
15792 15335 /x/aterm-link serve --fleet t --broker /tmp/other.sock
  900   800 vim notes-about-aterm-link-serve.txt
  700     1 /Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm link broker /Users//u/g/bus.sock /Users//u/g/bus.log
";
        let procs = parse_ps(ps);
        assert_eq!(procs.len(), 7);
        assert_eq!(broker_pid(&procs, "/Users//u/f/bus.sock"), Some(48078));
        assert_eq!(broker_pid(&procs, "/tmp/other.sock"), Some(15678));
        assert_eq!(broker_pid(&procs, "/Users//u/g/bus.sock"), Some(700));
        assert_eq!(broker_pid(&procs, "/tmp/none.sock"), None);
        let launchctl =
            "PID\tStatus\tLabel\n-\t0\tcom.apple.x\n48078\t0\tsystems.alab.astream-broker\n";
        assert_eq!(
            parse_launchctl(launchctl, 48078).as_deref(),
            Some("systems.alab.astream-broker")
        );
        assert_eq!(parse_launchctl(launchctl, 1), None);
    }

    #[test]
    fn unread_is_listed_minus_handled_and_posts_are_the_unlanded_rows() {
        let mut s = LocalSession::default();
        let header0 = "OK 2 hold=1 holder=h-andrew seen=3 bus_head=40 dropped=2 pending=1";
        let posts = vec![
            "post 7 to=@s-a kind=task off=- len=5".to_string(),
            "post 8 to=@s-b kind=note off=- len=9".to_string(),
        ];
        let full = vec![
            "msg 2 off=10 t=5 from=h-andrew kind=task trust=human len=3".to_string(),
            "msg 3 off=11 t=6 from=s-x@n-y kind=note trust=agent len=3".to_string(),
            "msg 4 off=12 t=7 from=s-x@n-y kind=ask trust=agent len=3".to_string(),
            "msg 5 off=13 t=8 from=h-andrew kind=task trust=human len=3".to_string(),
            "msg 6 off=14 t=9 from=s-x@n-y kind=note trust=agent re=3 len=3".to_string(),
            "post 7 to=@s-a kind=task off=- len=5".to_string(),
        ];
        fill_inbox(&mut s, header0, &posts, &full);
        assert_eq!(s.hold, Some(1));
        assert_eq!(s.holder, "h-andrew");
        assert_eq!(s.seen, Some(3));
        assert_eq!(s.dropped, Some(2));
        assert_eq!(s.pending, Some(1));
        assert_eq!(s.posts, Some(2));
        assert_eq!(
            s.unhandled.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![4, 5, 6],
            "rows at or below seen= are handled"
        );
        assert_eq!(
            s.unread,
            Some(2),
            "3 above the watermark, 1 of them never listed"
        );
        assert_eq!(s.unhandled[0].kind, "ask");
        assert_eq!(s.unhandled[0].off, 12);
    }

    #[test]
    fn the_hold_reason_is_the_last_hold_transition_in_the_timeline() {
        let tl = vec![
            "event 1 t=1 kind=spawned".to_string(),
            "event 2 t=2 kind=hold 1 reason=main%20broken origin=local".to_string(),
            "event 3 t=3 kind=inbox 1 from=h-a kind=note off=4".to_string(),
            "event 4 t=4 kind=hold 1 reason=fabric-lost origin=fleet".to_string(),
        ];
        assert_eq!(
            last_hold(&tl),
            Some(("fabric-lost".to_string(), "fleet".to_string()))
        );
        let lifted = [
            tl.clone(),
            vec!["event 5 t=5 kind=hold 0 reason=- origin=local".to_string()],
        ]
        .concat();
        assert_eq!(last_hold(&lifted), None);
        assert_eq!(
            last_hold(&tl[..2]),
            Some(("main broken".to_string(), "local".to_string()))
        );
        assert_eq!(last_hold(&tl[..1]), None);
        let both = [
            tl.clone(),
            vec![
                "event 6 t=6 kind=hold 1 reason=stop origin=fleet hold_origin=fleet,local"
                    .to_string(),
            ],
        ]
        .concat();
        assert_eq!(
            last_hold(&both),
            Some(("stop".to_string(), "fleet,local".to_string()))
        );
        let left = [
            both,
            vec!["event 7 t=7 kind=hold 1 reason=mine origin=local hold_origin=local".to_string()],
        ]
        .concat();
        assert_eq!(
            last_hold(&left),
            Some(("mine".to_string(), "local".to_string()))
        );
    }

    #[test]
    fn traffic_rows_classify_every_face_and_carry_no_text_in_their_columns() {
        let t = traffic_of(
            "lab",
            4,
            "/f/lab/in/n-a/s-w/n-a/task",
            b"v=1 t=1757000000000 from=s-me text=secret%20plan",
        );
        assert_eq!(t.from, "s-me@n-a", "the node attests which session spoke");
        assert_eq!(t.to, "s-w@n-a");
        assert_eq!(t.kind, "task");
        assert_eq!(t.trust, "agent");
        assert_eq!(t.len, "secret plan".len());
        assert_eq!(t.t_ms, 1_757_000_000_000);
        let cells = traffic_cells(&t, t.t_ms, 0).join(" ");
        assert!(!cells.contains("secret"), "{cells}");
        assert!(!tail_line(&t, t.t_ms, 0, false).contains("secret"));
        assert!(tail_line(&t, t.t_ms, 0, true).ends_with("text=secret plan"));

        let mirrored = b"v=2 t=1757000000001 ep=1 seq=4 ms=3 op=close node=r2 k=cmd out=fail code=1 src=shell conf=exact";
        let t = traffic_of("lab", 9, "/f/lab/pub/n-a/s-w/ev", mirrored);
        assert_eq!((t.kind.as_str(), t.trust), ("ev", "shell"));
        assert_eq!(t.len, mirrored.len());
        assert_eq!(t.t_ms, 1_757_000_000_001);
        let line = tail_line(&t, t.t_ms, 0, true);
        assert!(line.contains(" op=close node=r2 k=cmd out=fail "), "{line}");
        assert!(!tail_line(&t, t.t_ms, 0, false).contains("op=close"));
        let other = traffic_of("lab", 9, "/f/lab/pub/n-a/s-w/presence", mirrored);
        assert_eq!(other.kind, "presence");

        let h = traffic_of("lab", 5, "/f/lab/in/n-a/s-w/h-andrew/ask", b"v=1 t=1");
        assert_eq!((h.from.as_str(), h.trust), ("h-andrew", "human"));
        let r = traffic_of(
            "lab",
            6,
            "/f/lab/in/n-a/s-w/h-andrew/ask",
            b"v=1 t=1 via=n-b",
        );
        assert_eq!(r.trust, "relayed");
        let forged = traffic_of(
            "lab",
            7,
            "/f/lab/in/n-a/s-w/s-evil/note",
            b"v=1 from=h-andrew",
        );
        assert_eq!(forged.from, "s-evil");

        let p = traffic_of(
            "lab",
            1,
            "/f/lab/pub/n-a/s-w/presence",
            b"v=1 t=2 state=live",
        );
        assert_eq!(
            (p.from.as_str(), p.to.as_str(), p.kind.as_str(), p.len),
            ("s-w@n-a", "-", "presence", 0)
        );
        let n = traffic_of("lab", 2, "/f/lab/pub/n-a/node/presence", b"v=1 t=2");
        assert_eq!(n.from, "n-a");
        let s = traffic_of(
            "lab",
            150,
            "/f/lab/pub/n-a1b2/s-c3d4/say/build.failed",
            b"v=1 t=1 kind=task from=s-c3d4 text=x",
        );
        assert_eq!(
            (s.from.as_str(), s.to.as_str(), s.kind.as_str(), s.trust),
            ("s-c3d4@n-a1b2", "say:build.failed", "task", "agent")
        );
        let bare = traffic_of("lab", 151, "/f/lab/pub/n-a/s-w/say/say", b"v=1 t=1");
        assert_eq!((bare.to.as_str(), bare.kind.as_str()), ("say:say", "note"));
        let ev = traffic_of("lab", 3, "/f/lab/pub/n-a/s-w/ev", b"v=1 t=2");
        assert_eq!(ev.trust, SCREEN);
        let halt = traffic_of("lab", 8, "/f/lab/fleet/h-andrew/halt", b"v=1 t=2 state=on");
        assert_eq!(
            (
                halt.from.as_str(),
                halt.to.as_str(),
                halt.kind.as_str(),
                halt.trust
            ),
            ("h-andrew", "fleet", "halt", "human")
        );
        let odd = traffic_of("lab", 9, "/f/lab/pub/h-andrew/say/note", b"v=1 t=2");
        assert_eq!((odd.to.as_str(), odd.kind.as_str()), ("-", "note"));
        let key = traffic_of("lab", 10, "/f/lab/term/n-a/s-w/in/h-andrew", b"v=1");
        assert_eq!(
            (key.from.as_str(), key.kind.as_str(), key.trust),
            ("h-andrew", "term/in", SCREEN)
        );
        let odd = traffic_of("lab", 11, "/f/lab/zzz/q", b"");
        assert_eq!((odd.kind.as_str(), odd.trust), ("zzz", SCREEN));
        let alien = traffic_of("lab", 11, "/f/other/in/n-a/s-w/n-a/note", b"");
        assert_eq!((alien.kind.as_str(), alien.trust), ("unknown", SCREEN));

        let hostile = traffic_of("lab", 12, "/f/lab/in/n-a/s-w/n-\u{1b}[2J/note", b"v=1");
        let line = tail_line(&hostile, 0, 0, true);
        assert!(!line.contains('\u{1b}'), "{line:?}");
    }

    #[test]
    fn a_utc_offset_is_parsed_from_dates_own_spelling() {
        assert_eq!(parse_utc_offset("-0700"), Some(-7 * 3600));
        assert_eq!(parse_utc_offset("+0530"), Some(5 * 3600 + 30 * 60));
        assert_eq!(parse_utc_offset("UTC"), None);
    }

    const CMD: &str = "aterm link serve --fleet lab --broker /tmp/b.sock --state /s";

    fn healthy() -> Report {
        let session = LocalSession {
            sid: "s-one".to_string(),
            title: "work".to_string(),
            role: "-".to_string(),
            hold: Some(0),
            holder: "-".to_string(),
            seen: Some(3),
            dropped: Some(0),
            pending: Some(0),
            unread: Some(0),
            posts: Some(0),
            ..LocalSession::default()
        };
        Report {
            source: Source::File(PathBuf::from("/c/aterm.toml")),
            command: words(CMD),
            program_in_target_dir: false,
            program: bridge_program(&words(CMD)),
            config_path: Some(PathBuf::from("/c/aterm.toml")),
            cfg: bridge_config(&words(CMD)).expect("a bridge command"),
            presence: crate::presence::Mode::Meta,
            receipts: false,
            node: Some("n-a".to_string()),
            caps: vec![("/c/node.cap".to_string(), Ok(8))],
            broker: BrokerView {
                endpoint: "/tmp/b.sock".to_string(),
                reachable: true,
                attached: true,
                head: Some(10),
                ..BrokerView::default()
            },
            instances: vec![InstanceView {
                pid: 100,
                sock: "/run/aterm-100.sock".to_string(),
                fabric: Some("connected".to_string()),
                rtt_ms: Some(2),
                link_age_ms: Some(3_000),
                supervised: Some(true),
                command: Some(words(CMD)),
                program: bridge_program(&words(CMD)),
                bridge_pids: vec![101],
                node: Some("n-a".to_string()),
                state_dir: Some("/s".to_string()),
                sessions: vec![session],
                ..InstanceView::default()
            }],
            discovery_error: None,
            sessions: Vec::new(),
            nodes: Vec::new(),
            roster_rows: Some(0),
            roster_truncated: false,
            subjects: Some(Ok(3)),
            exited: 0,
            traffic: Vec::new(),
            overdue: Vec::new(),
            warnings: Vec::new(),
            now_ms: 100 * 60 * 1000,
        }
    }

    fn warned(r: &Report) -> Vec<String> {
        warnings(r, &BTreeMap::new(), &[])
    }

    #[cfg(unix)]
    fn unix_config(broker: &std::path::Path) -> Config {
        Config {
            fleet: "f".to_string(),
            broker: broker.to_string_lossy().into_owned(),
            cap_files: Vec::new(),
            state_dir: String::new(),
            accept_from: Vec::new(),
            sock: None,
            token: None,
            presence: crate::presence::Mode::Meta,
            receipts: false,
            log: crate::mirror::LogMode::Off,
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_a_locally_served_broker_is_diagnosed_from_local_supervisor_stderr() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("broker-role")
            .tempdir()
            .unwrap();
        let root = dir.path();
        std::fs::write(root.join("broker.err"), "old-local-broker-refusal\n").unwrap();
        let unix = unix_config(&root.join("bus.sock"));
        assert!(
            supervisor_last_word(&unix).is_some(),
            "the local socket keeps its supervisor's word"
        );
        let mut relative = unix.clone();
        relative.broker = "bus.sock".to_string();
        relative.cap_files = vec![root.join("node.cap").to_string_lossy().into_owned()];
        assert_eq!(
            supervisor_last_word(&relative),
            None,
            "a cap directory alone does not prove where the broker runs"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_broker_in_another_protocol_is_named_with_both_versions() {
        use astream_broker::proto::PROTO_VERSION;
        use astream_broker::proto::{encode_response, read_frame, write_frame, Response};
        let probe_against = |version: u8| {
            let dir = aterm_tempfile::Builder::new()
                .prefix("al-proto")
                .tempdir()
                .expect("scratch");
            let sock = dir.path().join("bus.sock");
            let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
            std::thread::spawn(move || {
                let Ok((mut c, _)) = listener.accept() else {
                    return;
                };
                if let Ok(Some(_hello)) = read_frame(&mut c) {
                    let mut reply = encode_response(&Response::Error {
                        code: astream_broker::proto::ERR_REFUSED,
                        msg: "malformed request".to_string(),
                    });
                    reply[0] = version;
                    let _ = write_frame(&mut c, &reply);
                }
            });
            let (view, _) = probe_broker(&unix_config(&sock), &[]);
            (view, dir)
        };
        let (view, _dir) = probe_against(PROTO_VERSION + 1);
        let skew = view.protocol.expect("the mismatch is named");
        assert_eq!(
            (skew.broker, skew.client),
            (PROTO_VERSION + 1, PROTO_VERSION)
        );
        assert!(!view.reachable, "{view:?}");
        assert!(
            view.error
                .as_deref()
                .is_some_and(|e| e.starts_with("a broker answered in another astream protocol")),
            "{view:?}"
        );
        let mut r = healthy();
        r.broker = view;
        let w = warned(&r);
        let warning = w
            .iter()
            .find(|l| l.contains("speaks astream protocol"))
            .unwrap_or_else(|| panic!("no protocol warning: {w:#?}"));
        assert!(
            warning.contains(&format!(
                "protocol {} and this aterm speaks {PROTO_VERSION}",
                PROTO_VERSION + 1
            )) && !warning.contains("does not answer"),
            "{warning}"
        );
        assert!(
            crate::enable::fix_for(warning, &crate::enable::Role::default())
                .contains("different releases"),
            "{}",
            crate::enable::fix_for(warning, &crate::enable::Role::default())
        );
        let summary = summary_of(&r.broker);
        assert_eq!(summary.level, SummaryLevel::Warn);
        assert!(!summary.unmeasured, "a protocol mismatch was measured");
        assert!(
            summary.text.contains(&format!(
                "speaks astream protocol {} and this aterm speaks {PROTO_VERSION}",
                PROTO_VERSION + 1
            )) && !summary.text.contains("does not answer"),
            "{}",
            summary.text
        );
        let (view, _dir) = probe_against(PROTO_VERSION);
        assert!(view.protocol.is_none(), "{view:?}");
        assert!(
            view.error
                .as_deref()
                .is_some_and(|e| e.contains("no broker answered: malformed request")),
            "{view:?}"
        );
        assert!(
            summary_of(&view).text.contains("does not answer"),
            "{}",
            summary_of(&view).text
        );
    }

    /// While doctor withholds its argument reads, a configured fabric's row says why and
    /// never reads the process table; an off fabric and a config fault answer as ever.
    /// NEGATIVE CONTROL: with nothing withheld the same command reads the table and probes.
    #[test]
    fn a_withheld_fabric_row_says_why_and_reads_no_process_table() {
        let absent = std::env::temp_dir().join(format!("atl-withheld-{}", std::process::id()));
        let bridge = words(&format!(
            "aterm link serve --fleet lab --broker {b} --cap-file /c --state {s}",
            b = absent.join("b.sock").display(),
            s = absent.join("state").display()
        ));
        let unread = || -> Vec<Proc> { panic!("the process table was read while withheld") };
        let why = Some("the pool is full");
        let summary = summary_for(Ok(Some(bridge.clone())), why, unread);
        assert_eq!(summary.level, SummaryLevel::Note);
        assert!(summary.unmeasured);
        assert_eq!(summary.text, "not read (the pool is full)");
        assert!(summary_for(Ok(None), why, unread).text.starts_with("off: "));
        assert_eq!(
            summary_for(Err("bad toml".into()), why, unread).level,
            SummaryLevel::Warn
        );
        assert_eq!(
            summary_for(Ok(Some(words("/usr/bin/false"))), why, unread).level,
            SummaryLevel::Warn
        );

        let read = std::cell::Cell::new(false);
        let probed = summary_for(Ok(Some(bridge)), None, || {
            read.set(true);
            Vec::new()
        });
        assert!(read.get(), "CONTROL: nothing withheld reads the table");
        assert!(probed.text.contains("does not answer"), "{}", probed.text);
    }

    #[cfg(unix)]
    #[test]
    fn a_broker_that_does_not_answer_is_shown_with_its_supervisors_last_line() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-err")
            .tempdir()
            .expect("scratch");
        let cfg = unix_config(&dir.path().join("bus.sock"));
        let (view, _) = probe_broker(&cfg, &[]);
        assert_eq!(view.last_word, None, "nothing was written");
        std::fs::write(
            dir.path().join("broker.err"),
            "aterm link broker: open bus.log: earlier\n\
             aterm link broker: open bus.log: record format v9: refused\n",
        )
        .expect("broker.err");
        let (view, _) = probe_broker(&cfg, &[]);
        assert!(!view.reachable);
        let mut r = healthy();
        r.broker = view;
        let w = warned(&r);
        let warning = w
            .iter()
            .find(|l| l.contains("does not answer (connect:"))
            .unwrap_or_else(|| panic!("no unreachable warning: {w:#?}"));
        assert!(
            warning.contains("; its supervisor's broker.err, written ")
                && warning
                    .ends_with("ago: aterm link broker: open bus.log: record format v9: refused"),
            "{warning}"
        );
        assert!(
            crate::enable::fix_for(warning, &crate::enable::Role::default())
                .contains("fix that first"),
            "{}",
            crate::enable::fix_for(warning, &crate::enable::Role::default())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_poisoned_brokers_exit_line_gets_its_remedy_at_real_lengths() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-poison")
            .tempdir()
            .expect("scratch");
        let role = crate::enable::Role::default();
        let fix = |w: &str| crate::enable::fix_for(w, &role);
        let root = crate::enable::default_root(Path::new("/Users//someone"));
        let log = root.join("bus.log").to_string_lossy().into_owned();
        let argv = format!(
            "{0}/bin/aterm link broker {1}/bus.sock {log} --secret-file {1}/mint.secret \
             --durability relaxed",
            "/Applications/aterm.app/Contents/MacOS",
            root.display()
        );
        let real = crate::broker_health::astream_poisoned_reason();
        let long = format!("{real}; {}", "and more of astream's words ".repeat(40));
        let warnings = |last: &str| {
            std::fs::write(dir.path().join("broker.err"), format!("{last}\n")).expect("err");
            let mut r = healthy();
            r.broker.reachable = false;
            r.broker.error = Some("connect: No such file or directory (os error 2)".into());
            r.broker.last_word = crate::enable::supervisor_last_word(dir.path());
            let inst = &mut r.instances[0];
            inst.fabric = Some("stalled".to_string());
            inst.reason = Some("no-socket".to_string());
            let w = warned(&r);
            let pick = |needle: &str| {
                w.iter()
                    .find(|l| l.contains(needle))
                    .unwrap_or_else(|| panic!("no {needle:?} warning: {w:#?}"))
                    .clone()
            };
            [
                pick("does not answer (connect:"),
                pick("broker link is down"),
            ]
        };
        for reason in [real.as_str(), long.as_str()] {
            let supervised = crate::broker_health::poisoned_line(&log, reason, None);
            assert!(
                supervised.chars().count() > 256,
                "PRECONDITION: {supervised}"
            );
            for w in warnings(&supervised) {
                assert!(w.contains("supervisor's broker.err"), "{w}");
                let f = fix(&w);
                assert!(
                    f.starts_with("the broker exited over a poisoned log for its supervisor")
                        && !f.contains("fix that first")
                        && !f.contains("--repair"),
                    "{f}\nfor: {w}"
                );
            }
            let alone = crate::broker_health::poisoned_line(&log, reason, Some(&argv));
            for w in warnings(&alone) {
                let f = fix(&w);
                assert!(
                    f.starts_with(
                        "the broker exited over a poisoned log, and no `aterm fabric on`"
                    ) && f.contains("broker.err")
                        && !f.contains("fix that first"),
                    "{f}\nfor: {w}"
                );
            }
            let other = format!("aterm link broker: open {log}: {reason}");
            for w in warnings(&other) {
                assert!(fix(&w).contains("fix that first"), "{}", fix(&w));
            }
        }
    }

    #[test]
    fn the_report_says_what_the_broker_says_of_its_log() {
        use astream_broker::{FlushCounts, HelloHealth};
        let role = crate::enable::Role::default();
        let fix = |w: &str| crate::enable::fix_for(w, &role);
        let ours = |w: &[String]| -> Vec<String> {
            w.iter()
                .filter(|l| l.contains(POISONED_WARNING) || l.contains(FLUSH_FAILED_WARNING))
                .cloned()
                .collect()
        };
        let row = |r: &Report, key: &str| -> Option<String> {
            render_text(r)
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("{key} ")))
                .map(|l| l.trim().to_string())
        };
        let mut r = healthy();
        r.broker.health = Some(HelloHealth {
            poisoned: None,
            flushes: FlushCounts {
                full: 3,
                barrier: 9,
                ..FlushCounts::default()
            },
        });
        assert!(ours(&warned(&r)).is_empty(), "{:?}", warned(&r));
        let health = row(&r, "health").expect("a health row");
        assert!(
            health.ends_with(
                "ok, not poisoned — flushes since it opened: 3 full, 9 barrier, 0 skipped, 0 \
                 failed"
            ),
            "{health}"
        );
        assert_eq!(summary_of(&r.broker).level, SummaryLevel::Ok);
        assert!(
            render_json(&r).contains(
                "\"in_band\":{\"poisoned\":null,\"flushes\":{\"full\":3,\"barrier\":9,\
                 \"skipped\":0,\"failed\":0}}"
            ),
            "{}",
            render_json(&r)
        );

        if let Some(h) = r.broker.health.as_mut() {
            h.flushes.failed = 2;
        }
        let w = ours(&warned(&r));
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(
            w[0].contains(&format!("{FLUSH_FAILED_WARNING} 2 time(s)")),
            "{}",
            w[0]
        );
        assert!(fix(&w[0]).starts_with("check the disk"), "{}", fix(&w[0]));
        for said in [w[0].as_str(), fix(&w[0]).as_str()] {
            assert!(
                !said.contains("lost nothing") && !said.contains("nothing acked was lost"),
                "{said}"
            );
        }
        assert_eq!(summary_of(&r.broker).level, SummaryLevel::Ok);

        if let Some(h) = r.broker.health.as_mut() {
            h.poisoned = Some("a flush failed (EIO)\u{1b}[2J".to_string());
        }
        let w = ours(&warned(&r));
        assert_eq!(w.len(), 1, "the poison, not the disk too: {w:?}");
        assert!(
            w[0].contains(&format!("{POISONED_WARNING} (a flush failed (EIO)")),
            "{}",
            w[0]
        );
        assert!(
            !w[0].contains('\u{1b}'),
            "astream's words are made safe: {}",
            w[0]
        );
        let f = fix(&w[0]);
        assert!(
            f.starts_with("the broker's log is poisoned")
                && !f.contains("node.cap")
                && !f.contains("--repair"),
            "{f}"
        );
        let health = row(&r, "health").expect("a health row");
        assert!(
            health.contains("POISONED (a flush failed (EIO)"),
            "{health}"
        );
        let summary = summary_of(&r.broker);
        assert_eq!(summary.level, SummaryLevel::Warn);
        assert!(summary.text.contains(POISONED_WARNING), "{}", summary.text);
        assert!(!summary.unmeasured, "a poisoned log is a measured failure");
        assert!(
            render_json(&r).contains("\"in_band\":{\"poisoned\":\"a flush failed (EIO)"),
            "{}",
            render_json(&r)
        );

        r.broker.health = None;
        assert!(ours(&warned(&r)).is_empty());
        let health = row(&r, "health").expect("a health row");
        assert!(health.contains("carries no health entry"), "{health}");
        assert_eq!(summary_of(&r.broker).level, SummaryLevel::Note);
        assert!(summary_of(&r.broker).unmeasured);

        r.broker.reachable = false;
        r.broker.error = Some("connect: refused".to_string());
        assert_eq!(row(&r, "health"), None);
        let summary = summary_of(&r.broker);
        assert_eq!(summary.level, SummaryLevel::Warn);
        assert!(
            summary.text.contains("does not answer (connect: refused)"),
            "{}",
            summary.text
        );
    }

    #[test]
    fn the_holds_row_is_current_only_for_the_serving_pid() {
        use astream_broker::{BrokerHealth, HealthRead, HealthWriter};
        let mut r = healthy();
        let row = |r: &Report| holds_row(&r.broker, 61_000);
        assert_eq!(row(&r), None, "no log named, no row");
        r.broker.log = Some(PathBuf::from("/r/bus.log"));
        r.broker.pid = Some(42);
        assert_eq!(
            row(&r).as_deref(),
            Some(
                "- (no health snapshot beside /r/bus.log: a broker older than this aterm keeps \
                 none, and one of this build that could not write it says why on its stderr, the \
                 supervisor's broker.err)"
            )
        );
        r.broker.snapshot = Some(Err("/r/bus.log.health: not a snapshot".to_string()));
        assert!(row(&r)
            .expect("a row")
            .contains("could not be read: /r/bus.log.health"));
        let mut h = BrokerHealth::default();
        h.pid = 42;
        h.written_at_ms = 1_000;
        h.retained_records = 5;
        h.retained_bytes = 2048;
        h.distinct_subjects = 3;
        h.active_conns = 4;
        let read = |writer, h: &BrokerHealth| HealthRead {
            health: h.clone(),
            writer,
        };
        r.broker.snapshot = Some(Ok(read(HealthWriter::Serving, &h)));
        assert_eq!(
            row(&r).as_deref(),
            Some(
                "5 record(s) in 2.0 KiB, 3 subject(s), 0 pending will(s), 0 group commit(s); 4 \
                 connection(s), 0 parked tail(s) (astream's <log>.health, written 1m ago)"
            )
        );
        assert!(r.broker.snapshot_now().is_some());
        r.broker.pid = Some(43);
        assert!(row(&r)
            .expect("a row")
            .starts_with("- (astream's <log>.health is pid 42's"));
        assert!(r.broker.snapshot_now().is_none());
        r.broker.pid = Some(42);
        h.poisoned = Some("a flush failed".to_string());
        r.broker.snapshot = Some(Ok(read(HealthWriter::Gone, &h)));
        let gone = row(&r).expect("a row");
        assert!(
            gone.starts_with("- (astream's <log>.health is the last word of pid 42")
                && gone.contains("that broker's log was POISONED (a flush failed)"),
            "{gone}"
        );
        assert!(r.broker.snapshot_now().is_none());
        assert_eq!(
            r.broker.poisoned(),
            None,
            "a gone broker's poison is not this one's"
        );
        assert!(
            !warned(&r).iter().any(|w| w.contains(POISONED_WARNING)),
            "{:?}",
            warned(&r)
        );
        r.broker.snapshot = Some(Ok(read(HealthWriter::Serving, &h)));
        assert_eq!(r.broker.poisoned(), Some("a flush failed"));
        assert!(render_json(&r).contains("\"writer\":\"serving\",\"current\":true"));
    }

    #[cfg(unix)]
    #[test]
    fn a_bridge_dial_nothing_answers_quotes_the_brokers_last_word() {
        use std::io::ErrorKind as K;
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-dial")
            .tempdir()
            .expect("scratch");
        let cfg = unix_config(&dir.path().join("bus.sock"));
        let role = crate::enable::Role::default();
        let report = |kind: K, reason: &str, command: Option<String>| {
            let line = crate::cli::log_refusal_remedy(kind, "/r/bus.log").expect("a remedy");
            std::fs::write(
                dir.path().join("broker.err"),
                format!("aterm link broker: open /r/bus.log: x — {line}\n"),
            )
            .expect("broker.err");
            let (view, _) = probe_broker(&cfg, &[]);
            assert!(!view.reachable && view.last_word.is_some(), "{view:?}");
            let mut r = healthy();
            r.broker = view;
            let inst = &mut r.instances[0];
            inst.fabric = Some("stalled".to_string());
            inst.reason = Some(reason.to_string());
            if let Some(c) = command {
                inst.command = Some(words(&c));
            }
            let w = warned(&r);
            let bridge = w
                .iter()
                .find(|l| l.contains("broker link is down"))
                .unwrap_or_else(|| panic!("no bridge warning: {w:#?}"))
                .clone();
            let broker = w
                .iter()
                .find(|l| l.contains("does not answer (connect:"))
                .unwrap_or_else(|| panic!("no broker warning: {w:#?}"))
                .clone();
            (bridge, broker)
        };
        let fix = |w: &str| crate::enable::fix_for(w, &role);
        for reason in ["no-socket", "refused"] {
            let (bridge, broker) = report(K::InvalidData, reason, None);
            assert!(
                bridge.contains("; its supervisor's broker.err, written ")
                    && bridge.contains(crate::cli::CORRUPT_LOG_REMEDY),
                "{bridge}"
            );
            assert!(fix(&bridge).starts_with(&fix(&broker)), "{}", fix(&bridge));
            assert!(fix(&bridge).contains("--repair"), "{}", fix(&bridge));
            let (bridge, broker) = report(K::Unsupported, reason, None);
            assert!(bridge.contains(crate::cli::NEWER_LOG_REMEDY), "{bridge}");
            assert!(
                fix(&bridge).starts_with(&fix(&broker)) && !fix(&bridge).contains("--repair"),
                "{}",
                fix(&bridge)
            );
        }
        let other = CMD.replace("/tmp/b.sock", "/elsewhere/b.sock");
        assert_ne!(other, CMD, "PRECONDITION: the command names /tmp/b.sock");
        let (bridge, _) = report(K::InvalidData, "refused", Some(other));
        assert!(!bridge.contains("broker.err"), "{bridge}");
        assert!(fix(&bridge).contains("starts it"), "{}", fix(&bridge));
        let (bridge, _) = report(K::InvalidData, "no-ack", None);
        assert!(!bridge.contains("broker.err"), "{bridge}");
    }

    #[cfg(unix)]
    #[test]
    fn a_lapsed_ring_is_named_and_its_fix_is_a_renewal() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-lapsed")
            .tempdir()
            .expect("scratch");
        let secret = vec![0x5a_u8; 32];
        let broker = astream_broker::Broker::open_guarded(dir.path().join("b.log"), secret.clone())
            .expect("a guarded broker");
        let socket = dir.path().join("b.sock");
        let _server = broker.serve(&socket).expect("serve the broker");
        let cap = dir.path().join("node.cap");
        let mut cfg = unix_config(&socket);
        cfg.cap_files = vec![cap.to_string_lossy().into_owned()];
        let probe = |key: &[u8], exp: &str| {
            let lines: String = crate::enable::node_grants("f", "n-a")
                .iter()
                .map(|g| {
                    let c = astream_cap::mint(key, &g.replacen(':', exp, 1)).expect("mint");
                    format!("{}\n", astream_cap::capfile::format_line(&c.filter, &c.tag))
                })
                .collect();
            std::fs::write(&cap, lines).expect("the cap file");
            let (view, _) = probe_broker(&cfg, &[]);
            let mut r = healthy();
            r.broker = view;
            r
        };
        let r = probe(&secret, ",exp=1:");
        assert!(
            r.broker.reachable && r.broker.cap_expired && !r.broker.read_ok(),
            "{:?}",
            r.broker
        );
        let w = warned(&r);
        let warning = w
            .iter()
            .find(|l| l.contains(CAP_LAPSED))
            .unwrap_or_else(|| panic!("no lapsed warning: {w:#?}"));
        assert!(!warning.contains("could not be read"), "{warning}");
        let here = crate::enable::fix_for(warning, &crate::enable::Role::default());
        assert!(
            here.contains("lapsed") && here.contains("move <root>/node.cap aside"),
            "{here}"
        );

        let r = probe(&[0xa5_u8; 32], ":");
        assert!(
            r.broker.reachable && !r.broker.cap_expired,
            "{:?}",
            r.broker
        );
        let w = warned(&r);
        assert!(
            w.iter().any(|l| l.contains("could not be read"))
                && !w.iter().any(|l| l.contains(CAP_LAPSED)),
            "{w:#?}"
        );
        let r = probe(&secret, ":");
        assert!(
            r.broker.read_ok() && !r.broker.cap_expired,
            "{:?}",
            r.broker
        );
    }

    #[test]
    fn nodes_lists_every_node_and_marks_this_one() {
        let mut r = healthy();
        let body = b"v=1 t=1 inc=1 state=live hold=0 holder=- attention=-";
        let roster = crate::glance::Glance {
            fleet: "lab".to_string(),
            rows: vec![
                crate::glance::Row::parse(
                    "lab",
                    1,
                    "/f/lab/pub/n-z/node/presence",
                    b"v=1 t=1 inc=1 state=live fabric=connected host=m7",
                )
                .expect("row"),
                crate::glance::Row::parse(
                    "lab",
                    2,
                    "/f/lab/pub/n-a/node/presence",
                    b"v=1 t=1 inc=1 state=live fabric=connected host=m100",
                )
                .expect("row"),
                crate::glance::Row::parse("lab", 3, "/f/lab/pub/n-z/s-far/presence", body)
                    .expect("row"),
                crate::glance::Row::parse("lab", 4, "/f/lab/pub/n-a/s-one/presence", body)
                    .expect("row"),
            ],
            truncated: false,
        };
        join_sessions(&mut r, Some(&roster));
        let json = render_json(&r);
        assert!(
            json.contains("\"this\":true") && json.contains("\"where\":\"remote\""),
            "{json}"
        );
        let mut lonely = healthy();
        lonely.nodes = vec![NodeRow {
            node: "n-z".to_string(),
            host: "m7".to_string(),
            state: "live".to_string(),
            fabric: "connected".to_string(),
            ..NodeRow::default()
        }];
        assert_eq!(node_where(&lonely, "n-a"), "this");
        assert_eq!(node_where(&lonely, "n-z"), "remote");
        let text = render_text(&lonely);
        assert_eq!(
            text.matches(REMOTE_NODE_NOTE).count(),
            1,
            "said once, under NODES:\n{text}"
        );
        lonely.instances[0].node = Some("n-z".to_string());
        assert_eq!(node_where(&lonely, "n-z"), "local");
        assert_eq!(node_where(&lonely, "n-q"), "remote");
        let text = render_text(&lonely);
        assert!(!text.contains(REMOTE_NODE_NOTE), "no remote row:\n{text}");
        assert!(text.contains("n-z"), "{text}");
    }

    #[test]
    fn the_readable_faces_are_the_caps_filters_under_the_fleet() {
        let dir = std::env::temp_dir().join(format!("atfab-faces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let cap = dir.join("node.cap");
        let lines: String = crate::enable::node_grants("lab", "n-a")
            .iter()
            .chain(std::iter::once(&"ro:/f/other/pub/>".to_string()))
            .map(|g| format!("{g} {}\n", "00".repeat(32)))
            .collect();
        std::fs::write(&cap, lines).expect("cap");
        let faces = readable_faces("lab", &[cap.to_string_lossy().into_owned()]);
        assert_eq!(
            faces,
            [
                "/f/lab/fleet/>",
                "/f/lab/in/*/*/n-a/*",
                "/f/lab/in/n-a/>",
                "/f/lab/pub/>",
                "/f/lab/pub/n-a/>",
                "/f/lab/term/n-a/*/screen",
                "/f/lab/term/n-a/>",
            ],
            "no cur/ group names and no other fleet's face"
        );
        assert_eq!(
            in_filters("lab", Some(&faces)),
            ["/f/lab/in/*/*/n-a/*", "/f/lab/in/n-a/>"]
        );
        assert_eq!(in_filters("lab", None), ["/f/lab/in/>"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_process_whose_executable_was_replaced_is_named() {
        let root = std::env::temp_dir().join(format!("atfab-exe-{}", std::process::id()));
        let dir = root.join("a b");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&dir).expect("scratch");
        let exe = dir.join("slp");
        let copy = |to: &Path| aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), to);
        copy(&exe);
        let run = |path: &Path| {
            aterm_uds::spawnfd::command(path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|e| panic!("run {}: {e}", path.display()))
        };
        let mut child = run(&exe);
        let link = root.join("slp-link");
        std::os::unix::fs::symlink(&exe, &link).expect("link it");
        let mut linked = run(&link);
        let fed = [
            aterm_uds::deadline::cat_echoes(&mut child),
            aterm_uds::deadline::cat_echoes(&mut linked),
        ];
        let shown = exe.to_string_lossy().into_owned();
        let via = link.to_string_lossy().into_owned();
        let before = replaced_build(child.id(), &shown);
        let before_via = replaced_build(linked.id(), &via);
        let own = {
            use std::os::unix::fs::MetadataExt;
            std::fs::symlink_metadata(&link).expect("the link").ino()
        };
        let running_via = running_image_inode(linked.id());
        copy(&dir.join("slp.new"));
        std::fs::rename(dir.join("slp.new"), &exe).expect("rename a new build over it");
        let after = replaced_build(child.id(), &shown);
        let after_via = replaced_build(linked.id(), &via);
        let relative = replaced_build(child.id(), "slp");
        #[cfg(target_os = "macos")]
        let read =
            [child.id(), linked.id()].map(|pid| (running_image_inode(pid), lsof_image_inode(pid)));
        for mut c in [child, linked] {
            let _ = c.kill();
            let _ = c.wait();
        }
        drop(fed);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(before, None, "it runs the file at its path");
        assert_eq!(
            after.as_deref(),
            Some(shown.as_str()),
            "a new file is at its path"
        );
        assert_eq!(before_via, None, "it runs the file its link names");
        assert_eq!(
            after_via.as_deref(),
            Some(via.as_str()),
            "a new file is behind its link"
        );
        assert!(
            running_via.is_some_and(|i| i != own),
            "{running_via:?} vs the link's {own}"
        );
        assert_eq!(relative, None, "a relative argv names no file");
        #[cfg(target_os = "macos")]
        for (ours, lsof) in read {
            assert!(ours.is_some(), "an image read");
            assert_eq!(ours, lsof, "the image lsof names");
        }
        let lsof =
            "p57558\nftxt\ni366658160\nn/x/slp\nftxt\ni1152921500312609216\nn/usr/lib/dyld\n";
        assert_eq!(lsof_first_inode(lsof), Some(366_658_160));
        assert_eq!(lsof_first_inode(""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_process_table_is_the_one_ps_prints() {
        use std::collections::{BTreeMap, BTreeSet};
        let sock = format!("/tmp/kproc-s\u{f6}k-{}.sock", std::process::id());
        let sock = sock.as_str();
        let mut fixture = aterm_uds::spawnfd::command("/bin/bash");
        fixture
            .arg0("aterm")
            .args(["-c", "read x", "link", "broker", sock, "a\nb"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut fixture = fixture.spawn().expect("spawn bash");
        let started = std::time::Instant::now();
        while broker_pid(&process_table(), sock) != Some(fixture.id()) {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(60),
                "bash's exec: not within 60s"
            );
            std::thread::yield_now();
        }
        let uid = aterm_sysprobe::kproc::record(std::process::id())
            .expect("this process")
            .uid;
        let mine: BTreeSet<u32> = aterm_sysprobe::kproc::records()
            .expect("records")
            .iter()
            .filter(|r| r.uid == uid)
            .map(|r| r.pid)
            .collect();
        let keyed =
            |t: Vec<Proc>| -> BTreeMap<u32, Proc> { t.into_iter().map(|p| (p.pid, p)).collect() };
        let a = keyed(process_table());
        let out = aterm_uds::spawnfd::command("/bin/ps")
            .args(["-A", "-ww", "-o", "pid=", "-o", "ppid=", "-o", "command="])
            .env("LC_ALL", "C.UTF-8")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .expect("run ps");
        let oracle = parse_ps(&String::from_utf8_lossy(&out.stdout));
        let b = keyed(process_table());
        let id = fixture.id();
        drop(fixture.stdin.take());
        let _ = fixture.kill();
        let _ = fixture.wait();
        let mut compared = BTreeSet::new();
        for row in &oracle {
            let (Some(x), Some(y)) = (a.get(&row.pid), b.get(&row.pid)) else {
                continue;
            };
            if x != y {
                continue;
            }
            if mine.contains(&row.pid) {
                assert_eq!(x, row, "the row of {}", row.pid);
            } else {
                assert_eq!(
                    (x.pid, x.ppid),
                    (row.pid, row.ppid),
                    "the row of {}",
                    row.pid
                );
            }
            compared.insert(row.pid);
        }
        assert!(compared.contains(&id) && compared.contains(&std::process::id()));
        assert_eq!(
            a[&id].cmd,
            format!("aterm -c read x link broker {sock} a\\012b")
        );
        assert_eq!(broker_pid(&oracle, sock), Some(id));
        assert!(
            compared.len() * 2 >= oracle.len(),
            "{} of {} rows compared",
            compared.len(),
            oracle.len()
        );
    }

    #[test]
    fn a_head_query_that_failed_is_not_a_broker_that_answered() {
        let mut r = healthy();
        r.broker.head = None;
        r.broker.rtt_ms = None;
        r.broker.error = Some(
            "head query: unauthorized: capability does not grant this subject/filter".to_string(),
        );
        let w = warned(&r);
        assert!(
            !w.is_empty(),
            "the bus could not be read and NOTHING warns: {w:#?}"
        );
        r.warnings = w;
        assert_eq!(exit_of(&r), 1, "a bus that cannot be read is not healthy");
    }

    #[test]
    fn a_node_the_bus_calls_away_is_a_warning_while_its_instance_says_connected() {
        let mut r = healthy();
        r.nodes = vec![NodeRow {
            node: "n-a".to_string(),
            host: "-".to_string(),
            state: "gone".to_string(),
            fabric: "disconnected".to_string(),
            ..NodeRow::default()
        }];
        one(&r, "state=away");

        let mut ok = healthy();
        ok.nodes = vec![
            NodeRow {
                node: "n-a".to_string(),
                host: "-".to_string(),
                state: "live".to_string(),
                fabric: "connected".to_string(),
                ..NodeRow::default()
            },
            NodeRow {
                node: "n-far".to_string(),
                host: "box".to_string(),
                state: "gone".to_string(),
                fabric: "disconnected".to_string(),
                ..NodeRow::default()
            },
        ];
        assert_eq!(warned(&ok), Vec::<String>::new());
    }

    fn one(r: &Report, needle: &str) {
        let w = warned(r);
        assert!(
            w.iter().any(|l| l.contains(needle)),
            "no warning says {needle:?}: {w:#?}"
        );
    }

    #[test]
    fn every_condition_that_makes_connected_a_lie_or_loses_mail_is_a_warning() {
        let mut r = healthy();
        assert_eq!(warned(&r), Vec::<String>::new());
        r.warnings = warned(&r);
        assert_eq!(exit_of(&r), 0);

        let mut r = healthy();
        r.broker.reachable = false;
        r.broker.attached = false;
        r.broker.error = Some("connect: Connection refused".to_string());
        one(&r, "connect: Connection refused");
        one(&r, "instance 100 says fabric=connected");

        let mut r = healthy();
        r.instances[0].fabric = Some("stalled".to_string());
        r.instances[0].reason = Some("refused".to_string());
        one(&r, "fabric=stalled reason=refused");
        r.instances[0].link_age_ms = None;
        r.instances[0].reason = Some("no-socket".to_string());
        one(&r, "fabric=stalled reason=no-socket");

        let mut r = healthy();
        r.instances[0].build = Some("0.104.0+g0123456789ab".to_string());
        r.instances[0].bridge_build = Some("0.104.0+g0123456789ab".to_string());
        assert_eq!(warned(&r), Vec::<String>::new());
        r.instances[0].bridge_build = None;
        assert_eq!(warned(&r), Vec::<String>::new());
        r.instances[0].bridge_build = Some("0.98.0+gfedcba987654".to_string());
        one(
            &r,
            "instance 100's bridge is another build than the instance: bridge \
             0.98.0+gfedcba987654, instance 0.104.0+g0123456789ab",
        );
        let mut r = healthy();
        r.instances[0].lane_faults = Some(0);
        assert_eq!(warned(&r), Vec::<String>::new());
        r.instances[0].lane_faults = Some(3);
        one(&r, "instance 100 could not parse 3 of its bridge's lines");

        let mut r = healthy();
        r.instances[0].sessions[0].dropped = Some(2);
        one(&r, "@s-one (instance 100)");

        let mut r = healthy();
        r.instances[0].sessions[0].hold = Some(1);
        r.instances[0].sessions[0].hold_why =
            Some(("fabric-lost".to_string(), "fleet".to_string()));
        one(&r, "reason=fabric-lost origin=fleet");
        r.instances[0].sessions[0].hold_why = None;
        one(&r, "@s-one");

        let mut r = healthy();
        r.instances[0].sessions[0].unhandled = vec![
            MsgMeta {
                id: 4,
                off: 40,
                from: "h-andrew".to_string(),
                kind: "task".to_string(),
            },
            MsgMeta {
                id: 5,
                off: 41,
                from: "s-x@n-b".to_string(),
                kind: "note".to_string(),
            },
        ];
        let mut ages = BTreeMap::new();
        ages.insert(("s-one".to_string(), 40), r.now_ms - 11 * 60 * 1000);
        ages.insert(("s-one".to_string(), 41), r.now_ms - 60 * 60 * 1000);
        let w = warnings(&r, &ages, &[]);
        assert!(w.len() == 1 && w[0].contains("h-andrew"), "{w:#?}");
        ages.insert(("s-one".to_string(), 40), r.now_ms - 9 * 60 * 1000);
        assert!(warnings(&r, &ages, &[]).is_empty(), "a note is not work");
    }

    #[cfg(unix)]
    #[test]
    fn the_status_reports_only_a_halt_a_bridge_applies() {
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-halt")
            .tempdir()
            .expect("the broker dir");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        let _server = broker.serve(&socket).expect("serve the broker");
        let (mut conn, _closer) =
            transport::connect(&socket.to_string_lossy(), Duration::from_secs(60))
                .expect("connect");
        for (seq, subject, body) in [
            (
                1,
                "/f/f1/fleet/h-andrew/halt",
                "v=1 t=1 state=on reason=main%20broken",
            ),
            (2, "/f/f1/fleet/a-svc/halt", "v=1 t=1 state=on reason=svc"),
            (3, "/f/f1/fleet/n-node/halt", "v=1 t=1 state=on reason=node"),
            (4, "/f/f1/fleet/h-bea/halt", "v=1 t=1\tstate=on reason=tab"),
            (
                5,
                "/f/f1/fleet/h-cy/halt",
                "v=1 t=1 state=off reason=lifted",
            ),
        ] {
            conn.publish(91, seq, subject, body.as_bytes())
                .expect("publish a halt row");
        }
        assert_eq!(
            fleet_halts(&mut conn, "f1"),
            vec![("h-andrew".to_string(), "main broken".to_string())]
        );
    }

    #[test]
    fn an_ask_past_its_deadline_with_no_reply_is_a_warning() {
        let now = 100 * 60 * 1000u64;
        let lane = |src: &str, kind: &str| format!("/f/lab/in/n-a/s-one/{src}/{kind}");
        let rec = |off: u64, subject: String, body: &str| (off, subject, body.as_bytes().to_vec());
        let records = vec![
            rec(
                40,
                lane("h-andrew", "ask"),
                &format!("v=1 t={} dl=600000 text=which", now - 20 * 60 * 1000),
            ),
            rec(
                41,
                lane("h-andrew", "task"),
                &format!("v=1 t={} dl=600000 text=do", now - 20 * 60 * 1000),
            ),
            rec(
                45,
                lane("n-b", "answer"),
                &format!("v=1 t={} from=s-two re=41 text=done", now - 5 * 60 * 1000),
            ),
            rec(
                42,
                lane("n-b", "ask"),
                &format!(
                    "v=1 t={} from=s-two dl=600000 text=why",
                    now - 20 * 60 * 1000
                ),
            ),
            rec(
                46,
                lane("n-a", "expired"),
                &format!("v=1 t={} re=42 dl=600000 text=expired", now - 9 * 60 * 1000),
            ),
            rec(
                43,
                lane("h-andrew", "ask"),
                &format!("v=1 t={} dl=600000 text=soon", now - 5 * 60 * 1000),
            ),
            rec(
                44,
                lane("h-andrew", "note"),
                &format!("v=1 t={} dl=1 text=fyi", now - 20 * 60 * 1000),
            ),
            rec(
                47,
                lane("h-andrew", "ask"),
                &format!("v=1 t={} text=nodl", now - 20 * 60 * 1000),
            ),
            rec(
                48,
                lane("h-andrew", "ask"),
                &format!("v=1 t={} dl=1 text=a", now - 20 * 60 * 1000),
            ),
            rec(
                49,
                lane("n-b", "ack"),
                "v=1 t=1 re=48 text=verdict%3Dhandled",
            ),
            rec(
                50,
                lane("h-andrew", "task"),
                &format!("v=1 t={} dl=1 text=b", now - 20 * 60 * 1000),
            ),
            rec(51, lane("n-b", "report"), "v=1 t=1 re=50 text=done"),
        ];
        let overdue = overdue_of("lab", &records, now);
        let offs: Vec<(u64, bool)> = overdue.iter().map(|o| (o.off, o.expired)).collect();
        assert_eq!(offs, vec![(40, false), (42, true)], "{overdue:#?}");
        assert_eq!(overdue[0].late_ms, 10 * 60 * 1000);
        assert_eq!(overdue[0].from, "h-andrew");
        assert_eq!(overdue[0].to, "s-one@n-a");
        assert_eq!(overdue[1].from, "s-two@n-b");

        let mut r = healthy();
        r.overdue = overdue;
        let w = warned(&r);
        assert_eq!(w.len(), 2, "{w:#?}");
        assert!(w[0].contains("off=40"), "{}", w[0]);
        assert!(w[1].contains("off=42"), "{}", w[1]);
        r.warnings = w;
        assert_eq!(exit_of(&r), 1);
        let json = render_json(&r);
        assert!(json.contains("\"overdue\":[{\"off\":40,"), "{json}");
        assert!(json.contains("\"expired\":true"), "{json}");

        let mut r = healthy();
        r.instances[0].supervised = Some(false);
        r.instances[0].fabric = Some("absent".to_string());
        one(&r, "fabric=absent");
        let mut r = healthy();
        r.instances[0].fabric = Some("disconnected".to_string());
        one(&r, "fabric=disconnected");

        r.instances[0].sessions[0].posts = Some(3);
        one(&r, "@s-one");

        let mut r = healthy();
        r.instances[0].command = Some(words(
            "aterm link serve --fleet lab --broker /tmp/old.sock --state /s",
        ));
        one(&r, "--broker /tmp/old.sock");
        r.broker.reachable = false;
        assert!(
            !warned(&r)
                .iter()
                .any(|l| l.contains("says fabric=connected")),
            "{:#?}",
            warned(&r)
        );
        let mut r = healthy();
        let substitute = CMD.replacen("aterm link", "/bin/sh -c /tmp/x link", 1);
        r.instances[0].command = Some(words(&substitute));
        r.instances[0].program = bridge_program(&words(&substitute));
        one(
            &r,
            &format!(
                "instance 100's bridge was armed with a different command than [fabric] command \
                 in /c/aterm.toml: it runs program {} (not {})",
                r.instances[0]
                    .program
                    .as_deref()
                    .expect("a bridge's program"),
                r.program.as_deref().expect("the report's program")
            ),
        );
        assert!(
            r.instances[0]
                .program
                .as_deref()
                .is_some_and(|p| p.ends_with(" -c /tmp/x")),
            "the words before `link serve` are the program's: {:?}",
            r.instances[0].program
        );

        let mut r = healthy();
        let mut twin = r.instances[0].clone();
        twin.pid = 200;
        twin.sessions.clear();
        twin.fabric = Some("stalled".to_string());
        twin.reason = Some("state-locked".to_string());
        r.instances.push(twin);
        one(&r, "reason=state-locked");

        let mut r = healthy();
        r.instances[0].error = Some("connect: timed out".to_string());
        one(&r, "connect: timed out");

        let mut r = healthy();
        r.source = Source::Instance(100);
        one(&r, "/c/aterm.toml");

        let mut r = healthy();
        r.node = None;
        r.caps = vec![(
            "/c/node.cap".to_string(),
            Err("the tag is not hex".to_string()),
        )];
        one(&r, "/s/node");
        one(&r, "the tag is not hex");

        let r = healthy();
        let w = warnings(
            &r,
            &BTreeMap::new(),
            &[("h-andrew".to_string(), "stop".to_string())],
        );
        assert!(w.iter().any(|l| l.contains("reason=stop")), "{w:#?}");

        let mut r = healthy();
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-kept")
            .tempdir()
            .expect("a state dir");
        r.cfg.state_dir = dir.path().to_string_lossy().into_owned();
        let st = crate::state::StateDir::open(dir.path()).expect("open");
        let halt = |reason: &str, kept| crate::state::StandingHalt {
            reason: reason.to_string(),
            kept,
        };
        st.set_standing_halts(
            "lab",
            &BTreeMap::from([
                ("h-andrew".to_string(), halt("main%20broken", true)),
                ("h-bea".to_string(), halt("stop", false)),
            ]),
        )
        .expect("write");
        let w = warned(&r);
        assert!(w.iter().any(|l| l.contains("h-andrew")), "{w:#?}");
        assert!(!w.iter().any(|l| l.contains("h-bea's")), "{w:#?}");

        let mut r = healthy();
        let fresh_gone = format!("v=1 t={} state=exited hold=0", crate::now_ms());
        let rows: [(&str, &[u8]); 5] = [
            (
                "/f/lab/pub/n-a/node/presence",
                b"v=1 t=1 state=live fabric=connected",
            ),
            (
                "/f/lab/pub/n-a/s-one/presence",
                b"v=1 t=1 state=live hold=0",
            ),
            (
                "/f/lab/pub/n-a/s-ghost/presence",
                b"v=1 t=1 state=live hold=0",
            ),
            ("/f/lab/pub/n-a/s-gone/presence", fresh_gone.as_bytes()),
            (
                "/f/lab/pub/n-far/s-far/presence",
                b"v=1 t=1 state=live hold=0",
            ),
        ];
        let glance = crate::glance::Glance {
            fleet: "lab".to_string(),
            rows: rows
                .iter()
                .enumerate()
                .filter_map(|(i, (subj, body))| {
                    crate::glance::Row::parse("lab", i as u64, subj, body)
                })
                .collect(),
            truncated: false,
        };
        join_sessions(&mut r, Some(&glance));
        let joined: Vec<(&str, Option<u32>, &str)> = r
            .sessions
            .iter()
            .map(|s| (s.sid.as_str(), s.pid, s.bus.as_str()))
            .collect();
        assert_eq!(
            joined,
            vec![
                ("s-one", Some(100), "live"),
                ("s-far", None, "live"),
                ("s-ghost", None, "live"),
                ("s-gone", None, "gone"),
            ],
            "local rows first, then the bus-only ones by sid; a recently retired \
             local row is LISTED as `gone` rather than counted"
        );
        assert_eq!(r.exited, 0, "nothing here is old enough to be a bare count");
        let w = warned(&r);
        assert!(w.iter().any(|l| l.contains("@s-ghost")), "{w:#?}");
        assert!(
            !w.iter().any(|l| l.contains("@s-gone")),
            "a retired row is not advertised live and must not be warned about: {w:#?}"
        );
        assert!(
            !w.iter().any(|l| l.contains("@s-far")),
            "a REMOTE node's session is not this machine's to host: {w:#?}"
        );
    }

    #[test]
    fn a_will_reads_away_and_so_do_the_rows_it_left_live() {
        let mut r = healthy();
        let fresh_exited = format!("v=1 t={} state=exited hold=0", crate::now_ms());
        let rows: [(&str, &[u8]); 7] = [
            (
                "/f/lab/pub/n-a/node/presence",
                b"v=1 t=1 state=live fabric=connected",
            ),
            (
                "/f/lab/pub/n-a/s-one/presence",
                b"v=1 t=1 state=live hold=0",
            ),
            (
                "/f/lab/pub/n-far/node/presence",
                b"v=1 t=1 state=away inc=4 fabric=disconnected",
            ),
            (
                "/f/lab/pub/n-old/node/presence",
                b"v=1 t=1 state=gone inc=2 fabric=disconnected",
            ),
            (
                "/f/lab/pub/n-far/s-far/presence",
                b"v=1 t=1 state=live hold=0",
            ),
            (
                "/f/lab/pub/n-old/s-old/presence",
                b"v=1 t=1 state=live hold=0",
            ),
            ("/f/lab/pub/n-a/s-ended/presence", fresh_exited.as_bytes()),
        ];
        let glance = crate::glance::Glance {
            fleet: "lab".to_string(),
            rows: rows
                .iter()
                .enumerate()
                .filter_map(|(i, (subj, body))| {
                    crate::glance::Row::parse("lab", i as u64, subj, body)
                })
                .collect(),
            truncated: false,
        };
        join_sessions(&mut r, Some(&glance));
        let joined: Vec<(&str, &str)> = r
            .sessions
            .iter()
            .map(|s| (s.sid.as_str(), s.bus.as_str()))
            .collect();
        assert_eq!(
            joined,
            vec![
                ("s-one", "live"),
                ("s-ended", "gone"),
                ("s-far", "away"),
                ("s-old", "away"),
            ]
        );
        assert_eq!(
            live_sessions_on(&r, "n-far"),
            1,
            "an away node keeps its sessions"
        );
        let text = render_text(&r);
        for node in ["n-far", "n-old"] {
            let line = text
                .lines()
                .find(|l| l.contains(node) && l.contains("host="))
                .unwrap_or_else(|| panic!("the {node} row:\n{text}"));
            assert!(line.contains("state=away"), "{line}");
        }
        assert!(
            render_json(&r).contains("\"state\":\"gone\",\"reads\":\"away\""),
            "the JSON keeps the word on the bus beside the reading"
        );
    }

    #[test]
    fn a_cut_roster_and_a_spent_subject_budget_are_said() {
        let mut quiet = healthy();
        let text = render_text(&quiet);
        assert!(text.contains(" 3 of 4096 spent by this node"), "{text}");
        assert!(!text.contains("truncated"), "{text}");
        assert_eq!(warned(&quiet), Vec::<String>::new());
        quiet.subjects = Some(Err("unauthorized".to_string()));
        assert!(render_text(&quiet).contains("could not be counted: unauthorized"));

        let mut r = healthy();
        r.roster_rows = Some(crate::glance::MAX_ROWS);
        r.roster_truncated = true;
        r.subjects = Some(Ok(SUBJECT_BUDGET_WARN));
        assert!(
            render_text(&r).contains("rows=4096+ truncated"),
            "the SESSIONS section says the read was cut"
        );
        one(&r, "rows=4096+ truncated");
        one(
            &r,
            &format!("holds {SUBJECT_BUDGET_WARN} of the 4096 distinct subjects"),
        );
        let json = render_json(&r);
        assert!(
            json.contains("\"roster\":{\"rows\":4096,\"truncated\":true}"),
            "{json}"
        );
        assert!(
            json.contains(&format!(
                "\"subjects\":{{\"used\":{SUBJECT_BUDGET_WARN},\"budget\":4096"
            )),
            "{json}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_subject_count_is_read_off_a_guarded_broker_through_the_nodes_ring() {
        let secret = [5u8; 32];
        let dir = aterm_tempfile::Builder::new()
            .prefix("atfab-subj")
            .tempdir()
            .expect("scratch");
        let socket = dir.path().join("b.sock");
        let broker =
            astream_broker::Broker::open_guarded(dir.path().join("b.log"), secret.to_vec())
                .expect("a guarded broker");
        let _server = broker.serve(&socket).expect("serve");
        let ring = |node: &str| -> Conn {
            let (mut conn, _) =
                transport::connect(&socket.to_string_lossy(), Duration::from_secs(60))
                    .expect("connect");
            for grant in crate::enable::node_grants("lab", node) {
                let cap = astream_cap::mint(&secret, &grant).expect("mint");
                conn.attach(&cap.filter, &cap.tag).expect("attach");
            }
            conn
        };
        let publish = |conn: &mut Conn, node: &str, subjects: &[&str]| {
            let producer = astream_cap::producer_id_of(node);
            for (seq, subject) in subjects.iter().enumerate() {
                conn.publish(producer, seq as u64 + 1, subject, b"v=1 t=1")
                    .unwrap_or_else(|e| panic!("{node} publishes {subject}: {e}"));
            }
        };
        let mut a = ring("n-a");
        publish(
            &mut a,
            "n-a",
            &[
                "/f/lab/pub/n-a/node/presence",
                "/f/lab/pub/n-a/node/ev",
                "/f/lab/pub/n-a/s-1/presence",
                "/f/lab/pub/n-a/s-1/ev",
                "/f/lab/pub/n-a/s-2/presence",
                "/f/lab/pub/n-a/s-1/say/lab",
                "/f/lab/in/n-b/s-9/n-a/ask",
                "/f/lab/in/n-b/s-9/n-a/ask",
                "/f/lab/in/n-b/s-8/n-a/note",
                "/f/lab/pub/n-a/node/presence",
            ],
        );
        let mut b = ring("n-b");
        publish(
            &mut b,
            "n-b",
            &[
                "/f/lab/pub/n-b/node/presence",
                "/f/lab/pub/n-b/s-9/presence",
                "/f/lab/in/n-a/s-1/n-b/ask",
            ],
        );
        assert_eq!(subjects_of(&mut a, "lab", "n-a").expect("counted"), 8);
        assert_eq!(subjects_of(&mut b, "lab", "n-b").expect("counted"), 3);
    }

    #[test]
    fn the_report_prints_no_body() {
        let mut r = healthy();
        r.traffic = vec![traffic_of(
            "lab",
            7,
            "/f/lab/in/n-a/s-one/h-andrew/task",
            b"v=1 t=6000000 text=the%20secret%20plan",
        )];
        r.warnings = warned(&r);
        let text = render_text(&r);
        assert!(
            !text.contains("secret"),
            "a body reached the report: {text}"
        );
        let json = render_json(&r);
        assert!(!json.contains("secret"), "{json}");
        assert!(json.contains("\"exit\":0"), "{json}");
    }

    #[test]
    fn a_retire_refusal_event_names_its_sid() {
        for (ev, sid) in [
            (
                "presence-retire-refused sid=s-x reason=hosted pid=42",
                Some("s-x"),
            ),
            (
                "presence-retire-refused sid=s-y reason=unaskable",
                Some("s-y"),
            ),
            ("presence-retire-refused sid=s-z reason=new", Some("s-z")),
            ("presence-retired sid=s-x reason=operator", None),
            ("presence-retire-refused reason=hosted", None),
        ] {
            assert_eq!(
                refused_retire_event(ev).map(|(s, _)| s).as_deref(),
                sid,
                "{ev}"
            );
        }
    }

    #[test]
    fn a_json_value_cannot_break_out_of_its_string() {
        let mut out = String::new();
        J::Obj(vec![
            ("a", J::s("q\"uo\\te\n\u{1b}")),
            ("b", J::Arr(vec![J::Num(1), J::Null, J::Bool(true)])),
        ])
        .render(&mut out);
        assert_eq!(out, r#"{"a":"q\"uo\\te\u000a\u001b","b":[1,null,true]}"#);
    }
}

#[cfg(test)]
mod running_argv_tests {
    use super::*;
    use crate::durability::Durability;
    use std::ffi::OsString;

    #[test]
    fn a_broker_report_uses_the_exact_argv_or_reports_an_unknown_tier() {
        let command =
            "/bin/aterm link broker /r/bus.sock --secret-file /r/name --durability relaxed";
        let procs = vec![Proc {
            pid: 71,
            ppid: 1,
            cmd: command.into(),
        }];
        let words = |args: &[&str]| {
            args.iter()
                .map(|word| OsString::from(*word))
                .collect::<Vec<_>>()
        };
        let exact = words(&[
            "/bin/aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "--secret-file",
            "/r/name --durability relaxed",
        ]);
        let relaxed = words(&[
            "/bin/aterm",
            "link",
            "broker",
            "/r/bus.sock",
            "--durability",
            "relaxed",
        ]);
        let other = words(&[
            "/bin/aterm",
            "link",
            "broker",
            "/other/socket",
            "--secret-file",
            "--unix /r/bus.sock",
        ]);
        let cases = [
            (Some(relaxed), Some(Durability::Relaxed)),
            (Some(exact), Some(crate::durability::default_for(true))),
            (None, None),
            (Some(Vec::new()), None),
            (Some(other), None),
        ];
        let mut view = BrokerView {
            endpoint: "/r/bus.sock".into(),
            ..BrokerView::default()
        };
        for (argv, expected) in cases {
            let mut read = Vec::new();
            view.locate_with(
                &procs,
                |pid| {
                    assert_eq!(pid, 71);
                    None
                },
                |pid, exe| {
                    assert_eq!(pid, 71);
                    assert_eq!(exe, "/bin/aterm", "argv[0], else the line's first word");
                    None
                },
                |pid| {
                    read.push(pid);
                    argv.clone()
                },
            );
            assert_eq!(read, vec![71]);
            assert_eq!(view.pid, Some(71));
            assert_eq!(view.durability, expected);
        }
    }

    #[test]
    fn the_replaced_build_check_reads_the_whole_argv0() {
        let spaced = "/a b/aterm";
        let procs = vec![Proc {
            pid: 72,
            ppid: 1,
            cmd: format!("{spaced} link broker /r/bus.sock /r/bus.log --secret-file /r/s"),
        }];
        let argv: Vec<OsString> = [spaced, "link", "broker", "/r/bus.sock", "/r/bus.log"]
            .iter()
            .map(OsString::from)
            .collect();
        for (words, want) in [(Some(argv), spaced), (None, "/a"), (Some(Vec::new()), "/a")] {
            let mut view = BrokerView {
                endpoint: "/r/bus.sock".into(),
                ..BrokerView::default()
            };
            let mut asked = Vec::new();
            view.locate_with(
                &procs,
                |_| None,
                |pid, exe| {
                    assert_eq!(pid, 72);
                    asked.push(exe.to_string());
                    Some(exe.to_string())
                },
                |_| words.clone(),
            );
            assert_eq!(asked, vec![want.to_string()]);
            assert_eq!(view.replaced.as_deref(), Some(want));
        }
    }
}
