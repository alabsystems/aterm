// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric`: fabric status, live traffic (`tail`), session-log brief (`brief`), lifecycle
//! (`on`, `off`, `doctor` in [`crate::enable`]), and person commands (`mint-for`, `halt`,
//! `inbox`, `post` in [`crate::person`]).

#[cfg(test)]
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
use aterm_types::fabric_grammar::ANSWER_KINDS;

/// Usage, printed for `help` (exit 0) and after a usage error (exit 2).
const USAGE: &str = "\
usage: aterm fabric [status] [--json]
       aterm fabric tail [--bodies] [--json] [--from <offset>] [--filter <subject-filter>]
       aterm fabric brief [@<sid>[@<node>]] [--window <records>] [--budget <tokens>]
                          [--cap-file <path>] [--json]
       aterm fabric on  [--dry-run] [--fleet <F>] [--service launchd|systemd|none|node]
                        [--durability strict|ordered|relaxed]
       aterm fabric off [--dry-run] [--service launchd|systemd|none|node]
       aterm fabric doctor [--retire-ghosts [--yes]]
       aterm fabric mint-for h-<name>|a-<name> [--out <cap>] [--fleet <F>]
                             [--expires <N>d|<N>h]
       aterm fabric halt  --cap-file <person-cap> <reason>|off
       aterm fabric inbox --cap-file <person-cap> [--peek]
       aterm fabric post  --cap-file <person-cap> to=@<sid>@<node> [kind=<k>] [re=<off>]
                          [--] <text>
       aterm fabric help

Inspect and manage this machine's fabric. Every verb that reads the bus finds the fleet,
broker, cap file and state dir in `[fabric] command` (aterm.toml), else in the rendezvous
file `fabric.toml` that `on` writes, else in a running instance's bridge command.

  status           the default; seven sections:
                   - CONFIG: the command's source and program, fleet, node id, cap
                     files and their grants, state dir, presence and receipts
                   - BROKER: whether it answers, its pid (and launchd job), bus head,
                     `subjects` this node spent of 4096, `acks` durability, `health`
                     (poisoned or not, flush counts), `holds` (from `<log>.health`)
                     and `guard` (whether it checks capabilities)
                   - NODES: every node on the presence roster, marked `this`, `local`
                     or `remote` (no instance here answers as that node)
                   - BRIDGES: each aterm instance on this machine and its bridge
                   - SESSIONS: each session's bus state (`live`, `away`, `gone`), hold,
                     mail counts, topics, role, detail, phase and title (CTX stays `-`:
                     no bridge of this build writes it; `rows=4096+ truncated` past 4096
                     rows)
                   - TRAFFIC: the last 10 bus records, metadata only
                   - WARNINGS: each problem found (`doctor` prints its fix)
  --json           status: one JSON object of the same data; brief: one object whose
                   `sessions` each carry these keys:
@SESSION_KEYS@
                   tail: one object per record, one per line, offsets strictly
                   increasing (so `--from` the last `off` plus one misses none), with
                   these keys (`node` and `sid` null where the subject names none;
                   `body` only with --bodies):
@TAIL_KEYS@
                   and, where retention gave records up before it read them, one
                   {\"gap\":{...}} line
  tail             stream bus records as they land, until Ctrl-C
  --from <offset>  tail: replay from bus offset <offset> first
  --bodies         tail: print each record's body after its trust label (--json:
                   `body`): its `text=`, or, for a record with no `text=`, its whole
                   first line and never a byte past it (a presence row's fields; an `ev`
                   record's session log bus line, whose grammar `aterm help fabric` gives
                   under THE SESSION LOG ON THE BUS); `len` counts the body's bytes
  --filter <f>     tail: one full subject pattern, `/f/<F>/…` (`*` one segment, `>`
                   the rest), instead of the whole fleet
  brief            fold each session's mirrored log into a few lines (need, open work,
                   mail), newest session first, within --budget tokens (default 2000)
                   over the last --window records (default 65536); `@<sid>[@<node>]`
                   picks one session
  on               provision (idempotently) the fabric root `~/.local/share/aterm-fabric`
                   (0700), node id, mint secret and the node's 8 grants; install and start
                   the broker under launchd (macOS) or systemd --user (Linux); write
                   `[fabric] command` to aterm.toml (keeping `.bak`) and `fabric.toml`
                   (0600); attach running instances; prove delivery with a note a session
                   posts to itself; print `status`. An aterm window's converge pass runs
                   these steps (the proof aside) wherever the fabric was never turned off
  --dry-run        on/off: print every step, change nothing
  --fleet <F>      on, mint-for: the fleet (`/f/<F>/`; default the configured one, else
                   `local`)
  --service <s>    on/off: `launchd`, `systemd`, `none` (a broker run some other way), or
                   `node` (the node service hosts bus.log: no broker job; the default with
                   the mesh on)
  --durability strict|ordered|relaxed
                   on: what the broker's ack promises (default the installed job's, else
                   `ordered`):
                   - `ordered`: acks after a disk ordering barrier and flushes in full
                     within 100 ms; served `strict` off macOS, for a log off the boot
                     volume, or where the device refuses the barrier
                   - `strict`: acks after a full drive-cache flush; a power loss loses
                     nothing acked
                   - `relaxed`: acks after the page-cache write; survives a broker
                     restart, not a power loss or kernel panic
                   Halts, a human's words, mail to a person and a person's read marks
                   always flush in full before their ack. A clean stop flushes and closes
                   the log; after an unclean reboot the log's generation advances, and
                   bridges and notifiers resume without skipping mail
  off              record the off (`fabric-off` in the root: the converge pass, which
                   turns the fabric on by default, leaves it off until `on`; an off waits
                   for a turn-on the pass is making), stop and remove the broker job,
                   `[fabric]` in aterm.toml (keeping `.bak`) and `fabric.toml`; the node
                   id, secret, cap and bus log stay
  doctor           print the broker's durability, health and holds, then each warning
                   with its fix; `--retire-ghosts` lists the live presence rows of this
                   machine's nodes that no instance hosts and, after y/N or `--yes`,
                   publishes `exited` for each
  mint-for <id>    mint a cap for human `h-<name>` or agent `a-<name>` under this
                   machine's mint secret (a human's includes `halt`)
  --out <cap>      mint-for: write the cap to <cap> (0600; refuses a symlink or a
                   different cap there); without it the cap goes to stdout and the
                   guidance to stderr, and a file you redirect it into must be 0600
  --expires <N>d|<N>h
                   mint-for: stamp each grant `exp=` so the broker refuses it after that
  halt <reason>    as a human's cap: hold every session on the fleet (`hold=1
                   origin=fleet`); `halt off` withdraws this human's halt. With the
                   mesh on a halt reaches every Mac under an id; `halt off <id>` lifts it
                   on every Mac, from any member's person (`--here`: on this Mac alone),
                   and `halt off` lifts every halt that stands here
  inbox            as a person's cap: print its unread mail and mark it read (`--peek`
                   leaves it unread)
  post             as a person's cap: send to `@<sid>@<node>` with `kind=ask|answer|task|
                   report|note` (default `note`) and optional `re=<off>`
  --cap-file <c>   halt/inbox/post: the person's cap (`mint-for h-<name> --out <c>`);
                   brief: read under it instead of the node's cap. A regular file of
                   yours at 0600 or tighter, in any folder: a wider one, a symlink or a
                   FIFO is refused at once

`status` and `tail` print no message text (`tail --bodies` does); `status`, `tail`,
`brief` and a plain `doctor` write nothing. Exit status: 0 healthy (`on`: done, even
when its status warns), 1 a warning, a failed step or proof, or a request the broker
refused or failed, 2 fabric off, unreadable config, a refused precondition, or a usage
error.
";

/// The keys of each record `aterm fabric tail --json` prints, in order ([`tail_json`]).
pub const TAIL_KEYS: [&str; 12] = [
    "off", "subject", "node", "sid", "t_ms", "time", "kind", "len", "trust", "from", "to", "body",
];

/// `aterm fabric --help`, its key lists filled from the declarations the JSON writers use.
#[must_use]
pub fn usage() -> String {
    USAGE
        .replace(
            "@SESSION_KEYS@\n",
            &key_list(&crate::fabric_brief::SESSION_KEYS),
        )
        .replace("@TAIL_KEYS@\n", &key_list(&TAIL_KEYS))
}

/// One key list as [`usage`] prints it: each key backticked, in its order, filled to the
/// help's width, one newline after.
fn key_list(keys: &[&str]) -> String {
    let words: Vec<String> = keys.iter().map(|k| format!("`{k}`")).collect();
    format!("{}\n", crate::render::fill(&words, 19, 90))
}

const FABRIC_COMMAND_ENV: &str = "ATERM_FABRIC_COMMAND";

/// The [`FABRIC_COMMAND_ENV`] seam's value: `None` in a shipped build, which compiles the read out.
pub(crate) fn command_seam() -> Option<String> {
    aterm_types::dev_seam!(FABRIC_COMMAND_ENV).map(|v| v.to_string_lossy().into_owned())
}

/// How many bus records TRAFFIC shows.
pub const TRAFFIC_ROWS: usize = 10;

const TRAFFIC_SCAN_MAX: u64 = 1 << 20;

/// An unhandled `task` or `ask` older than this is a warning.
pub const STALE_WORK_MS: u64 = 10 * 60 * 1000;

/// The bound on every socket this report and the operator's one-shot broker commands open: the
/// dial, and each read and write ([`transport::connect`]). This module, the roster (`cli`), the
/// brief and a person's commands (`person`) read it through [`io_timeout`].
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(60);

/// The minute hang detector for a required local broker or instance reply.
pub(crate) fn io_timeout() -> Duration {
    io_timeout_at(Instant::now())
}

pub(crate) fn io_timeout_at(started: Instant) -> Duration {
    started
        .checked_add(IO_TIMEOUT)
        .map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        })
}

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
    /// `aterm fabric tail [--bodies] [--json] [--from <offset>] [--filter <subject>]`.
    Tail {
        /// `--bodies`.
        bodies: bool,
        /// `--json`: one object per record ([`tail_json`]).
        json: bool,
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
        /// `--retire-ghosts`: publish `exited` for every presence row this machine's nodes
        /// advertise (`live` or `away`) that no local instance hosts.
        retire_ghosts: bool,
        /// `--yes`: skip the y/N confirmation (requires `--retire-ghosts`).
        yes: bool,
    },
    /// `aterm fabric mint-for h-<name>|a-<name> …` ([`crate::person::mint_for`]).
    MintFor(crate::person::MintForOpts),
    /// `aterm fabric halt|inbox|post --cap-file <cap> …` ([`crate::person`]): the verb, and
    /// what it was told.
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
        Some("brief") => {
            let rest = &args[1..];
            if rest.iter().any(|a| matches!(a.as_str(), "-h" | "--help")) {
                return Ok(Cmd::Help);
            }
            return crate::fabric_brief::BriefOpts::parse(rest).map(Cmd::Brief);
        }
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
            ("status" | "tail", "--json") => json = true,
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
                from = Some(
                    aterm_types::fabric_grammar::parse_bus_offset(&v)
                        .ok_or_else(|| format!("--from {} is not a bus offset", safe(&v, 64)))?,
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
            (_, "--json") => {
                return Err(
                    "--json is a `status`, `tail` or `brief` flag (`aterm fabric --json`, \
                     `aterm fabric tail --json`, `aterm fabric brief --json`)"
                        .to_string(),
                );
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
            json,
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
#[must_use = "handle or propagate the command's exit status"]
pub fn main(args: &[String]) -> ExitCode {
    match parse_args(args) {
        Ok(Cmd::Help) => aterm_uds::stdout::answer_exit("aterm fabric", &usage(), 0),
        Ok(Cmd::Status { json }) => status_main(json),
        Ok(Cmd::Tail {
            bodies,
            json,
            from,
            filter,
        }) => tail_main(TailFmt::new(bodies, json), from, filter.as_deref()),
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
            eprint!("{}", usage());
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
    /// `command` in the rendezvous file at this path (`aterm fabric on` wrote it; aterm.toml
    /// has no command).
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

    const fn token(&self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::File(_) => "config",
            Source::Rendezvous(_) => "rendezvous",
            Source::Instance(_) => "instance",
        }
    }
}

/// The aterm config file: `$XDG_CONFIG_HOME/aterm/aterm.toml`, else
/// `$HOME/.config/aterm/aterm.toml`, the path `aterm-gui`'s `app_config::config_path` resolves
/// on every Unix.
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

/// `[fabric] command` out of an aterm.toml's text, through the parser the app loads the file
/// with, as the argv it names. Either spelling ([`aterm_types::fabric_command`]): a string
/// split on whitespace, or an array of strings taken word for word.
pub fn command_in_toml(text: &str) -> Result<Option<Vec<String>>, String> {
    let table: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    if !swarm_enabled_in_table(&table)? {
        return Ok(None);
    }
    let Some(fabric) = table.get("fabric") else {
        return Ok(None);
    };
    let Some(fabric) = fabric.as_table() else {
        return Err("`fabric` is not a table".to_string());
    };
    fabric.get("command").map_or(Ok(None), command_of)
}

fn swarm_enabled_in_table(table: &aterm_toml::Table) -> Result<bool, String> {
    let Some(swarm) = table.get("swarm") else {
        return Ok(true);
    };
    let Some(swarm) = swarm.as_table() else {
        return Err("`swarm` is not a table".to_string());
    };
    if swarm.keys().any(|key| key != "enabled") {
        return Err("unknown `[swarm]` setting".to_string());
    }
    match swarm.get("enabled") {
        None => Ok(true),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| "`[swarm] enabled` is not a boolean".to_string()),
    }
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

/// How a bridge argv is shown to a person: its words joined by spaces, each one a shell would
/// not read as one word single-quoted ([`shell_word`]), so a path with a space in it is
/// visibly one word.
#[must_use]
pub fn shown_argv<S: AsRef<str>>(argv: &[S]) -> String {
    argv.iter()
        .map(|w| shell_word(w.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One word of [`shown_argv`]: as it is when every character is a letter, a digit or one of
/// `/-_.,:=+@%`, else single-quoted, with a quote in it spelled `'\''`.
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

/// `[fabric] presence` out of an aterm.toml's text: `Ok(None)` when the table or the key is
/// absent (the bridge's default, `meta`, applies).
pub fn presence_in_toml(text: &str) -> Result<Option<crate::presence::Mode>, String> {
    fabric_key(text, "presence", |v| match v.as_str() {
        Some(s) => crate::presence::Mode::parse(s).map(Some).ok_or_else(|| {
            format!("`[fabric] presence = {s:?}` is neither \"meta\" nor \"minimal\"")
        }),
        None => Err("`[fabric] presence` is not a string".to_string()),
    })
}

/// `[fabric] presence` from the aterm.toml [`config_path`] resolves, for a `serve` whose
/// command line did not say.
#[must_use]
pub fn presence_from_config() -> crate::presence::Mode {
    presence_from_config_at(config_path().as_deref())
}

fn presence_from_config_at(path: Option<&Path>) -> crate::presence::Mode {
    key_from_config_at(
        path,
        presence_in_toml,
        crate::presence::Mode::Meta,
        crate::presence::Mode::Minimal,
        "presence is minimal",
    )
}

fn key_from_config_or<T>(
    in_toml: fn(&str) -> Result<Option<T>, String>,
    absent: T,
    broken: T,
    fallback: &str,
) -> T {
    key_from_config_at(config_path().as_deref(), in_toml, absent, broken, fallback)
}

fn key_from_config_at<T>(
    path: Option<&Path>,
    in_toml: fn(&str) -> Result<Option<T>, String>,
    absent: T,
    broken: T,
    fallback: &str,
) -> T {
    let Some(path) = path else {
        return absent;
    };
    let text = match aterm_types::dirs::read_aterm_config_text(path) {
        Ok(Some(t)) => t,
        Ok(None) => return absent,
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

/// `[fabric] receipts` from the aterm.toml [`config_path`] resolves, for a `serve` whose
/// command line said neither `--receipts` nor `--no-receipts`.
#[must_use]
pub fn receipts_from_config() -> bool {
    receipts_from_config_at(config_path().as_deref())
}

fn receipts_from_config_at(path: Option<&Path>) -> bool {
    key_from_config_at(path, receipts_in_toml, true, false, "receipts are off")
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

/// `[fabric] log` from `aterm.toml`: `auto` when absent, `off` with a stderr warning when
/// unreadable or invalid.
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
    let text = match aterm_types::dirs::read_aterm_config_text(path) {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(None),
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

/// The flags of an `aterm link serve …` / `aterm-link serve …` argv, or `None` when the argv
/// is not a bridge.
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

/// The program prefix of a bridge command (`argv` before `link serve` / `serve`), with
/// `argv[0]` resolved on `$PATH` and canonicalized, formatted with [`shown_argv`].
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

/// The bridges among an instance's aterm children ([`aterm_ctl::census::aterm_children`]):
/// the ones whose words are `aterm link serve …` or `aterm-link serve …` ([`serve_flags`]). A
/// child whose words were not read (its thread had not run, or they are unread) is no bridge.
fn bridges_among(children: &[aterm_ctl::census::Child]) -> Vec<u32> {
    children
        .iter()
        .filter(|c| {
            c.words
                .as_deref()
                .is_some_and(|words| serve_flags(words).is_some())
        })
        .map(|c| c.pid)
        .collect()
}

/// The process serving the broker at `sock`: the one process of this user holding the listening
/// socket bound there, by the socket's own record ([`aterm_uds::holders::scan_listener_holders`]),
/// never by a command line that names the socket. `Ok(None)` when no process holds it, or when
/// which one does is unknown with nothing bound to the socket's file ([`unknown_unless_unbound`]).
///
/// # Errors
/// Which process serves it is unknown, and why, while a socket may be bound to its file: the
/// scan cannot run here, does not answer in time or cannot tell (a connection refused by a
/// listener whose accept queue is full, say), a table that may hold the listener did not read,
/// or two processes hold the listener ([`the_one_holder`]).
pub(crate) fn listener_pid(sock: &str) -> Result<Option<u32>, String> {
    let path = Path::new(sock);
    let held = scan_listener(path)
        .map_err(|e| format!("the listener scan cannot tell ({e})"))
        .and_then(|scan| the_one_holder(&scan));
    unknown_unless_unbound(held, || aterm_uds::holders::bound_at(path))
}

/// The native listener scan, using its own hang detector.
fn scan_listener(path: &Path) -> io::Result<aterm_uds::holders::HolderScan> {
    aterm_uds::holders::scan_listener_holders(path, &[])
}

/// `held`, the scan's answer ([`listener_pid`]), but where it is unknown and a probe of the
/// socket's file (`bound`, [`aterm_uds::holders::bound_at`]) finds nothing bound to it (no file,
/// or a socket file whose listener closed, as a killed broker leaves one), no process serves the
/// socket: none. A cause the scan cannot outlast (a table this build misreads, a kernel with no
/// socket dump) then leaves no broker unrenewed where none can be. The probe is made only for an
/// unknown.
///
/// # Errors
/// `held`'s unknown, with what the probe found, when it found a socket bound there or failed.
fn unknown_unless_unbound(
    held: Result<Option<u32>, String>,
    bound: impl FnOnce() -> io::Result<aterm_uds::holders::Bound>,
) -> Result<Option<u32>, String> {
    use aterm_uds::holders::Bound;
    let Err(unknown) = held else {
        return held;
    };
    match bound() {
        Ok(Bound::Nothing) => Ok(None),
        Ok(Bound::Stream) => Err(format!("{unknown}; a stream socket is bound there")),
        Ok(Bound::Datagram) => Err(format!("{unknown}; a datagram socket is bound there")),
        Err(e) => Err(format!("{unknown}; what is bound there is unknown ({e})")),
    }
}

/// The holder a scan found: `Ok(None)` when it found none and read every table it had to, the
/// one it found, or an error naming each when it found more than one, as which one serves is
/// then unknown. A table the scan could not read does not unseat the one it found: Linux seals
/// every non-dumpable process's table (an `ssh-agent`), so a scan that had to read every table
/// would name no broker on most hosts, and a broker never seals its own. Where it found none,
/// such a table may hold the listener, so which process does is unknown: Linux's scan reads the
/// tables only once the kernel's socket dump names a listener holding the path (none is read
/// for a path no listener holds), and macOS's leaves both lists empty.
///
/// # Errors
/// The scan found two or more holders, or none with a table it could not read.
fn the_one_holder(scan: &aterm_uds::holders::HolderScan) -> Result<Option<u32>, String> {
    match scan.holders.as_slice() {
        [] if !(scan.sealed.is_empty() && scan.unreadable.is_empty()) => Err(format!(
            "no table the scan read holds its listener, and the tables of {} did not read",
            scan.sealed
                .iter()
                .chain(&scan.unreadable)
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        many => Err(format!(
            "{} processes hold its listener ({})",
            many.len(),
            many.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The launchd job label whose running pid is `pid`, from `launchctl list`'s
/// `PID  Status  Label` rows.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn parse_launchctl(text: &str, pid: u32) -> Option<String> {
    text.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let p: u32 = cols
            .next()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))?
            .parse()
            .ok()?;
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

/// Who serves a fabric's socket, read off the argv of the one process holding its listener
/// ([`BrokerView::locate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// `aterm link broker <socket> …` (or `aterm-link broker …`) serving this socket: a broker
    /// process, kept by the job `aterm fabric on` installed or run by hand.
    LinkBroker,
    /// `<aterm> node serve` ([`crate::enable::NODE_SERVE`]) holding `<root>/bus.sock`: the node
    /// service ([`crate::enable::NODE_JOB`]) hosts `<root>/bus.log` in its own process. Read only
    /// where the mesh is on ([`BrokerView::mesh`]).
    NodeService,
}

impl Host {
    /// Its word in `aterm fabric --json`'s `broker.host`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Host::LinkBroker => "link-broker",
            Host::NodeService => "node-service",
        }
    }
}

/// What the broker probe found.
#[derive(Debug, Clone, Default)]
pub struct BrokerView {
    /// The broker socket the command names.
    pub endpoint: String,
    /// This build's mesh switch (`aterm_types::mesh_switch::MESH`), read once by the probe that
    /// made this view (a test sets it either way): where it is on, a process serving the socket
    /// as `aterm node serve` is the node service ([`Host::NodeService`]), and with none named the
    /// node service is the bus's keeper ([`Self::keeper`]).
    pub mesh: bool,
    /// Who serves the socket, as its holder's argv says ([`Self::locate`]); `None` where no
    /// holder is named or its argv is neither a broker of this socket nor the node service.
    pub host: Option<Host>,
    /// The holder's argv0, as its argv was read: the program it runs.
    pub program: Option<String>,
    /// Whether the node service serving the socket runs this process's own build: its argv0 names
    /// this process's executable (canonically), its image is not [`Self::replaced`], and it maps
    /// the same image this process does ([`runs_this_build`]). Read only for
    /// [`Host::NodeService`].
    pub own_build: bool,
    /// Whether a broker answered: a connect and a `Hello` round trip in this build's astream
    /// protocol.
    pub reachable: bool,
    /// Why it is not reachable, or why the cap file was refused.
    pub error: Option<String>,
    /// A broker that answered the `Hello` in another astream protocol: neither side can read
    /// what the other sends, so it is not `reachable`, and this names why.
    pub protocol: Option<astream_broker::ProtocolMismatch>,
    /// The last line the broker's supervisor captured from its stderr (`<root>/broker.err`) and
    /// its age, read only when the broker does not answer and a broker job keeps the bus
    /// ([`Self::keeper`]): the broker's own reason it is not running, such as a log it refuses to
    /// open or an address in use ([`crate::enable::supervisor_last_word`]). Under the node
    /// service it is never read: the node service writes no `broker.err`, and one in the root is
    /// a retired job's.
    pub last_word: Option<String>,
    /// Whether every cap was attached, so the bus can be read.
    pub attached: bool,
    /// Whether the broker refused the ring as lapsed
    /// ([`astream_broker::BrokerRefusal::is_expired`]): genuine grants whose `exp=` has passed,
    /// at the attach or on the head query.
    pub cap_expired: bool,
    /// Whether [`Self::error`] is a cap file this report could not read, so it never asked the bus
    /// anything: the report's own cap warning says why, whole.
    pub cap_unread: bool,
    /// The pid serving it: the one process of this user holding its listener ([`listener_pid`]).
    pub pid: Option<u32>,
    /// Why the pid serving it is unknown, where [`listener_pid`] could not tell; `None` where it
    /// could, so [`Self::pid`] names the holder or that no process holds the listener.
    pub pid_unknown: Option<String>,
    /// The launchd job running that pid (macOS).
    pub launchd: Option<String>,
    /// The bus head offset — the next offset the log will assign.
    pub head: Option<u64>,
    /// How long connect + hello + attach + head took.
    pub rtt_ms: Option<u64>,
    /// `None` when the report reads the whole fleet (`/f/<F>/>`); `Some(faces)` when the broker
    /// is guarded (every broker `on` runs is) and the cap files do not grant that, so every read
    /// is made over the faces they do grant, and TRAFFIC says so.
    pub faces: Option<Vec<String>>,
    /// Whether the broker checks capabilities, measured by attaching one grant minted under
    /// [`GUARD_PROBE_SECRET`]: `Some(true)` when it is refused; `Some(false)` when acknowledged,
    /// so the cap-forced `<src>`, `trust=human` and per-human halts rest on nothing.
    pub guarded: Option<bool>,
    /// The broker's argv0 when that path now names a different file than the image it runs: an
    /// update replaced the build after this broker started.
    pub replaced: Option<String>,
    /// What the broker's ack promises, read off the serving process's argv
    /// ([`crate::durability`]): no wire request names the tier, and the argv is what the process
    /// was started with, not what a job file says it would be.
    pub durability: Option<crate::durability::Durability>,
    /// What the broker says it serves, its `Hello` answer's tag 0x01
    /// ([`astream_broker::Client::served_durability`]): the tier it was opened at, the one in
    /// force, and why they differ.
    pub served: Option<astream_broker::ServedInfo>,
    /// The log's health as the broker says it, its `Hello` answer's entry 0x04
    /// ([`astream_broker::Client::broker_health`]): whether the log is poisoned (it refuses every
    /// write until reopened) and why, and the flushes issued since it opened.
    pub health: Option<astream_broker::HelloHealth>,
    /// The log the serving process's argv names ([`crate::broker_args::BrokerArgs::log_path`]),
    /// or, for the node service, `<root>/bus.log` beside the `<root>/bus.sock` it serves (it
    /// always hosts the two together): beside it astream keeps its health snapshot.
    pub log: Option<PathBuf>,
    /// The log a fabric root keeps beside the configured socket, when one is there: what
    /// [`Self::at_rest`] reads while no serving process names a log.
    pub root_log: Option<PathBuf>,
    /// astream's health snapshot beside that log (`<log>.health`: what the log holds, who is
    /// attached, its flushes and its poison), as [`astream_broker::read_health`] found it, with
    /// whether the process that wrote it still serves the log.
    pub snapshot: Option<Result<astream_broker::HealthRead, String>>,
}

const GUARD_PROBE_SECRET: &[u8] = b"aterm fabric guard probe: no broker's secret";

impl BrokerView {
    /// `sealed` or `plaintext`, of the log the serving process names, or with none serving of the
    /// fabric root's own: a refused broker is when a person asks about the seal. `None` when there
    /// is neither.
    #[must_use]
    pub fn at_rest(&self) -> Option<&'static str> {
        self.log
            .as_deref()
            .or(self.root_log.as_deref())
            .map(crate::at_rest::word)
    }

    /// Whether the bus was read: connect, `Hello`, every attach and the head query all answered.
    #[must_use]
    pub const fn read_ok(&self) -> bool {
        self.reachable && self.attached && self.head.is_some()
    }

    /// How much of [`Self::error`] a report prints: a cap's refusal whole.
    const fn error_words(&self) -> usize {
        if self.cap_unread {
            CAP_REFUSAL_WORDS
        } else {
            256
        }
    }

    /// Whether a process may hold its listener: one is named ([`Self::pid`]), or which one is
    /// unknown ([`Self::pid_unknown`]).
    #[must_use]
    pub const fn held(&self) -> bool {
        self.pid.is_some() || self.pid_unknown.is_some()
    }

    /// The tier the report names: the one the serving process's argv names
    /// ([`Self::durability`]); else, for the node service (whose argv names none), the one its
    /// current health snapshot says the log was opened at ([`Self::snapshot_now`]); else, for a
    /// process whose argv this host cannot read, the one its `Hello` says ([`Self::served`]). A
    /// snapshot is read only beside a log the serving process names ([`Self::log`]: a broker's
    /// argv, which then names its tier too, or the node service's root), so a broker's tier is
    /// its argv's whenever there is one.
    #[must_use]
    pub fn tier(&self) -> Option<crate::durability::Durability> {
        use crate::durability::Durability;
        self.durability
            .or_else(|| {
                self.snapshot_now()
                    .and_then(|h| Durability::of_tier(h.served.requested))
            })
            .or_else(|| self.served.and_then(|s| Durability::of_tier(s.requested)))
    }

    /// Who keeps the bus this view read, for the remedies `aterm fabric doctor` gives: the
    /// node service where it serves the socket, a broker job (or a broker run by hand) where a
    /// broker does, and with neither named this build's default keeper
    /// ([`crate::enable::Service::default_for`] of [`Self::mesh`]).
    #[must_use]
    pub fn keeper(&self) -> crate::enable::Keeper {
        use crate::enable::{Keeper, Service};
        match self.host {
            Some(Host::NodeService) => Keeper::Node,
            Some(Host::LinkBroker) => Keeper::Job,
            None if Service::default_for(self.mesh) == Service::Node => Keeper::Node,
            None => Keeper::Job,
        }
    }

    /// The build the node service serving the socket runs, where it is this process's own
    /// ([`Self::own_build`]): `<version>+g<commit>` ([`aterm_stamp::build_id`]).
    #[must_use]
    pub fn build(&self) -> Option<String> {
        self.own_build.then(aterm_stamp::build_id)
    }

    /// astream's health snapshot ([`Self::snapshot`]) only when it is the serving broker's own
    /// now: its writer still serves the log and is [`Self::pid`]
    /// ([`crate::broker_health::is_current`]).
    #[must_use]
    pub fn snapshot_now(&self) -> Option<&astream_broker::BrokerHealth> {
        match &self.snapshot {
            Some(Ok(read)) if crate::broker_health::is_current(read, self.pid) => {
                Some(&read.health)
            }
            _ => None,
        }
    }

    /// Whether the broker says its log is poisoned, and why: the `Hello`'s entry 0x04
    /// ([`Self::health`]) first, then the snapshot beside the log, which may be a period old.
    #[must_use]
    pub fn poisoned(&self) -> Option<&str> {
        self.health
            .as_ref()
            .and_then(|h| h.poisoned.as_deref())
            .or_else(|| self.snapshot_now().and_then(|h| h.poisoned.as_deref()))
    }

    /// Name the pid serving [`Self::endpoint`] ([`listener_pid`]), or why it is unknown, its
    /// launchd job, and what its own argv says it serves; no other process is read.
    pub(crate) fn locate(&mut self) {
        self.locate_with(
            listener_pid,
            launchd_label,
            replaced_build,
            |pid| aterm_uds::process::read_process_argv(i32::try_from(pid).ok()?),
            runs_this_build,
        );
    }

    /// [`Self::locate`] through its seams: `holder` names the pid holding the socket, and
    /// `label`, `replaced`, `argv` and `own` (whether it runs this process's own build, asked
    /// only of the node service) read only that pid.
    fn locate_with(
        &mut self,
        mut holder: impl FnMut(&str) -> Result<Option<u32>, String>,
        mut label: impl FnMut(u32) -> Option<String>,
        mut replaced: impl FnMut(u32, &str) -> Option<String>,
        mut argv: impl FnMut(u32) -> Option<Vec<std::ffi::OsString>>,
        mut own: impl FnMut(u32, &str) -> bool,
    ) {
        (self.pid, self.pid_unknown) = match holder(&self.endpoint) {
            Ok(pid) => (pid, None),
            Err(why) => (None, Some(why)),
        };
        self.launchd = self.pid.and_then(&mut label);
        let words = self.pid.and_then(&mut argv);
        self.program = words
            .as_ref()
            .and_then(|w| w.first())
            .map(|w| w.to_string_lossy().into_owned());
        self.replaced = self
            .pid
            .zip(self.program.as_deref())
            .and_then(|(pid, exe)| replaced(pid, exe));
        let endpoint = aterm_uds::holders::SocketPath::of(Path::new(&self.endpoint));
        let parsed = words
            .as_deref()
            .and_then(crate::broker_args::BrokerArgs::from_invocation)
            .filter(|parsed| parsed.serves(&endpoint));
        self.durability = parsed
            .as_ref()
            .and_then(crate::broker_args::BrokerArgs::serving_durability);
        self.log = parsed
            .as_ref()
            .and_then(crate::broker_args::BrokerArgs::log_path);
        let node_log = words
            .as_deref()
            .filter(|_| self.mesh && parsed.is_none())
            .filter(|words| is_node_serve(words))
            .and_then(|_| node_service_log(&self.endpoint));
        self.host = if parsed.is_some() {
            Some(Host::LinkBroker)
        } else if node_log.is_some() {
            Some(Host::NodeService)
        } else {
            None
        };
        self.own_build = false;
        if let Some(log) = node_log {
            self.log = Some(log);
            self.own_build = self.replaced.is_none()
                && self
                    .pid
                    .zip(self.program.as_deref())
                    .is_some_and(|(pid, exe)| own(pid, exe));
        }
    }
}

/// Whether `argv` is the node service's: its program, then [`crate::enable::NODE_SERVE`]'s
/// words (`node serve`), then any.
fn is_node_serve(argv: &[std::ffi::OsString]) -> bool {
    argv.get(1..=crate::enable::NODE_SERVE.len())
        .is_some_and(|words| words.iter().eq(crate::enable::NODE_SERVE.iter()))
}

/// The log the node service hosts beside `endpoint`: `<root>/bus.log` where the endpoint is
/// `<root>/bus.sock` with `<root>` absolute, the two the node service always serves together;
/// `None` for any other socket, which it never serves.
fn node_service_log(endpoint: &str) -> Option<PathBuf> {
    if Path::new(endpoint).file_name()? != crate::enable::BUS_SOCK {
        return None;
    }
    crate::enable::root_of(endpoint).map(|root| root.join(crate::enable::BUS_LOG))
}

/// Whether `pid`, started as `exe`, runs this process's own build: `exe` names this process's
/// executable once both are canonical, and the two processes map the same executable image
/// ([`running_image_inode`]), so neither runs a file an update has since replaced at that path.
fn runs_this_build(pid: u32, exe: &str) -> bool {
    let same_path = match (
        std::fs::canonicalize(exe),
        std::env::current_exe().and_then(std::fs::canonicalize),
    ) {
        (Ok(theirs), Ok(ours)) => theirs == ours,
        _ => false,
    };
    same_path
        && running_image_inode(pid)
            .is_some_and(|theirs| running_image_inode(std::process::id()) == Some(theirs))
}

fn replaced_build(pid: u32, exe: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let exe = Some(exe).filter(|w| w.starts_with('/'))?;
    let running = running_image_inode(pid)?;
    let now = std::fs::metadata(exe).ok()?.ino();
    (now != running).then(|| exe.to_string())
}

/// The inode of the executable image `pid` runs, whatever has happened to its path since:
/// `/proc/<pid>/exe` follows to it even once it is unlinked. macOS reads its lowest
/// vnode-backed mapping in-process ([`aterm_sysprobe::kproc::image_inode`]), the file
/// `lsof -d txt` lists first, which keeps the inode mapped after a new file is renamed over the
/// path; an `lsof` child cost 0.1-0.16 s per read.
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
    let mut cmd = aterm_uds::spawnfd::command(aterm_uds::os_path!("lsof"));
    cmd.args([
        "-n",
        "-P",
        "-a",
        "-p",
        &pid.to_string(),
        "-d",
        "txt",
        "-F",
        "i",
    ]);
    let out =
        aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(15)).ok()?;
    lsof_first_inode(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(any(test, not(any(target_os = "linux", target_os = "macos"))))]
fn lsof_first_inode(text: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let n = l.strip_prefix('i')?;
        (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .then(|| n.parse().ok())
            .flatten()
    })
}

/// The faces a report may read when the broker is guarded and `/f/<F>/>` is not granted: every
/// grant's filter under `/f/<F>/` in the cap files, either mode (a guarded broker authorizes a
/// read by any grant containing it), except the consumer-group names under `/f/<F>/cur/`,
/// which are never published to.
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

/// How much of a cap file's refusal a report prints: its remedy names the path a second time, so a
/// long path's `chmod` is not cut off.
const CAP_REFUSAL_WORDS: usize = 640;

/// The words of `aterm fabric`'s lapsed-ring warning ([`BrokerView::cap_expired`]).
pub const CAP_LAPSED: &str = "refuses this node's cap as lapsed";

/// The words of `aterm fabric`'s poisoned-log warning ([`BrokerView::poisoned`]).
pub const POISONED_WARNING: &str = "says its log is POISONED";

/// The words of the warning for a broker whose flushes failed without poisoning its log.
pub const FLUSH_FAILED_WARNING: &str = "says a flush of its log failed";

impl BrokerView {
    /// A view of `cfg`'s broker socket before anything is read, under this build's mesh switch
    /// (`aterm_types::mesh_switch::MESH`, read here once for the probe).
    fn of(cfg: &Config) -> Self {
        Self {
            endpoint: cfg.broker.clone(),
            mesh: aterm_types::mesh_switch::MESH,
            ..Self::default()
        }
    }
}

/// Name the broker's process ([`BrokerView::locate`]), read the health snapshot beside its log,
/// and reach it as [`reach_broker`] does.
pub(crate) fn probe_broker(cfg: &Config) -> (BrokerView, Option<Conn>) {
    probe_view(cfg, BrokerView::of(cfg))
}

/// [`probe_broker`] from `view`, its endpoint `cfg`'s and its mesh switch set.
fn probe_view(cfg: &Config, mut view: BrokerView) -> (BrokerView, Option<Conn>) {
    view.locate();
    view.snapshot = view
        .log
        .as_deref()
        .and_then(|log| match astream_broker::read_health(log) {
            Ok(read) => read.map(Ok),
            Err(e) => Some(Err(e.to_string())),
        });
    view.root_log = crate::enable::root_of(&cfg.broker)
        .map(|root| root.join(crate::enable::BUS_LOG))
        .filter(|log| log.exists());
    reach(cfg, view)
}

/// Reach the broker, naming no process: connect, `Hello`, the whole ring attached
/// ([`transport::attach_ring`]), and a head query.
pub(crate) fn reach_broker(cfg: &Config) -> (BrokerView, Option<Conn>) {
    reach(cfg, BrokerView::of(cfg))
}

/// Whether a broker answers on `endpoint`: one connect and a `Hello` round trip, in this build's
/// astream protocol or another (a broker answers either way). Nothing is attached or read.
///
/// # Errors
/// Why none answered: the connect, or a socket that accepted and answered no `Hello`.
pub(crate) fn answers(endpoint: &str) -> Result<(), String> {
    let (mut conn, _closer) =
        transport::connect(endpoint, io_timeout()).map_err(|e| format!("connect: {e}"))?;
    match conn.hello() {
        Ok(_) => Ok(()),
        Err(e) if astream_broker::protocol_mismatch(&e).is_some() => Ok(()),
        Err(e) => Err(format!("the socket accepted but no broker answered: {e}")),
    }
}

fn reach(cfg: &Config, mut view: BrokerView) -> (BrokerView, Option<Conn>) {
    let started = Instant::now();
    let mut conn = match transport::connect(&cfg.broker, io_timeout()) {
        Ok((c, _)) => c,
        Err(e) => {
            view.error = Some(format!("connect: {e}"));
            view.last_word = supervisor_last_word(cfg, &view);
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
                view.last_word = supervisor_last_word(cfg, &view);
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
                view.cap_unread = true;
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

/// The broker job's last word in `cfg`'s root ([`crate::enable::supervisor_last_word`]), only
/// where a broker job keeps the bus `view` reads ([`BrokerView::keeper`]): the node service
/// writes no `broker.err`, so one there is a retired job's.
fn supervisor_last_word(cfg: &Config, view: &BrokerView) -> Option<String> {
    if view.keeper() == crate::enable::Keeper::Node {
        return None;
    }
    let root = crate::enable::root_of(&cfg.broker)?;
    crate::enable::supervisor_last_word(&root)
}

/// The BROKER section's `health` row, and doctor's `health:` line: the log's health as the
/// broker's own `Hello` said it ([`crate::broker_health::in_band`]).
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

/// The BROKER section's `host` row, and doctor's `host:` line, where the node service serves the
/// socket ([`Host::NodeService`]): its job and pid, the log it hosts, the tier it was opened at
/// ([`BrokerView::tier`]), and its build where it is this aterm's ([`BrokerView::build`]), else
/// the program it runs, said REPLACED where an update replaced that file since it started.
/// `None` for any other holder, which the `pid` row names.
#[must_use]
pub fn host_row(b: &BrokerView) -> Option<String> {
    if b.host != Some(Host::NodeService) {
        return None;
    }
    let pid = b.pid.map_or_else(String::new, |pid| format!(", pid {pid}"));
    let log = b.log.as_ref().map_or_else(
        || "bus.log".to_string(),
        |log| safe(&log.display().to_string(), 256),
    );
    let tier = b.tier().map_or_else(
        || "a tier it does not say".to_string(),
        |d| d.name().to_string(),
    );
    let build = match (b.build(), b.program.as_deref(), b.replaced.is_some()) {
        (Some(id), _, _) => format!("build {} (this aterm's)", safe(&id, 64)),
        (None, Some(program), true) => format!(
            "program {} — A REPLACED BUILD: that path is another file now",
            safe(program, 256)
        ),
        (None, Some(program), false) => format!("program {}", safe(program, 256)),
        (None, None, _) => "build unknown".to_string(),
    };
    Some(format!(
        "the node service ({}{pid}) hosts {log} at {tier}, {build}",
        crate::enable::NODE_JOB
    ))
}

/// The BROKER section's `holds` row at `now_ms`: what the log holds and who is attached, from
/// astream's snapshot beside the log ([`crate::broker_health::snapshot_row`]), said as the
/// serving broker's own only when astream's reader says so.
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
        None if b.host == Some(Host::NodeService) => format!(
            "- (no health snapshot beside {}: the node service keeps one there, and journals why \
             where it cannot write it)",
            safe(&log.display().to_string(), 256)
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
                // The broker process's own tally, as the holds row quotes it; null in a
                // snapshot an older broker wrote, which carries none.
                (
                    "process_flushes",
                    h.process_flushes.map_or(J::Null, |t| {
                        J::Obj(vec![
                            ("full", J::Num(t.full)),
                            ("barrier", J::Num(t.barrier)),
                            ("plain", J::Num(t.plain)),
                        ])
                    }),
                ),
                // What of the log the broker keeps decoded in memory, as the holds row's
                // `resident=` quotes it; null in a snapshot an older broker wrote.
                (
                    "resident",
                    h.resident.map_or(J::Null, |r| {
                        J::Obj(vec![
                            ("records", J::Num(r.records)),
                            ("bytes", J::Num(r.bytes)),
                        ])
                    }),
                ),
                // How long the broker process's flushes took, and its slow ones, each with
                // its index, kind, start and end on the clock named, as the holds row sums them up,
                // and how many calls were still running as it read them; null in a snapshot an
                // older broker wrote.
                (
                    "process_flush_times",
                    h.process_flush_times.as_ref().map_or(J::Null, |t| {
                        J::Obj(vec![
                            ("clock", J::s(t.clock.word())),
                            ("full_ns", J::Num(t.full_ns)),
                            ("barrier_ns", J::Num(t.barrier_ns)),
                            ("plain_ns", J::Num(t.plain_ns)),
                            ("slow_count", J::Num(t.slow_count)),
                            (
                                "slow",
                                J::Arr(
                                    t.slow
                                        .iter()
                                        .map(|f| {
                                            J::Obj(vec![
                                                ("index", J::Num(f.index)),
                                                ("kind", J::s(f.kind.word())),
                                                ("start_ns", J::Num(f.start_ns)),
                                                ("end_ns", J::Num(f.end_ns)),
                                            ])
                                        })
                                        .collect(),
                                ),
                            ),
                            ("running", J::Num(t.running)),
                        ])
                    }),
                ),
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

/// How much the top-level `aterm doctor`'s `fabric:` row asks of its reader, in that report's
/// three weights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryLevel {
    /// The broker answers, the bus reads, and it says its log takes writes.
    Ok,
    /// Nothing to fix here: the fabric is off, or a broker that answers does not say whether
    /// its log is poisoned.
    Note,
    /// Mail does not move: the broker does not answer, its bus cannot be read, or it says its
    /// log is poisoned.
    Warn,
}

/// The fabric in one row, for the top-level `aterm doctor` ([`doctor_summary`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorSummary {
    pub level: SummaryLevel,
    pub text: String,
    /// The observation did not establish health, independently of its display level.
    pub unmeasured: bool,
    /// Who serves the fabric's socket, as this read found it: `Some(Some(pid))` the one process
    /// found holding its listener (the probe's [`BrokerView::pid`]), `Some(None)` nobody, since
    /// the fabric is off, and `None` when nothing read says: the probe found no one holder (none
    /// holds it, two do, or the scan cannot read), which proves no broker is not the one serving.
    pub serving: Option<Option<u32>>,
}

/// The top-level `aterm doctor`'s `fabric:` row: probes the broker the command names
/// (`$ATERM_FABRIC_COMMAND` in a development build, `aterm.toml`, or the `fabric.toml`
/// rendezvous file) without asking local instances or reading the roster.
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
        |cfg| probe_broker(cfg).0,
    )
}

fn summary_for(
    found: Result<Option<Vec<String>>, String>,
    withheld: Option<&str>,
    probe: impl FnOnce(&Config) -> BrokerView,
) -> DoctorSummary {
    let command = match found {
        Ok(Some(command)) => command,
        Ok(None) => {
            return DoctorSummary {
                level: SummaryLevel::Note,
                unmeasured: false,
                serving: Some(None),
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
                serving: None,
                text: format!(
                    "the fabric's config cannot be read ({}) — `aterm fabric` says where",
                    safe(&e, 256)
                ),
            };
        }
    };
    match (bridge_config(&command), withheld) {
        (Ok(cfg), None) => summary_of(&probe(&cfg)),
        (Ok(_), Some(why)) => DoctorSummary {
            level: SummaryLevel::Note,
            unmeasured: true,
            serving: None,
            text: format!("not read ({why})"),
        },
        (Err(e), _) => DoctorSummary {
            level: SummaryLevel::Warn,
            unmeasured: true,
            serving: None,
            text: format!(
                "the [fabric] command does not parse ({}) — `aterm fabric` says where it is",
                safe(&e, 256)
            ),
        },
    }
}

/// [`doctor_summary`]'s verdict on one probe, pure: the broker's own words (its `Hello`'s
/// served tier and health entries), never the argv's. Its subject is who serves the socket: the
/// node service where it does, its socket where it keeps the bus and nothing is named, else the
/// broker.
#[must_use]
pub fn summary_of(b: &BrokerView) -> DoctorSummary {
    let socket = safe(&b.endpoint, 256);
    let at = match (b.host, b.keeper()) {
        (Some(Host::NodeService), _) => format!("node service at {socket}"),
        (None, crate::enable::Keeper::Node) => format!("node service's socket {socket}"),
        (Some(Host::LinkBroker) | None, _) => format!("broker at {socket}"),
    };
    let fix = "`aterm fabric doctor` names the fix";
    let warn = |text: String, unmeasured| DoctorSummary {
        level: SummaryLevel::Warn,
        unmeasured,
        text,
        serving: b.pid.map(Some),
    };
    if let Some(skew) = b.protocol {
        return warn(
            format!(
                "the {at} speaks astream protocol {} and this aterm speaks {}: no record \
             lands and none is delivered — {fix}",
                skew.broker, skew.client
            ),
            false,
        );
    }
    if !b.reachable {
        return warn(
            format!(
                "the {at} does not answer ({}) — {fix}",
                safe(b.error.as_deref().unwrap_or("unknown"), 256)
            ),
            true,
        );
    }
    if let Some(why) = b.poisoned() {
        return warn(
            format!(
                "the {at} {POISONED_WARNING} ({}): it refuses every write until the log \
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
                "the {at} answers and {served}, but the bus could not be read ({}) — {fix}",
                safe(b.error.as_deref().unwrap_or("unknown"), b.error_words())
            ),
            true,
        );
    }
    match &b.health {
        Some(_) => DoctorSummary {
            level: SummaryLevel::Ok,
            unmeasured: false,
            serving: b.pid.map(Some),
            text: format!(
                "the {at} answers, {served}, and says its log is not poisoned \
                 (`aterm fabric` reads the rest)"
            ),
        },
        None => DoctorSummary {
            level: SummaryLevel::Note,
            unmeasured: true,
            serving: b.pid.map(Some),
            text: format!(
                "the {at} answers and {served}, and does not say whether its log is \
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
    /// Who sent it: the cap-forced `<src>` segment, or `<sid>@<node>` when a node attests which
    /// of its sessions spoke (the bridge's own rule); `<person>@<node>` for a person's post the
    /// Mac `<node>` carried, once its seal verifies at the reader's clock
    /// ([`crate::person_key::carried_by`], the bridge's own check); and on a person's lane, the
    /// `<sid>@<node>` this Mac's bridge names a session on another Mac by ([`render_from`]).
    pub from: String,
    /// Who it is for: `<sid>@<node>`, `fleet`, `all`, or `-` for a node's own face.
    pub to: String,
    /// The record's kind: the `in` kind, or the face's leaf (`presence`, …).
    pub kind: String,
    /// The body's length in bytes ([`Traffic::text`]'s, as the record holds it).
    pub len: usize,
    /// The receiver's trust label ([`crate::render::trust_of`]) of the sender's principal: the
    /// lane's writer, the cap-forced segment of its address, save for a carried person's post
    /// whose seal verifies, which is the person's (`human`). A claim of a person that does not
    /// verify leaves the writer, the Mac whose lane brought it, accountable.
    pub trust: &'static str,
    /// The body ([`traffic_of`]): the decoded `text=` when the record's first line carries one,
    /// else that whole first line — only `tail --bodies` ever prints it.
    pub text: String,
}

/// The sender of an `in` record the writer `src` wrote, its body naming `body_from`: a node's
/// session, `<sid>@<src>`, when a node names a bare session (`s-`); on a person's lane
/// (`person_lane`, `/f/<F>/in/p/<person>/<node>/<kind>`) written by a node, a `<sid>@<node>` as
/// it is, the session on another Mac whose answer or receipt this Mac's bridge handed the person
/// (§11 C3d): only this Mac's own bridge writes `in/p/*/<this node>/*`, since no peer's ring
/// reaches a person's lane; else `src`.
fn render_from(person_lane: bool, src: &str, body_from: Option<&str>) -> String {
    let principal = crate::subject::is_principal;
    let session = |sid: &str| sid.starts_with("s-") && principal(sid);
    let by_node = src.starts_with("n-");
    match body_from {
        Some(sid) if by_node && session(sid) => format!("{sid}@{src}"),
        Some(named)
            if person_lane
                && by_node
                && named.split_once('@').is_some_and(|(sid, node)| {
                    session(sid) && node.starts_with("n-") && principal(node)
                }) =>
        {
            named.to_string()
        }
        _ => src.to_string(),
    }
}

/// Classify one record, as a reader whose clock reads `now_ms` sees it (a carried person's
/// certificate is judged in force or lapsed at it). Its body ([`Traffic::text`]) and `len` follow
/// [`first_line_body`].
#[must_use]
pub fn traffic_of(fleet: &str, off: u64, subject: &str, raw: &[u8], now_ms: u64) -> Traffic {
    if raw.starts_with(b"v=2 ") {
        if let Some(t) = mirrored_traffic(fleet, off, subject, raw, now_ms) {
            return t;
        }
    }
    let mut t = traffic_of_body(fleet, off, subject, Body::decode(raw).0, now_ms);
    if let Some((text, len)) = first_line_body(raw) {
        t.text = text;
        t.len = len;
    }
    t
}

fn mirrored_traffic(
    fleet: &str,
    off: u64,
    subject: &str,
    raw: &[u8],
    now_ms: u64,
) -> Option<Traffic> {
    let text = std::str::from_utf8(raw).ok()?;
    let b = aterm_slog::decode_bus(text).ok()?;
    let mut t = traffic_of_body(fleet, off, subject, Body::default(), now_ms);
    if t.kind != "ev" {
        return None;
    }
    t.t_ms = b.rec.t;
    t.trust = b.rec.src.word();
    (t.text, t.len) = first_line_body(raw).unwrap_or_else(|| {
        let text = Body::decode(raw).0.text;
        let len = text.len();
        (text, len)
    });
    Some(t)
}

/// THE BODY OF A RECORD WITH NO `text=` (fix unit E5, E2E-10): when `raw`'s first line (its
/// bytes before the first newline) carries no `text=` token, the body `tail --bodies` prints is
/// that whole first line, read as UTF-8 lossily, and `len` its bytes: a presence row's fields,
/// a halt's `state=`, an `ev` record's session-log line. `None` when the first line carries
/// `text=`, whose decoded text is the body. Never a byte after the first newline: what a body
/// carries past its header line is a payload (keystrokes, in a drive record a log from before
/// the drive face's cut may still hold).
fn first_line_body(raw: &[u8]) -> Option<(String, usize)> {
    let head = raw.split(|b| *b == b'\n').next().unwrap_or_default();
    if head
        .split(|b| *b == b' ')
        .any(|tok| tok.starts_with(b"text="))
    {
        return None;
    }
    Some((String::from_utf8_lossy(head).into_owned(), head.len()))
}

/// [`traffic_of`] over a body already decoded, at the reader's clock `now_ms`.
pub(crate) fn traffic_of_body(
    fleet: &str,
    off: u64,
    subject: &str,
    body: Body,
    now_ms: u64,
) -> Traffic {
    let relayed = body.via.is_some();
    let segs: Vec<&str> = subject.split('/').collect();
    let seg = |i: usize| safe(segs.get(i).copied().unwrap_or("-"), 128);
    let face = if segs.len() >= 5 && segs[0].is_empty() && segs[1] == "f" && segs[2] == fleet {
        segs[3]
    } else {
        ""
    };
    let (from, to, kind, trust) = match (face, segs.len()) {
        ("in", 8) => {
            let (from, trust) = in_sender(fleet, &segs, &body, relayed, now_ms);
            (from, format!("{}@{}", seg(5), seg(4)), seg(7), trust)
        }
        ("fleet", 6) => (
            seg(4),
            "fleet".to_string(),
            seg(5),
            trust_of(segs[4], relayed),
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

/// Who sent the record `body` on the `in` lane `segs` (`/f/<F>/in/<node>/<sid>/<src>/<kind>`,
/// split at `/`), and its trust label, at the reader's clock `now_ms`. A record on a session's
/// lane that names a person ([`crate::person_key::BY_TOKEN`]) is the person's,
/// `<person>@<src>` (`human`), only once its seal verifies as the bridge would check it
/// ([`crate::person_key::carried_by`]); one whose claim does not is the writer's alone, the Mac
/// whose lane brought it, its body's `from=` unread, so a claimed name is never shown as the
/// sender. Any other record is [`render_from`]'s, its trust the writer's.
fn in_sender(
    fleet: &str,
    segs: &[&str],
    body: &Body,
    relayed: bool,
    now_ms: u64,
) -> (String, &'static str) {
    let (sid, src, kind) = (segs[5], segs[6], segs[7]);
    let session = sid.starts_with("s-") && crate::subject::is_principal(sid);
    let carried = if session {
        crate::person_key::carried_by(fleet, sid, src, kind, body, now_ms)
    } else {
        Ok(None)
    };
    match carried {
        Ok(Some(person)) => (safe(&person, 128), trust_of(&person, relayed)),
        Ok(None) => (
            safe(&render_from(segs[4] == "p", src, body.from.as_deref()), 128),
            trust_of(src, relayed),
        ),
        Err(_) => (safe(src, 128), trust_of(src, relayed)),
    }
}

/// TRAFFIC: the newest `n` records the broker holds of the fleet (or of `faces`), as a reader at
/// `now_ms` classifies them ([`traffic_of`]), read TOLD
/// ([`transport::fetch_told_kept`]): the fleet's root reaches the bulk stream's class and the
/// held mail and presence below its cut, so its oldest is the cut, and a plain read on at it
/// would leave out the records retention still holds below it (astream's
/// `retention::oldest`; audit finding BUS-1).
fn last_records(
    conn: &mut Conn,
    fleet: &str,
    faces: Option<&[String]>,
    head: u64,
    n: usize,
    now_ms: u64,
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
                    transport::fetch_told_kept(conn, next, filter, transport::POST_PAGE)?;
                for (off, subject, raw) in &page {
                    let older = merged.len() >= n
                        && merged
                            .first_key_value()
                            .is_none_or(|(first, _)| off < first);
                    if older || merged.contains_key(off) {
                        continue;
                    }
                    merged.insert(*off, traffic_of(fleet, *off, subject, raw, now_ms));
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

/// The `t=` of the record at `off` on `filter`, read TOLD ([`transport::fetch_told_kept`]): a
/// record retention still holds below the filter's oldest has its age too (the fleet's `in`
/// lanes span one reader group per node and the person lanes', and the oldest is the greatest
/// of their floors). `None` when no record of the filter is at `off`.
fn record_t(conn: &mut Conn, filter: &str, off: u64) -> Option<u64> {
    let ((page, _), _) = transport::fetch_told_kept(conn, off, filter, 1).ok()?;
    let (at, _, raw) = page.first()?;
    (*at == off).then(|| Body::decode(raw).0.t)
}

/// An `ask` or `task` whose `dl=` has passed with no `answer`, `report` or `ack` carrying its
/// offset as `re=` anywhere on the fleet's `in` lanes, of the lanes whose verdicts above it the
/// bus still keeps (`overdue_work`).
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
    /// Whether the asker's own bridge, which owns that verdict, has already put
    /// `expired re=<off>` on the asker's lane: `None` when retention gave up records of the
    /// asker's `expired` verdicts above the ask, so the bus no longer says.
    pub expired: Option<bool>,
}

/// How far below the head the deadline scan ([`overdue_work`]) reads for asks.
pub(crate) const OVERDUE_SCAN_SPAN: u64 = 1 << 16;

/// The overdue asks on the fleet's `in` lanes (or the `in` faces of `faces`) within the last
/// [`OVERDUE_SCAN_SPAN`] offsets, read TOLD ([`transport::fetch_told_kept`]). Those lanes span
/// one reader group per node and the person lanes' (`/f/<F>/cur/p/node/inbox`, which nothing
/// commits), and a filter's oldest is the greatest floor of what it reaches, so a node's read
/// mail given up past an unread ask to a person raises it above that ask, which retention
/// still holds: a read on at the floor would never list it (audit finding BUS-1). A broker
/// that does not tell fetches (one older than this build's astream) is read on at the floor,
/// as before.
///
/// Each ask is judged only on verdicts the bus still keeps ([`verdicts_kept`]): an ask is
/// listed only while every `answer`, `report` and `ack` that could reach its asker's lane
/// above it is kept (retention gives up the asker node's read mail with the cut while it
/// holds the ask, so a reply given up beside a held ask would read as none), and its
/// `expired` is `None` where the asker's `expired` verdicts above it are not all kept. An
/// ask past its deadline that is so left out is counted ([`Deadlines::unjudged`]), never
/// listed: a reply given up above it may have been its answer or another ask's (audit
/// finding BUS2-3).
fn overdue_work(
    conn: &mut Conn,
    fleet: &str,
    faces: Option<&[String]>,
    head: u64,
    now_ms: u64,
) -> io::Result<Deadlines> {
    let filters = in_filters(fleet, faces);
    let mut scan = OverdueScan::default();
    let mut fed: BTreeSet<u64> = BTreeSet::new();
    for filter in &filters {
        let mut next = head.saturating_sub(OVERDUE_SCAN_SPAN);
        loop {
            let ((page, (after, now_head)), _) =
                transport::fetch_told_kept(conn, next, filter, transport::POST_PAGE)?;
            for (off, subject, raw) in &page {
                if fed.insert(*off) {
                    scan.feed(fleet, *off, subject, raw, now_ms);
                }
            }
            if after <= next || after >= now_head {
                break;
            }
            next = after;
        }
    }
    let mut oldest = BTreeMap::new();
    let mut found = Deadlines::default();
    for (mut o, lane) in scan.finish_on_lanes(now_ms) {
        let above = o.off.saturating_add(1);
        if !verdicts_kept(conn, &mut oldest, &filters, &lane, &ANSWER_KINDS, above)? {
            found.unjudged = found.unjudged.saturating_add(1);
            continue;
        }
        if o.expired == Some(false)
            && !verdicts_kept(conn, &mut oldest, &filters, &lane, &["expired"], above)?
        {
            o.expired = None;
        }
        found.overdue.push(o);
    }
    Ok(found)
}

/// What the deadline scan ([`overdue_work`]) found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Deadlines {
    /// The asks past their deadline with no verdict, judged on verdicts the bus keeps.
    overdue: Vec<Overdue>,
    /// The asks past their deadline it cannot judge: the bus no longer keeps every `answer`,
    /// `report` and `ack` above them on their asker's lane (or no face read reaches it), so it
    /// cannot say whether they had one.
    unjudged: usize,
}

/// Whether the bus keeps, from `from` on, every record of `kinds` on the asker's `lane`
/// ([`verdict_lane`]) the scan of `faces` reads: the oldest of each (`Client::oldest`, the
/// offset at and above which a filter is complete; astream's `retention::oldest` takes the
/// greatest floor of the subjects it matches, and a subject's floor is one past its newest
/// record given up) is at or below `from`. Each filter's oldest is asked once (`oldest`). A
/// broker that names no oldest predates retention's held records (no aterm pin carried them
/// without it) and serves everything from its base, so what it serves is whole. `false` when
/// no face reaches the lane: the scan reads none of those verdicts.
fn verdicts_kept(
    conn: &mut Conn,
    oldest: &mut BTreeMap<String, Option<u64>>,
    faces: &[String],
    lane: &str,
    kinds: &[&str],
    from: u64,
) -> io::Result<bool> {
    for kind in kinds {
        let verdicts = format!("{lane}/{kind}");
        let mut reached = false;
        for face in faces {
            let Some(filter) = filter_meet(&verdicts, face) else {
                continue;
            };
            reached = true;
            let floor = match oldest.get(&filter) {
                Some(floor) => *floor,
                None => {
                    let (floor, _) = conn.oldest(&filter)?;
                    oldest.insert(filter, floor);
                    floor
                }
            };
            if floor.is_some_and(|floor| floor > from) {
                return Ok(false);
            }
        }
        if !reached {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Where the verdicts on an ask at `subject` (an `in` lane, `/f/<F>/in/<a>/<b>/<src>/<kind>`)
/// from `from` (its body's `from=`) land, as a filter of the lane with the verdict's source
/// left `*` (the kind follows): a session's ask (`src` a node, `from` its session) is answered
/// on that session's lane, `/f/<F>/in/<src>/<from>/*`, and expired there by its bridge; a
/// person's on the person's lane, `/f/<F>/in/p/<src>/*` (bridge.rs's reply lanes). An asker
/// the record does not name gets every `in` lane, `/f/<F>/in/*/*/*`.
fn verdict_lane(fleet: &str, subject: &str, from: Option<&str>) -> String {
    let src = subject.split('/').nth(6).unwrap_or("");
    let principal = crate::subject::is_principal;
    let session = from.filter(|sid| sid.starts_with("s-") && principal(sid));
    if src.starts_with("n-") && principal(src) {
        if let Some(sid) = session {
            return format!("/f/{fleet}/in/{src}/{sid}/*");
        }
    } else if !src.starts_with("s-") && principal(src) {
        return format!("/f/{fleet}/in/p/{src}/*");
    }
    format!("/f/{fleet}/in/*/*/*")
}

/// The filter of exactly the subjects both `a` and `b` match (segments literal, `*` for one,
/// a trailing `>` for one or more), or `None` where no subject matches both.
fn filter_meet(a: &str, b: &str) -> Option<String> {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split('/').collect(), b.split('/').collect());
    let mut out: Vec<&str> = Vec::with_capacity(a.len().max(b.len()));
    let mut i = 0usize;
    loop {
        match (a.get(i).copied(), b.get(i).copied()) {
            (None, None) => return Some(out.join("/")),
            (Some(">"), Some(_)) => {
                out.extend(b.iter().skip(i));
                return Some(out.join("/"));
            }
            (Some(_), Some(">")) => {
                out.extend(a.iter().skip(i));
                return Some(out.join("/"));
            }
            (Some(x), Some(y)) if x == y || y == "*" => out.push(x),
            (Some("*"), Some(y)) => out.push(y),
            _ => return None,
        }
        i = i.saturating_add(1);
    }
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

/// The overdue asks among `records` as of `now_ms`, for tests: a `WORK_KINDS` record whose
/// `t + dl` is past, and no `answer|report|ack` with `re=` naming it.
#[cfg(test)]
#[must_use]
pub(crate) fn overdue_of(
    fleet: &str,
    records: &[(u64, String, Vec<u8>)],
    now_ms: u64,
) -> Vec<Overdue> {
    let mut scan = OverdueScan::default();
    for (off, subject, raw) in records {
        scan.feed(fleet, *off, subject, raw, now_ms);
    }
    scan.finish_on_lanes(now_ms)
        .into_iter()
        .map(|(o, _)| o)
        .collect()
}

#[derive(Default)]
struct OverdueScan {
    settled: BTreeSet<u64>,
    expired: BTreeSet<u64>,
    /// Each work record with a deadline, its `dl=` and its asker's lane ([`verdict_lane`]).
    asks: Vec<(Traffic, u64, String)>,
}

impl OverdueScan {
    /// Take the record at `off`, its sender read at the reader's clock `now_ms`
    /// ([`traffic_of_body`]).
    fn feed(&mut self, fleet: &str, off: u64, subject: &str, raw: &[u8], now_ms: u64) {
        let (body, _) = Body::decode(raw);
        let (dl, re) = (body.dl, body.re);
        let lane = verdict_lane(fleet, subject, body.from.as_deref());
        let t = traffic_of_body(fleet, off, subject, body, now_ms);
        match t.kind.as_str() {
            k if WORK_KINDS.contains(&k) => {
                if let Some(dl) = dl {
                    let t = Traffic {
                        text: String::new(),
                        ..t
                    };
                    self.asks.push((t, dl, lane));
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

    /// The overdue asks among what was fed, each with its asker's lane ([`verdict_lane`]).
    fn finish_on_lanes(self, now_ms: u64) -> Vec<(Overdue, String)> {
        let Self {
            settled,
            expired,
            asks,
        } = self;
        asks.into_iter()
            .filter(|(t, _, _)| !settled.contains(&t.off))
            .filter_map(|(t, dl, lane)| {
                let at = t.t_ms;
                let due = at.saturating_add(dl);
                (at > 0 && now_ms > due).then(|| {
                    let o = Overdue {
                        off: t.off,
                        expired: Some(expired.contains(&t.off)),
                        from: t.from,
                        to: t.to,
                        kind: t.kind,
                        dl,
                        late_ms: now_ms - due,
                    };
                    (o, lane)
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
    pub kind: String,
}

/// What one local instance says about one of its sessions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalSession {
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
    /// The last hold transition's `(reason, origins)` while held ([`last_hold`]); `fleet,local`
    /// when both slots stand.
    pub hold_why: Option<(String, String)>,
    /// The broadcast topics this session has opted into (`topic ls`), in topic order.
    pub topics: Vec<String>,
    /// The inbox header's `given_up=` (with `given_up_count=`): the newest range of this node's
    /// unread mail the broker's retention gave up before the bridge read it, as told to this
    /// session.
    pub given_up: Option<GivenUpRange>,
    /// The inbox header's `lane_given_up=`: the newest range of this session's own lane given up
    /// before it handled the mail there.
    pub lane_given_up: Option<GivenUpRange>,
    /// The `topic ls` rows' `given_up=`: per topic, the newest range of its broadcasts given up
    /// before they were replayed to this session, in topic order.
    pub topic_given_up: Vec<(String, GivenUpRange)>,
    /// A ctl read that failed for this session.
    pub error: Option<String>,
}

/// A range of bus offsets the broker's retention gave up, as an instance names one:
/// `@<from>..@<to>` (`inbox`'s `given_up=` and `lane_given_up=`, a `topic ls` row's `given_up=`,
/// `fabric status`'s `mail_gap=`), and its records where the broker counted them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GivenUpRange {
    pub from: u64,
    pub to: u64,
    pub records: Option<u64>,
}

impl GivenUpRange {
    /// `@<from>..@<to>` (`from <= to`), and `count` as its records where it is a number.
    #[must_use]
    pub fn parse(range: &str, count: Option<&str>) -> Option<Self> {
        let dec = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        let (from, to) = range.strip_prefix('@')?.split_once("..@")?;
        let (from, to) = (dec(from)?, dec(to)?);
        (from <= to).then(|| Self {
            from,
            to,
            records: count.and_then(dec),
        })
    }

    /// `@<from>..@<to>`, as the instance named it.
    #[must_use]
    pub fn range(&self) -> String {
        format!("@{}..@{}", self.from, self.to)
    }

    /// The range, and its records where they were counted: `@10..@42 (3 records)`.
    #[must_use]
    pub fn said(&self) -> String {
        match self.records {
            Some(1) => format!("{} (1 record)", self.range()),
            Some(n) => format!("{} ({n} records)", self.range()),
            None => self.range(),
        }
    }
}

/// What one local instance says about itself.
#[derive(Debug, Clone, Default)]
pub struct InstanceView {
    pub pid: u32,
    pub sock: String,
    /// Whether `aterm ctl --pid <pid>` reaches it ([`aterm_ctl::pid_reaches`]).
    pub pid_reaches: bool,
    /// Why it could not be asked, when it could not.
    pub error: Option<String>,
    /// `fabric status`'s `state=`.
    pub fabric: Option<String>,
    /// `fabric status`'s `reason=`: why the bridge's broker link is down when `state=stalled`,
    /// else `-`.
    pub reason: Option<String>,
    /// `fabric status`'s `rtt_ms=`: the bridge's last acknowledged round trip to the broker.
    pub rtt_ms: Option<u64>,
    /// `fabric status`'s `link_age_ms=`: how long ago that ack was.
    pub link_age_ms: Option<u64>,
    /// `fabric status`'s `supervised=`.
    pub supervised: Option<bool>,
    /// `fabric status`'s `build=` (the instance's own build) and `bridge_build=` (the build its
    /// bridge named in `hello`); absent from an instance or a bridge that predates them.
    pub build: Option<String>,
    pub bridge_build: Option<String>,
    /// `fabric status`'s `lane_faults=`: bridge lines the instance could not parse.
    pub lane_faults: Option<u64>,
    /// `fabric status`'s `mail_gaps=`: the ranges of this node's unread mail its bridges told it
    /// the broker's retention gave up before they read them, since the instance started; absent
    /// before the first, and from an instance that predates the field.
    pub mail_gaps: Option<u64>,
    /// `fabric status`'s `mail_gap=` (with `mail_gap_count=`): the newest of those ranges, while
    /// its offsets name the bridge's log.
    pub mail_gap: Option<GivenUpRange>,
    /// The command it runs (or was launched with), decoded into its argv ([`status_argv`]).
    pub command: Option<Vec<String>>,
    /// The program that command runs ([`bridge_program`]), resolved when the report was read so
    /// a substituted program is named beside its flags.
    pub program: Option<String>,
    /// Its bridge children: the aterm children the census proves by their executable whose words
    /// are `aterm link serve …` ([`bridges_among`]); `None` when its children could not be read,
    /// which says nothing of whether it has a bridge.
    pub bridge_pids: Option<Vec<u32>>,
    /// The node its bridge answers as, from that command's state dir.
    pub node: Option<String>,
    /// The state dir its bridge command names.
    pub state_dir: Option<String>,
    pub sessions: Vec<LocalSession>,
}

/// The BRIDGES table's FABRIC cell: the state, plus the `stalled` reason or the last acked
/// round trip and its age.
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

/// Fill a session's inbox numbers from the two peeked replies: `inbox 0 --peek --meta` (its
/// header counts every unlisted row as `pending=`, and it lists every un-landed post) and
/// `inbox --peek --meta` (every message row).
pub fn fill_inbox(s: &mut LocalSession, header0: &str, posts: &[String], full: &[String]) {
    let num = |k: &str| kv(header0, k).and_then(crate::parse_uint::<u64>);
    s.hold = kv(header0, "hold").and_then(crate::parse_uint::<u8>);
    s.holder = kv(header0, "holder").unwrap_or("-").to_string();
    s.seen = num("seen");
    s.dropped = num("dropped");
    s.pending = num("pending");
    s.given_up =
        kv(header0, "given_up").and_then(|r| GivenUpRange::parse(r, kv(header0, "given_up_count")));
    s.lane_given_up = kv(header0, "lane_given_up").and_then(|r| GivenUpRange::parse(r, None));
    s.posts = Some(posts.iter().filter(|r| r.starts_with("post ")).count() as u64);
    let seen = s.seen.unwrap_or(0);
    s.unhandled = full
        .iter()
        .filter_map(|r| {
            let mut words = r.split_whitespace();
            if words.next() != Some("msg") {
                return None;
            }
            let id = crate::parse_uint::<u64>(words.next()?)?;
            Some(MsgMeta {
                id,
                off: crate::parse_uint(kv(r, "off")?)?,
                from: kv(r, "from").unwrap_or("-").to_string(),
                kind: kv(r, "kind").unwrap_or("-").to_string(),
            })
        })
        .filter(|m| m.id > seen)
        .collect();
    s.unread = Some((s.unhandled.len() as u64).saturating_sub(s.pending.unwrap_or(0)));
}

/// Fill a session's topics, and the range of each that retention gave up, from its `topic ls`
/// rows (`topic <t> since=<s>[ given_up=@<a>..@<b>]`).
pub fn fill_topics(s: &mut LocalSession, rows: &[String]) {
    s.topics.clear();
    s.topic_given_up.clear();
    for rest in rows.iter().filter_map(|row| row.strip_prefix("topic ")) {
        let Some(topic) = rest.split_whitespace().next() else {
            continue;
        };
        s.topics.push(topic.to_string());
        if let Some(gap) = kv(rest, "given_up").and_then(|r| GivenUpRange::parse(r, None)) {
            s.topic_given_up.push((topic.to_string(), gap));
        }
    }
}

/// The `(reason, origins)` of the last `kind=hold` transition in a `timeline` reply, when it
/// left the session held.
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
            fill_topics(&mut s, t.rows());
        }
        Ok(())
    })();
    if let Err(e) = result {
        s.error = Some(e);
    }
    s
}

/// The view of the instance at `pid`, serving `sock`: its `fabric status`, and with `full` also its
/// bridge children and its sessions.
fn read_instance(pid: u32, sock: &str, full: bool) -> Option<InstanceView> {
    let mut v = InstanceView {
        pid,
        sock: sock.to_string(),
        pid_reaches: aterm_ctl::pid_reaches(pid, sock),
        ..InstanceView::default()
    };
    if full {
        v.bridge_pids = aterm_ctl::census::aterm_children(pid)
            .ok()
            .map(|children| bridges_among(&children));
    }
    let mut ctl = match open_instance(pid, sock, io_timeout()) {
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
            v.rtt_ms = kv(h, "rtt_ms").and_then(crate::parse_uint);
            v.link_age_ms = kv(h, "link_age_ms").and_then(crate::parse_uint);
            v.supervised = kv(h, "supervised").map(|s| s == "1");
            v.build = kv(h, "build").filter(|b| *b != "-").map(str::to_string);
            v.bridge_build = kv(h, "bridge_build")
                .filter(|b| *b != "-")
                .map(str::to_string);
            v.lane_faults = kv(h, "lane_faults").and_then(crate::parse_uint);
            v.mail_gaps = kv(h, "mail_gaps").and_then(crate::parse_uint);
            v.mail_gap =
                kv(h, "mail_gap").and_then(|r| GivenUpRange::parse(r, kv(h, "mail_gap_count")));
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
    if !full {
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
    if timeout.is_zero() {
        return Err("instance reply deadline expired".into());
    }
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
    ctl.get_ref()
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    ctl.get_ref()
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
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
    pub sid: String,
    /// The local instance hosting it, if one here does.
    pub pid: Option<u32>,
    /// The node its presence row (or its instance's bridge) names.
    pub node: String,
    /// The bus presence `state=` as the report reads it (`away` for a `live` row whose node's
    /// will fired, `gone` for a retired one still shown), or `-` when the bus has no row for it.
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
    pub node: String,
    /// Its `host=`.
    pub host: String,
    /// Its `state=` as the node's row carries it: `live`, or `away` once its will fired (`gone`
    /// from an older bridge).
    pub state: String,
    /// Its `fabric=`.
    pub fabric: String,
    /// The astream protocol it speaks (`proto=`), `-` from a node whose bridge predates the token.
    pub proto: String,
    /// The aterm release it runs (`aterm=`), `-` likewise.
    pub aterm: String,
}

impl NodeRow {
    /// The state as the report says it: a will reads [`crate::glance::AWAY`] whichever word the
    /// bridge that registered it wrote.
    #[must_use]
    pub fn reads(&self) -> &str {
        if crate::glance::is_will(&self.state) {
            crate::glance::AWAY
        } else {
            &self.state
        }
    }
}

/// The distinct subjects one producer (one node) may hold at once. A departed session's
/// presence row and the lanes its node wrote to it go back (the bridge's reclaim, astream's
/// conditional retire); each topic face a session broadcast on (`say`) stays spent for good.
pub const SUBJECT_BUDGET: usize = astream_broker::store::MAX_SUBJECTS_PER_PRODUCER;

/// How much of [`SUBJECT_BUDGET`] spent is a warning: three quarters, while about a thousand
/// subjects are still left.
pub const SUBJECT_BUDGET_WARN: usize = SUBJECT_BUDGET / 4 * 3;

/// Everything `status` found.
#[derive(Debug, Clone)]
pub struct Report {
    pub source: Source,
    /// The command itself, as the argv it runs.
    pub command: Vec<String>,
    /// Whether its program lies in a cargo target directory ([`in_cargo_target_dir`]).
    pub program_in_target_dir: bool,
    /// The program the command runs ([`bridge_program`]), resolved when the report was read:
    /// what each instance's own is compared with.
    pub program: Option<String>,
    /// The config file looked in, if any.
    pub config_path: Option<PathBuf>,
    pub cfg: Config,
    /// What the bridge's presence rows carry: `--presence` on the command, else
    /// `[fabric] presence` in the config file, else `meta`.
    pub presence: crate::presence::Mode,
    /// Whether the bridge publishes receipts: `--receipts`/`--no-receipts` on the command, else
    /// `[fabric] receipts` in the config file, else on.
    pub receipts: bool,
    /// The node id in the command's state dir.
    pub node: Option<String>,
    /// `(path, grant count or error)` per cap file.
    pub caps: Vec<(String, Result<usize, String>)>,
    /// `(path, why)` per cap file wider than the node's ceiling, the command's files judged as
    /// the one ring the bridge attaches ([`crate::enable::wider_than_ceiling`]): `aterm fabric
    /// on` keeps a wide `<root>/node.cap` aside and attaches no other file.
    pub wide_caps: Vec<(String, String)>,
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
    /// Whether the roster read was cut at [`crate::glance::MAX_ROWS`], so some nodes and
    /// sessions are in no section.
    pub roster_truncated: bool,
    /// The distinct subjects this node's producer holds on the bus, of [`SUBJECT_BUDGET`]
    /// ([`subjects_of`]), or why they could not be counted.
    pub subjects: Option<Result<usize, String>>,
    /// Bus session rows that are not live and not local (not shown in text).
    pub exited: usize,
    /// TRAFFIC, oldest first.
    pub traffic: Vec<Traffic>,
    /// Every `ask`/`task` on the bus past its `dl=` with no reply, each a warning: one
    /// retention still holds below the `in` lanes' oldest included (`overdue_work` reads them
    /// told, [`transport::fetch_told_kept`]). Judged only where the bus keeps the verdicts:
    /// an ask whose asker's lane gave up an `answer`, `report` or `ack` above it is not
    /// listed, as status cannot say it had none, and `expired` is `None` where the asker's
    /// `expired` verdicts above it are not all kept.
    pub overdue: Vec<Overdue>,
    /// How many asks past their `dl=` the deadline scan left out of `overdue` because the bus no
    /// longer keeps every verdict above them on their asker's lane: one warning names the count
    /// (audit finding BUS2-3).
    pub overdue_unjudged: usize,
    /// Structured WARNINGS paired with their [`WarningKind`] for [`crate::enable::fix_for`].
    pub warning_items: Vec<Warning>,
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
    /// The state alone: what `doctor` prints above its own `fix:` line, so the remedy is said
    /// once.
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

    /// The one line `status` prints: the state, and for an off fabric the verb that turns it
    /// on.
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

    let (listed, discovery_error) = match aterm_ctl::local_instances() {
        Ok(list) => (list, None),
        Err(e) => (Vec::new(), Some(e.reason)),
    };
    let instances: Vec<InstanceView> = listed
        .iter()
        .filter_map(|(pid, sock)| read_instance(*pid, sock, true))
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
    let rings: Vec<(&String, std::io::Result<Vec<crate::bridge::Cap>>)> = cfg
        .cap_files
        .iter()
        .map(|p| (p, read_cap_file(p)))
        .collect();
    let attached: Vec<(&str, &[crate::bridge::Cap])> = rings
        .iter()
        .filter_map(|(path, ring)| Some((path.as_str(), ring.as_ref().ok()?.as_slice())))
        .collect();
    let wide_caps = node.as_deref().map_or_else(Vec::new, |node| {
        crate::enable::wider_than_ceiling(&attached, &cfg.fleet, node)
            .into_iter()
            .map(|(path, why)| (path.to_string(), why))
            .collect()
    });
    let caps = rings
        .into_iter()
        .map(|(p, ring)| (p.clone(), ring.map(|c| c.len()).map_err(|e| e.to_string())))
        .collect();

    let (broker, conn) = probe_broker(&cfg);
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
        wide_caps,
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
        overdue_unjudged: 0,
        warning_items: Vec::new(),
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
            Err(e) => bus_errors.push(Warning::new(
                WarningKind::BusUnreadable,
                format!("the presence roster could not be read: {e}"),
            )),
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
            now_ms,
        ) {
            Ok(t) => report.traffic = t,
            Err(e) => bus_errors.push(Warning::new(
                WarningKind::BusUnreadable,
                format!("the traffic could not be read: {e}"),
            )),
        }
        halts = if report.broker.mesh {
            mesh_halts_here(&report.cfg.state_dir)
        } else {
            fleet_halts(&mut conn, &report.cfg.fleet)
        };
        match overdue_work(&mut conn, &report.cfg.fleet, faces.as_deref(), head, now_ms) {
            Ok(d) => {
                report.overdue = d.overdue;
                report.overdue_unjudged = d.unjudged;
            }
            Err(e) => bus_errors.push(Warning::new(
                WarningKind::BusUnreadable,
                format!("the deadlines could not be read: {e}"),
            )),
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
    report.warning_items = warning_items(&report, &ages, &halts);
    report.warning_items.extend(bus_errors);
    report.warnings = report
        .warning_items
        .iter()
        .map(|w| w.text.clone())
        .collect();
    Ok(report)
}

fn in_cargo_target_dir(program: &Path) -> bool {
    program.is_absolute()
        && program
            .ancestors()
            .skip(1)
            .any(|d| d.join("CACHEDIR.TAG").is_file())
}

/// Count the distinct subjects `node`'s producer holds in the broker's last-value index across
/// `/f/<fleet>/pub/<node>/>` and `/f/<fleet>/in/*/*/<node>/*`, up to [`SUBJECT_BUDGET`].
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

/// With the mesh on, every halt this Mac's bridge applied and holds its sessions under (§11 C4),
/// as `(who, reason)`: the person, the Mac whose bridge said it, and its id, which `aterm fabric
/// halt off <id>` lifts on every Mac.
fn mesh_halts_here(state_dir: &str) -> Vec<(String, String)> {
    if state_dir.is_empty() {
        return Vec::new();
    }
    let Some(text) = crate::state::StateDir::peek(state_dir).mesh_halts() else {
        return Vec::new();
    };
    let Ok((halts, _)) = crate::halt::Halts::parse(&text) else {
        return Vec::new();
    };
    halts
        .standing()
        .iter()
        .map(|(id, halt)| {
            (
                format!(
                    "{} on {} (halt {id}; `aterm fabric halt off {id}` lifts it)",
                    safe(&halt.by, 64),
                    safe(&halt.origin, 64)
                ),
                crate::pct::decode(&halt.reason),
            )
        })
        .collect()
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
                let recent = crate::parse_uint::<u64>(row.field("t"))
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

/// The words each warning of records the broker's retention gave up says what happened with.
pub const RETENTION_GAVE_UP: &str = "the broker's retention gave up";

/// The words of the warning that counts the asks past their deadline the deadline scan cannot
/// judge ([`Report::overdue_unjudged`]).
pub const OVERDUE_UNJUDGED: &str = "past their deadline cannot be judged";

/// Closed warning grammar covering every finding emitted by [`gather`] and `doctor` so
/// [`crate::enable::fix_for`] dispatches exhaustively without prose substring matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WarningKind {
    /// No `[fabric] command` in `aterm.toml`; report fell back to a running instance's bridge.
    NoFabricCommand,
    /// State directory has no `node` identity file.
    NoNodeId,
    /// A configured cap file could not be read or parsed.
    CapUnreadable {
        /// Whether the refusal is an owner-only mode or symlink violation on a secret cap file.
        owner_or_link_refused: bool,
    },
    /// A configured cap file grants wider authority than this node's ceiling.
    CapWiderRing,
    /// The broker speaks a different `astream` wire protocol version than this `aterm`.
    BrokerProtocolSkew,
    /// The broker endpoint did not answer the probe.
    BrokerUnreachable {
        /// Untruncated last non-empty stderr line from `<root>/broker.err`, if present.
        last_word: Option<String>,
    },
    /// The broker refused this node's capability ring as expired (`exp=`).
    BrokerCapLapsed,
    /// The broker answered, or connected, and a bus read failed.
    BusUnreadable,
    /// The serving broker process maps a replaced executable image.
    BrokerReplacedBuild,
    /// The broker accepted an unverified probe capability (running unguarded).
    BrokerUnguarded,
    /// The broker reports a poisoned log.
    BrokerLogPoisoned,
    /// The broker reports failed log flushes without poisoning the log.
    BrokerFlushFailed,
    /// Local `aterm` control-socket discovery failed.
    InstanceDiscoveryFailed,
    /// A local `aterm` instance did not answer on its control socket.
    InstanceUnreachable,
    /// A local `aterm` instance is unsupervised and has no bridge.
    InstanceNoBridge,
    /// A local `aterm` instance's supervised bridge is down (`fabric=disconnected`).
    BridgeDown,
    /// A local `aterm` instance's bridge stands by behind another bridge holding its state dir.
    BridgeStateLocked,
    /// A local `aterm` instance's bridge has attached and has not yet reported its broker link.
    BridgeStarting,
    /// A local `aterm` instance's bridge is stalled on its broker link (`fabric=stalled`).
    BridgeLinkDown {
        /// The `reason=` token reported by `fabric status`.
        reason: String,
        /// Untruncated last non-empty stderr line from `<root>/broker.err`, when applicable.
        last_word: Option<String>,
    },
    /// A local `aterm` instance's bridge has stopped answering work (`fabric=stale`).
    BridgeStale,
    /// An instance reports `fabric=connected` while this report's probe cannot reach the broker.
    ConnectedProbeMismatch,
    /// An instance's bridge build differs from the instance's own build.
    BridgeBuildMismatch,
    /// An instance failed to parse one or more lines from its bridge (`lane_faults > 0`).
    BridgeLaneFaults,
    /// Broker retention gave up unread node mail or a session's own lane/topic records.
    RetentionGaveUp,
    /// An instance's supervised bridge was armed with a different command than the config.
    BridgeDifferentCommand,
    /// An instance reports `fabric=connected` while the bus presence row marks its node away.
    ConnectedPresenceMismatch,
    /// Querying a listed session over its instance's control socket failed.
    SessionQueryFailed {
        /// Instance PID hosting the session.
        pid: u32,
        /// Control socket path when `--pid <pid>` does not reach the instance.
        sock: Option<String>,
    },
    /// A session's bounded inbox ring evicted unhandled messages (`dropped > 0`).
    SessionInboxEvicted,
    /// A session is held (`hold=1`).
    SessionHeld {
        /// Session identifier without leading `@`.
        sid: String,
        /// Instance PID hosting the session.
        pid: u32,
        /// Control socket path when `--pid <pid>` does not reach the instance.
        sock: Option<String>,
        /// Comma-separated `origin=` / `hold_origin=` slots from the timeline, if still present.
        origin: Option<String>,
        /// Whether the hold reason is `fabric-lost`.
        fabric_lost: bool,
    },
    /// A session has unhandled `task` or `ask` work older than `STALE_WORK_MS`.
    SessionUnhandledStaleWork,
    /// A session has queued outbound posts with no working bridge.
    SessionQueuedPostsNoBridge,
    /// An `ask` or `task` on the bus passed its `dl=` deadline without a reply.
    OverdueWork {
        /// Whether the recipient is a person (`@<person>@p`).
        to_person: bool,
    },
    /// One or more held asks past their deadline lack kept reply-lane history to judge.
    OverdueUnjudged,
    /// The bus advertises a `live` session row on a local node that no local instance hosts.
    GhostSessionRow,
    /// A fleet halt stands on the bus or in local kept state.
    FleetHaltStanding,
    /// The presence roster read hit `glance::MAX_ROWS` and truncated remaining rows.
    RosterTruncated,
    /// This node holds at least `SUBJECT_BUDGET_WARN` subjects on the broker.
    SubjectBudgetHigh,
    /// `doctor` found no `rendezvous.toml` file.
    RendezvousMissing,
    /// `doctor` found an unreadable or malformed `rendezvous.toml` file.
    RendezvousUnreadable,
}

/// One rendered warning line paired with its structured [`WarningKind`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// Structured warning classification for [`crate::enable::fix_for`].
    pub kind: WarningKind,
    /// Human-readable warning sentence printed in `WARNINGS` and `doctor`.
    pub text: String,
}

impl Warning {
    /// Construct a [`Warning`] pairing `kind` with its rendered `text`.
    #[must_use]
    pub fn new(kind: WarningKind, text: String) -> Self {
        Self { kind, text }
    }
}

/// How many ranges one warning names before it counts the rest.
const GAP_RANGES_SHOWN: usize = 4;

/// `told` as the ranges of records they name, lowest first: ranges that overlap are one (a
/// later gap notice on one subscription can overlap an earlier one, astream's `Gap::and`; a new
/// subscription's notice can cover the last), carrying the first count of a range told that is
/// the whole merged range and none where every count is of a part of it; ranges that only touch
/// stay apart, and an empty range is no loss.
fn merged_ranges(told: &[GivenUpRange]) -> Vec<GivenUpRange> {
    let mut spans: Vec<GivenUpRange> = Vec::new();
    let mut sorted: Vec<&GivenUpRange> = told.iter().filter(|g| g.from < g.to).collect();
    sorted.sort_by_key(|g| (g.from, g.to));
    for g in sorted {
        match spans.last_mut() {
            Some(span) if g.from < span.to => span.to = span.to.max(g.to),
            _ => spans.push(GivenUpRange {
                from: g.from,
                to: g.to,
                records: None,
            }),
        }
    }
    for span in &mut spans {
        span.records = told
            .iter()
            .filter(|g| (g.from, g.to) == (span.from, span.to))
            .find_map(|g| g.records);
    }
    spans
}

/// The warning for this node's unread mail that one instance's bridges were told the broker's
/// retention gave up before they read it (`link gap` with no `sid=`, the inbox group's gap
/// notice), or `None`. It names the ranges the instance's counter names (`fabric status`'s
/// `mail_gaps=` and newest `mail_gap=`) and those its sessions still name (`inbox`'s
/// `given_up=`, which a seamless update carries and the counter does not), ranges that overlap
/// as one ([`merged_ranges`]), newest first, so a range told to every session is one warning,
/// not one per session.
fn node_mail_given_up(inst: &InstanceView) -> Option<String> {
    let told = inst.mail_gaps.filter(|k| *k > 0);
    let named: Vec<GivenUpRange> = inst
        .mail_gap
        .iter()
        .chain(inst.sessions.iter().filter_map(|s| s.given_up.as_ref()))
        .copied()
        .collect();
    let ranges = merged_ranges(&named);
    if told.is_none() && ranges.is_empty() {
        return None;
    }
    let since = told.map_or_else(String::new, |k| {
        format!(" ({k} range(s) since the instance started)")
    });
    let named = if ranges.is_empty() {
        ", its offsets on a bus log since replaced".to_string()
    } else {
        let shown: Vec<String> = ranges
            .iter()
            .rev()
            .take(GAP_RANGES_SHOWN)
            .map(GivenUpRange::said)
            .collect();
        let more = ranges.len().saturating_sub(GAP_RANGES_SHOWN);
        let more = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        format!(": {}{more}", shown.join(", "))
    };
    Some(format!(
        "instance {}: {RETENTION_GAVE_UP} this node's unread mail before its bridge read \
         it{since}{named}; the sessions it served then were told (`inbox` given_up=, a `mail-gap` \
         mark in each log)",
        inst.pid
    ))
}

/// What the broker's retention gave up of one session's own before it handled it, or `None`:
/// its mail lane's newest range (`inbox`'s `lane_given_up=`, the bridge's own read of the lane
/// met the floor) and each topic's (`topic ls`'s `given_up=`, the bridge's replay of the topic
/// met it), each with the field that names it.
fn session_given_up(s: &LocalSession) -> Option<String> {
    let mut parts: Vec<String> = s
        .lane_given_up
        .iter()
        .map(|g| format!("its mail lane {} (`inbox` lane_given_up=)", g.said()))
        .collect();
    let topics: Vec<String> = s
        .topic_given_up
        .iter()
        .map(|(topic, g)| format!("topic {} {}", safe(topic, 64), g.said()))
        .collect();
    if !topics.is_empty() {
        parts.push(format!("{} (`topic ls` given_up=)", topics.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

pub(crate) fn warning_items(
    r: &Report,
    ages: &BTreeMap<(String, u64), u64>,
    halts: &[(String, String)],
) -> Vec<Warning> {
    let mut w = Vec::new();
    let mut push = |kind: WarningKind, text: String| w.push(Warning::new(kind, text));
    let broker = safe(&r.cfg.broker, 256);

    if let Source::Instance(pid) = r.source {
        let path = r
            .config_path
            .as_ref()
            .map_or_else(|| "aterm.toml".to_string(), |p| p.display().to_string());
        push(
            WarningKind::NoFabricCommand,
            format!(
                "no [fabric] command in {path}: this report read instance {pid}'s running bridge \
                 instead, and the next aterm launch starts none"
            ),
        );
    }
    if r.node.is_none() {
        push(
            WarningKind::NoNodeId,
            format!(
                "no node id in {}/node: the bridge has no identity to answer mail as",
                safe(&r.cfg.state_dir, 256)
            ),
        );
    }
    for (path, caps) in &r.caps {
        if let Err(e) = caps {
            push(
                WarningKind::CapUnreadable {
                    owner_or_link_refused: e.contains("a secret file")
                        || e.contains("read through no link"),
                },
                format!(
                    "cap file {}: {}",
                    safe(path, 256),
                    safe(e, CAP_REFUSAL_WORDS)
                ),
            );
        }
    }
    for (path, why) in &r.wide_caps {
        push(
            WarningKind::CapWiderRing,
            format!("cap file {}: {}", safe(path, 256), safe(why, 512)),
        );
    }

    if let Some(skew) = r.broker.protocol {
        push(
            WarningKind::BrokerProtocolSkew,
            format!(
                "the broker at {broker} speaks astream protocol {} and this aterm speaks {}: \
                 neither can read what the other sends, so no record lands and none is delivered",
                skew.broker, skew.client
            ),
        );
    } else if !r.broker.reachable {
        let last = r.broker.last_word.as_deref().map_or_else(String::new, |l| {
            format!("; its supervisor's broker.err, {}", safe(l, 256))
        });
        push(
            WarningKind::BrokerUnreachable {
                last_word: r.broker.last_word.clone(),
            },
            format!(
                "the broker at {broker} does not answer ({}): no record lands and none is \
                 delivered{last}",
                safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
            ),
        );
    } else if r.broker.cap_expired {
        push(
            WarningKind::BrokerCapLapsed,
            format!(
                "the broker at {broker} {CAP_LAPSED} (its grants' `exp=` has passed): no record of \
                 this node lands and none is delivered to it — {}",
                safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
            ),
        );
    } else if !r.broker.read_ok() && !r.broker.cap_unread {
        // A cap the report could not read is its own warning above (`r.caps`), its fix what that
        // file needs: nothing is known of the bus, and a second warning would say the bus failed.
        push(
            WarningKind::BusUnreadable,
            format!(
                "the broker at {broker} answers but the bus could not be read: {}",
                safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
            ),
        );
    }
    if let (Some(pid), Some(exe)) = (r.broker.pid, &r.broker.replaced) {
        let who = match r.broker.host {
            Some(Host::NodeService) => "the node service hosting bus.log",
            Some(Host::LinkBroker) | None => "the broker",
        };
        push(
            WarningKind::BrokerReplacedBuild,
            format!(
                "{who} (pid {pid}) runs a replaced build: {} is another file than the one it \
                 started from, so the broker-side fixes of the build on disk are not running",
                safe(exe, 256)
            ),
        );
    }
    if r.broker.guarded == Some(false) {
        push(
            WarningKind::BrokerUnguarded,
            format!(
                "the broker at {broker} checks no capability (it accepted one it cannot verify): \
                 any process that reaches it can publish as any node or human, and set or lift any \
                 fleet halt"
            ),
        );
    }
    if let Some(why) = r.broker.poisoned() {
        let reopens = match r.broker.keeper() {
            crate::enable::Keeper::Job => {
                "a broker of this build exits within seconds for its supervisor to reopen it"
            }
            crate::enable::Keeper::Node => {
                "the node service reopens it at its next pass, within seconds"
            }
        };
        push(
            WarningKind::BrokerLogPoisoned,
            format!(
                "the broker at {broker} {POISONED_WARNING} ({}): it refuses every write until the \
                 log is reopened, so no record lands; {reopens}",
                safe(why, 256)
            ),
        );
    } else if let Some(failed) = r
        .broker
        .health
        .as_ref()
        .map(|h| h.flushes.failed)
        .filter(|n| *n > 0)
    {
        push(
            WarningKind::BrokerFlushFailed,
            format!(
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
            ),
        );
    }

    if let Some(e) = &r.discovery_error {
        push(
            WarningKind::InstanceDiscoveryFailed,
            format!(
                "cannot list this machine's aterm instances: {}",
                safe(e, 512)
            ),
        );
    }
    let config_flags = serve_flags(&r.command);
    for inst in &r.instances {
        let pid = inst.pid;
        if let Some(e) = &inst.error {
            push(
                WarningKind::InstanceUnreachable,
                format!(
                    "instance {pid} did not answer ({}): its bridge and sessions are not in \
                     this report",
                    safe(e, 256)
                ),
            );
            continue;
        }
        let own = inst.command.as_deref().and_then(|c| bridge_config(c).ok());
        let same_broker = own.as_ref().is_none_or(|c| c.broker == r.cfg.broker);
        match (inst.fabric.as_deref(), inst.supervised) {
            (_, Some(false)) => push(
                WarningKind::InstanceNoBridge,
                format!(
                    "instance {pid} has no bridge (fabric={}, not supervised): its sessions \
                     receive no mail and their posts only queue",
                    safe(inst.fabric.as_deref().unwrap_or("-"), 32)
                ),
            ),
            (Some("disconnected"), _) => push(
                WarningKind::BridgeDown,
                format!(
                    "instance {pid}'s bridge is down (fabric=disconnected): its sessions get no \
                     mail and their posts queue; aterm relaunches the bridge"
                ),
            ),
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
                push(
                    WarningKind::BridgeStateLocked,
                    format!(
                        "instance {pid}'s bridge stands by (fabric=stalled reason=state-locked): \
                         {holder} holds its state dir {}, and one state dir is one node — its \
                         sessions receive no mail and no fleet halt, and their posts queue until \
                         that bridge exits",
                        safe(inst.state_dir.as_deref().unwrap_or("-"), 256)
                    ),
                );
            }
            (Some("stalled"), _) if inst.reason.as_deref() == Some("starting") => {
                push(
                    WarningKind::BridgeStarting,
                    format!(
                        "instance {pid}'s bridge is attached and has not reported its broker link \
                         (fabric=stalled reason=starting); if this stays, check the broker at \
                         {broker}"
                    ),
                );
            }
            (Some("stalled"), _) => {
                let reason = inst.reason.as_deref().unwrap_or("-");
                let (last_word, last) = match (reason, r.broker.last_word.as_deref()) {
                    ("no-socket" | "refused", Some(l)) if same_broker && !r.broker.reachable => (
                        Some(l.to_string()),
                        format!("; its supervisor's broker.err, {}", safe(l, 256)),
                    ),
                    _ => (None, String::new()),
                };
                push(
                    WarningKind::BridgeLinkDown {
                        reason: reason.to_string(),
                        last_word,
                    },
                    format!(
                        "instance {pid}'s bridge is attached but its broker link is down \
                         (fabric=stalled reason={}, last ack {}): its posts queue and no mail \
                         arrives from the broker at {broker}{last}",
                        safe(reason, 32),
                        inst.link_age_ms
                            .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
                    ),
                );
            }
            (Some("stale"), _) => push(
                WarningKind::BridgeStale,
                format!(
                    "instance {pid}'s bridge has not answered for the work it was given \
                     (fabric=stale, last ack {}): its posts queue and `post --wait` from its \
                     sessions fails fast",
                    inst.link_age_ms
                        .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
                ),
            ),
            (Some("connected"), _) if !r.broker.reachable && same_broker => push(
                WarningKind::ConnectedProbeMismatch,
                format!(
                    "instance {pid} says fabric=connected (its bridge's last ack was {}), but the \
                     broker at {broker} does not answer this report's own probe: either the broker \
                     died since that ack and the bridge has not exchanged a record with it yet, \
                     or the two are not looking at the same socket",
                    inst.link_age_ms
                        .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
                ),
            ),
            _ => {}
        }
        if let (Some(theirs), Some(ours)) = (&inst.bridge_build, &inst.build) {
            if theirs != ours {
                push(
                    WarningKind::BridgeBuildMismatch,
                    format!(
                        "instance {pid}'s bridge is another build than the instance: bridge {}, \
                         instance {}",
                        safe(theirs, 64),
                        safe(ours, 64)
                    ),
                );
            }
        }
        if let Some(n) = inst.lane_faults.filter(|n| *n > 0) {
            push(
                WarningKind::BridgeLaneFaults,
                format!(
                    "instance {pid} could not parse {n} of its bridge's lines (lane_faults={n}): \
                     the mail they carried is held back on the bus, not lost"
                ),
            );
        }
        if let Some(line) = node_mail_given_up(inst) {
            push(WarningKind::RetentionGaveUp, line);
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
            push(
                WarningKind::BridgeDifferentCommand,
                format!(
                    "instance {pid}'s bridge was armed with a different command than {}: \
                     {differs} — an instance records its command at launch or `fabric attach`, \
                     so this report and that bridge are not describing the same fabric",
                    r.source.describe()
                ),
            );
        }
        if inst.fabric.as_deref() == Some("connected") {
            if let Some(row) = inst
                .node
                .as_deref()
                .and_then(|n| r.nodes.iter().find(|row| row.node == n))
            {
                if row.state != "live" || row.fabric != "connected" {
                    push(
                        WarningKind::ConnectedPresenceMismatch,
                        format!(
                            "instance {pid} says fabric=connected, but the bus's own presence \
                             for its node {} says state={} fabric={}: every peer reading the \
                             fleet's roster sees this node away",
                            safe(row.node.as_str(), 64),
                            safe(row.reads(), 16),
                            safe(&row.fabric, 16)
                        ),
                    );
                }
            }
        }
        let bridge_ok = inst.fabric.as_deref() == Some("connected") && r.broker.reachable;
        let reach_sock = (!inst.pid_reaches).then(|| inst.sock.clone());
        for s in &inst.sessions {
            let at = format!("@{} (instance {pid})", s.sid);
            if let Some(e) = &s.error {
                push(
                    WarningKind::SessionQueryFailed {
                        pid,
                        sock: reach_sock.clone(),
                    },
                    format!("{at}: {}", safe(e, 256)),
                );
                continue;
            }
            if let Some(n) = s.dropped.filter(|n| *n > 0) {
                push(
                    WarningKind::SessionInboxEvicted,
                    format!(
                        "{at} lost {n} message(s): its inbox ring evicted rows nobody handled \
                         (dropped={n})"
                    ),
                );
            }
            if let Some(lost) = session_given_up(s) {
                push(
                    WarningKind::RetentionGaveUp,
                    format!(
                        "{at}: {RETENTION_GAVE_UP} records of its own before it handled them: \
                         {lost}"
                    ),
                );
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
                push(
                    WarningKind::SessionHeld {
                        sid: s.sid.clone(),
                        pid,
                        sock: reach_sock.clone(),
                        origin: s.hold_why.as_ref().map(|(_, o)| o.clone()),
                        fabric_lost: s
                            .hold_why
                            .as_ref()
                            .is_some_and(|(reason, _)| reason == "fabric-lost"),
                    },
                    format!("{at} is HELD ({why}): every key and turn verb answers ERR halted"),
                );
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
                push(
                    WarningKind::SessionUnhandledStaleWork,
                    format!(
                        "{at} has {} unhandled task/ask older than 10 min — the oldest, {} from \
                         {}, is {} old",
                        stale.len(),
                        safe(&oldest.kind, 16),
                        safe(&oldest.from, 128),
                        age(*old)
                    ),
                );
            }
            if let Some(n) = s.posts.filter(|n| *n > 0) {
                if !bridge_ok {
                    push(
                        WarningKind::SessionQueuedPostsNoBridge,
                        format!(
                            "{at} has {n} queued post(s) and no working bridge to publish them"
                        ),
                    );
                }
            }
        }
    }
    for o in &r.overdue {
        push(
            WarningKind::OverdueWork {
                to_person: o.to.ends_with("@p"),
            },
            format!(
                "{} off={} from {} to @{} passed its deadline {} ago (dl={} ms) with no answer, \
                 report or ack — {}",
                safe(&o.kind, 16),
                o.off,
                safe(&o.from, 128),
                safe(&o.to, 128),
                age(o.late_ms),
                o.dl,
                match o.expired {
                    Some(true) => "the asker's bridge recorded it expired",
                    Some(false) => "not yet recorded expired by the asker's bridge",
                    None => "whether the asker's bridge recorded it expired is not kept on the bus",
                }
            ),
        );
    }
    if r.overdue_unjudged > 0 {
        push(
            WarningKind::OverdueUnjudged,
            format!(
                "{} held ask(s) {OVERDUE_UNJUDGED}: the bus no longer keeps (or this cap does not \
                 read) every answer, report and ack above them on their asker's lane, so status \
                 cannot say whether they had one",
                r.overdue_unjudged
            ),
        );
    }
    let answered = every_instance_answered(r);
    let local_nodes = local_nodes(&r.instances, r.node.as_deref());
    if answered {
        for s in ghost_rows(&r.sessions, &local_nodes) {
            push(
                WarningKind::GhostSessionRow,
                format!(
                    "the bus advertises @{} {} on {} — this machine's node — but no \
                     aterm instance here hosts it: mail to it is undeliverable",
                    safe(&s.sid, 64),
                    safe(&s.bus, 16),
                    safe(&s.node, 64)
                ),
            );
        }
    }
    for (who, reason) in halts {
        push(
            WarningKind::FleetHaltStanding,
            format!(
                "{who} has a fleet halt standing (reason={}): every session on the fleet \
                 is held",
                safe(reason, 128)
            ),
        );
    }
    if r.roster_truncated {
        push(
            WarningKind::RosterTruncated,
            format!(
                "the presence roster has more than {} rows and the read stopped there \
                 (rows={}+ truncated): NODES, SESSIONS and every warning drawn from them leave \
                 out what lies past it",
                crate::glance::MAX_ROWS,
                crate::glance::MAX_ROWS
            ),
        );
    }
    if let (Some(Ok(used)), Some(node)) = (&r.subjects, r.node.as_deref()) {
        if *used >= SUBJECT_BUDGET_WARN {
            push(
                WarningKind::SubjectBudgetHigh,
                format!(
                    "node {} holds {used} of the {SUBJECT_BUDGET} distinct subjects one node may \
                     hold at once: a departed session's presence row and the lanes this node wrote \
                     to it go back five minutes after it leaves (its `ev` face once the session-log \
                     mirror landed its tail), but each topic a session broadcast on keeps one \
                     subject for good, so what stays is held by live sessions, every topic face a \
                     session of this node ever broadcast on, mail still owed and the node's own \
                     faces; past the bound a new session gets no presence row and its posts are \
                     refused",
                    safe(node, 64)
                ),
            );
        }
    }
    let kept = if r.cfg.state_dir.is_empty() {
        BTreeMap::new()
    } else {
        crate::state::StateDir::peek(&r.cfg.state_dir).standing_halts(&r.cfg.fleet)
    };
    for (who, halt) in kept.into_iter().filter(|(_, h)| h.kept) {
        push(
            WarningKind::FleetHaltStanding,
            format!(
                "{}'s fleet halt (reason={}) stands on this node with no row on the bus: it was \
                 read on a bus log since replaced, and only {0}'s own `state=off` on this log \
                 lifts it",
                safe(&who, 64),
                safe(&crate::pct::decode(&halt.reason), 128)
            ),
        );
    }
    w
}

#[cfg(test)]
fn warnings(
    r: &Report,
    ages: &BTreeMap<(String, u64), u64>,
    halts: &[(String, String)],
) -> Vec<String> {
    warning_items(r, ages, halts)
        .into_iter()
        .map(|w| w.text)
        .collect()
}

/// The local clock's offset from UTC in seconds at `ms` since the epoch (`tm_gmtoff`); `None`
/// where the C library cannot say, and the report then prints UTC and says so.
pub(crate) fn local_offset_at_ms(ms: u64) -> Option<i64> {
    aterm_uds::zone::local_offset_at(i64::try_from(ms / 1000).unwrap_or(i64::MAX / 2))
}

/// A record's time at the clock's offset: `HH:MM:SS` within 20 hours of `now`, `MM-DDTHH:MM`
/// beyond, `-` when the record carried none.
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

/// [`clock`] at `t_ms`'s own local offset (`local_at`, Unix seconds to UTC offset seconds),
/// naming that offset (`UTC±HH:MM`, or `UTC` when `local_at` returns `None`) whenever it
/// differs from `header_offset`.
#[must_use]
pub fn clock_at(
    t_ms: u64,
    now_ms: u64,
    header_offset: Option<i64>,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> String {
    if t_ms == 0 {
        return "-".to_string();
    }
    let secs = i64::try_from(t_ms / 1000).unwrap_or(i64::MAX / 2);
    let row_offset = local_at(secs);
    let base = clock(t_ms, now_ms, row_offset.unwrap_or(0));
    if row_offset == header_offset {
        base
    } else {
        format!("{base} {}", zone_word(row_offset))
    }
}

pub(crate) fn zone_word(offset: Option<i64>) -> String {
    match offset {
        None => "UTC".to_string(),
        Some(o) => {
            let sign = if o < 0 { '-' } else { '+' };
            let o = o.abs();
            format!("UTC{sign}{:02}:{:02}", o / 3600, (o % 3600) / 60)
        }
    }
}

/// How the times in this report are zoned, for the header.
pub(crate) fn zone_label(offset: Option<i64>) -> String {
    format!("times {}", zone_word(offset))
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

#[cfg(test)]
fn traffic_cells(t: &Traffic, now_ms: u64, offset: i64) -> Vec<String> {
    traffic_cells_at(t, now_ms, Some(offset), &|_| Some(offset))
}

fn traffic_cells_at(
    t: &Traffic,
    now_ms: u64,
    header_offset: Option<i64>,
    local_at: &dyn Fn(i64) -> Option<i64>,
) -> Vec<String> {
    vec![
        format!("@{}", t.off),
        clock_at(t.t_ms, now_ms, header_offset, local_at),
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

/// Classify which node a NODES row names (`this`, `local`, or `remote`).
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
    let (mut conn, _closer) = transport::connect(&cfg.broker, io_timeout())?;
    transport::attach_cap_files(&mut conn, &cfg.cap_files)?;
    Ok(conn)
}

/// Publish `state=exited` for each `(sid, node)` by rewriting its presence row (`inc=`,
/// `epoch=` and `gen=` kept) under [`crate::state::StateDir::lock_bridge`], with sequences
/// reserved from the current incarnation.
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
        // Under the node's producer, whose bucket the broker's rate limit keeps: a refusal is
        // waited out and sent again at the same sequence ([`crate::rate_hold::waiting_out`]).
        let (off, deduped) = crate::rate_hold::waiting_out(
            || conn.publish(producer_id, seq, &subject, rewritten.as_bytes()),
            Duration::from_secs(3),
            std::thread::sleep,
        )?;
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
    /// The `state=` of each named session's presence row under the node asked about; a sid
    /// with no row is absent.
    pub states: BTreeMap<String, String>,
    /// Why the bridge refused each named session it refused.
    pub refused: BTreeMap<String, String>,
    /// Whether the node's `ev` face was read through to its head, so an empty `refused` means
    /// the bridge refused nothing.
    pub refusals_read: bool,
}

/// What the bus says of each named session of `node` since the doctor asked that node's bridge
/// to retire it, read with the cap files' grants: the `state=` of its presence row, and why the
/// bridge refused it, for every `presence-retire-refused` on `node`'s `ev` face at or after
/// offset `since`.
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

/// A bridge's `presence-retire-refused sid=<sid> reason=<r> [pid=<p>]` event as `(sid, why)`,
/// the why in words; `None` for any other event.
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

/// One presence body with `state=` set to `exited` and `t=` refreshed, every other token kept.
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

pub(crate) fn retirement_candidates(r: &Report) -> Result<Vec<(String, String)>, Vec<String>> {
    let silent = unanswered(r);
    if !silent.is_empty() {
        return Err(silent);
    }
    let local = local_nodes(&r.instances, r.node.as_deref());
    Ok(ghost_rows(&r.sessions, &local)
        .into_iter()
        .map(|row| (row.sid.clone(), row.node.clone()))
        .collect())
}

/// The ghosts: presence rows this machine's nodes advertise that no local instance hosts,
/// `away` ones included, since a local node whose will fired with no instance here (a crash, a
/// SIGKILL) leaves exactly these behind.
pub(crate) fn ghost_rows<'r>(
    sessions: &'r [SessionRow],
    local: &BTreeSet<&str>,
) -> Vec<&'r SessionRow> {
    sessions
        .iter()
        .filter(|s| s.pid.is_none() && advertised(&s.bus) && local.contains(s.node.as_str()))
        .collect()
}

/// Every session any local instance hosts, by pid, from each instance's `sessions`.
pub(crate) fn node_hosted() -> Result<BTreeMap<String, u32>, String> {
    let listed = aterm_ctl::local_instances().map_err(|e| e.reason)?;
    let mut hosted = BTreeMap::new();
    for (pid, sock) in listed {
        let Some(mut ctl) =
            open_instance(pid, &sock, io_timeout()).map_err(|e| format!("instance {pid}: {e}"))?
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

/// Why a presence row may not be retired on an operator's request: the request came through
/// one instance, but the ghost list is the node's.
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

/// Whether a SESSIONS row's `bus` is one the roster still advertises: `live`, or `away` (a node
/// whose will fired).
#[must_use]
pub fn advertised(bus: &str) -> bool {
    bus == "live" || bus == crate::glance::AWAY
}

/// How many sessions the roster advertises on `node` ([`advertised`]).
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
    render_text_local(r, &aterm_uds::zone::local_offset_at)
}

/// [`render_text`], asking `local_at` (Unix seconds to UTC offset seconds) at the report's
/// timestamp and at each traffic record's timestamp.
#[must_use]
pub fn render_text_local(r: &Report, local_at: &dyn Fn(i64) -> Option<i64>) -> String {
    let offset = local_at(i64::try_from(r.now_ms / 1000).unwrap_or(i64::MAX / 2));
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
            .unwrap_or_else(|| format!("- (no node id in {}/node)", safe(&r.cfg.state_dir, 256))),
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
                Err(e) => format!(
                    "{} (UNREADABLE: {})",
                    safe(path, 256),
                    safe(e, CAP_REFUSAL_WORDS)
                ),
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
    if let Some(row) = host_row(b) {
        kvs.push(("host", row));
    }
    kvs.push((
        "reachable",
        if b.read_ok() {
            format!("yes ({} ms)", b.rtt_ms.unwrap_or(0))
        } else if b.reachable {
            format!(
                "yes, but the bus cannot be read: {}",
                safe(b.error.as_deref().unwrap_or("-"), b.error_words())
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
                (None, _) => b.pid_unknown.as_deref().map_or_else(
                    || match b.keeper() {
                        crate::enable::Keeper::Job => {
                            "- (no `link broker` process serves this socket)".to_string()
                        }
                        crate::enable::Keeper::Node => format!(
                            "- (no process serves this socket: the node service ({}) hosts \
                             bus.log here, and `aterm node status` says whether it runs)",
                            crate::enable::NODE_JOB
                        ),
                    },
                    |why| format!("- (unknown: {})", safe(why, 256)),
                ),
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
                     `say` face and lane it holds; a departed session's presence row and the \
                     lanes this node wrote to it go back five minutes after it leaves, its `ev` \
                     face once the session-log mirror landed its tail, and its `say` faces never"
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
    if let Some(word) = b.at_rest() {
        kvs.push(("at-rest", word.to_string()));
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
            match i.bridge_pids.as_deref() {
                None => "?".to_string(),
                Some([]) => "-".to_string(),
                Some(pids) => pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
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
            .map(|t| traffic_cells_at(t, r.now_ms, offset, local_at))
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
                    i.bridge_pids.as_ref().map_or(J::Null, |pids| {
                        J::Arr(pids.iter().map(|p| J::Num(u64::from(*p))).collect())
                    }),
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
                ("mail_gaps", J::opt_n(i.mail_gaps)),
                (
                    "mail_gap",
                    J::opt_s(i.mail_gap.map(|g| g.range()).as_deref()),
                ),
                (
                    "mail_gap_count",
                    J::opt_n(i.mail_gap.and_then(|g| g.records)),
                ),
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
                            .and_then(crate::parse_uint),
                    ),
                ),
                (
                    "hold",
                    J::opt_n(
                        l.and_then(|l| l.hold)
                            .map(u64::from)
                            .or_else(|| s.bus_hold.as_deref().and_then(crate::parse_uint)),
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
                (
                    "given_up",
                    J::opt_s(l.and_then(|l| l.given_up).map(|g| g.range()).as_deref()),
                ),
                (
                    "given_up_count",
                    J::opt_n(l.and_then(|l| l.given_up).and_then(|g| g.records)),
                ),
                (
                    "lane_given_up",
                    J::opt_s(
                        l.and_then(|l| l.lane_given_up)
                            .map(|g| g.range())
                            .as_deref(),
                    ),
                ),
                (
                    "topics_given_up",
                    l.map_or(J::Null, |l| {
                        J::Arr(
                            l.topic_given_up
                                .iter()
                                .map(|(t, g)| {
                                    J::Obj(vec![("topic", J::s(t)), ("given_up", J::s(&g.range()))])
                                })
                                .collect(),
                        )
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
    let traffic = r.traffic.iter().map(|t| J::Obj(traffic_json(t))).collect();
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
                ("host", J::opt_s(b.host.map(Host::word))),
                ("program", J::opt_s(b.program.as_deref())),
                ("build", J::opt_s(b.build().as_deref())),
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
                ("at_rest", J::opt_s(b.at_rest())),
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
                            ("expired", o.expired.map_or(J::Null, J::Bool)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("overdue_unjudged", J::Num(r.overdue_unjudged as u64)),
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
    tail_line_at(t, now_ms, Some(offset), &|_| Some(offset), bodies)
}

/// [`tail_line`] at `t.t_ms`'s own local offset (`local_at`), naming that offset whenever it
/// differs from `header_offset`.
#[must_use]
pub fn tail_line_at(
    t: &Traffic,
    now_ms: u64,
    header_offset: Option<i64>,
    local_at: &dyn Fn(i64) -> Option<i64>,
    bodies: bool,
) -> String {
    let c = traffic_cells_at(t, now_ms, header_offset, local_at);
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

/// A record's metadata as JSON fields, the keys `aterm fabric --json`'s `traffic` carries.
fn traffic_json(t: &Traffic) -> Vec<(&'static str, J)> {
    vec![
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
    ]
}

/// The node and session a record's subject names: `/f/<F>/pub|in/<node>/<sid>/…`, the node an
/// `n-` principal and the sid an `s-` one (a node's own face, a person's lane and the fleet's
/// lanes name no session).
fn subject_session<'a>(fleet: &str, subject: &'a str) -> (Option<&'a str>, Option<&'a str>) {
    let segs: Vec<&str> = subject.split('/').collect();
    let faced = segs.len() >= 6
        && segs[0].is_empty()
        && segs[1] == "f"
        && segs[2] == fleet
        && matches!(segs[3], "pub" | "in");
    let named = |seg: &'a str, class: &str| {
        Some(seg).filter(|s| s.starts_with(class) && crate::subject::is_principal(s))
    };
    match faced.then(|| named(segs[4], "n-")).flatten() {
        Some(node) => (Some(node), named(segs[5], "s-")),
        None => (None, None),
    }
}

/// One `aterm fabric tail --json` line: the keys [`TAIL_KEYS`] names, in its order. `body`,
/// only under `--bodies`, is the text `--bodies` prints in the table.
#[must_use]
pub fn tail_json(fleet: &str, subject: &str, t: &Traffic, bodies: bool) -> String {
    let (node, sid) = subject_session(fleet, subject);
    let mut fields = traffic_json(t);
    fields.extend([
        ("subject", J::s(subject)),
        ("node", J::opt_s(node)),
        ("sid", J::opt_s(sid)),
    ]);
    if bodies {
        fields.push(("body", J::Str(safe(&t.text, crate::render::TEXT_CAP))));
    }
    let mut keyed = Vec::with_capacity(fields.len());
    for key in TAIL_KEYS {
        if let Some(i) = fields.iter().position(|(k, _)| *k == key) {
            keyed.push(fields.swap_remove(i));
        }
    }
    let mut out = String::new();
    J::Obj(keyed).render(&mut out);
    out
}

/// A gap notice as `tail --json` prints it: `{"gap":{"filter","from","to","records"}}`.
fn tail_gap_json(filter: &str, gap: &astream_broker::Gap) -> String {
    let mut out = String::new();
    J::Obj(vec![(
        "gap",
        J::Obj(vec![
            ("filter", J::s(filter)),
            ("from", J::Num(gap.from)),
            ("to", J::Num(gap.to)),
            ("records", J::opt_n(gap.records)),
        ]),
    )])
    .render(&mut out);
    out
}

/// How `tail` prints: the table's lines, or one JSON object per line (`--json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TailFmt {
    bodies: bool,
    json: bool,
    /// The local clock's offset from UTC when `tail` printed its header, seconds.
    offset: Option<i64>,
}

impl TailFmt {
    const fn new(bodies: bool, json: bool) -> Self {
        Self {
            bodies,
            json,
            offset: Some(0),
        }
    }

    fn record(self, fleet: &str, off: u64, subject: &str, raw: &[u8]) -> String {
        self.record_at(
            fleet,
            off,
            subject,
            raw,
            crate::now_ms(),
            &aterm_uds::zone::local_offset_at,
        )
    }

    fn record_at(
        self,
        fleet: &str,
        off: u64,
        subject: &str,
        raw: &[u8],
        now_ms: u64,
        local_at: &dyn Fn(i64) -> Option<i64>,
    ) -> String {
        let t = traffic_of(fleet, off, subject, raw, now_ms);
        if self.json {
            tail_json(fleet, subject, &t, self.bodies)
        } else {
            tail_line_at(&t, now_ms, self.offset, local_at, self.bodies)
        }
    }

    /// A gap notice: under `--json` the object it prints in line, else the line it says on
    /// stderr (and `None`).
    fn gap(self, filter: &str, gap: &astream_broker::Gap) -> Option<String> {
        if self.json {
            Some(tail_gap_json(filter, gap))
        } else {
            tail_gap(filter, gap);
            None
        }
    }
}

fn tail_main(fmt: TailFmt, from: Option<u64>, filter: Option<&str>) -> ExitCode {
    let cfg = match configured() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    match tail(&cfg, fmt, from, filter, &mut io::stdout().lock()) {
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
                    .filter_map(|(pid, sock)| read_instance(*pid, sock, false))
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

/// The broker link a tail reads on ([`TailLink::open`]): one connection with the caps
/// attached, on which the tail subscribes where the caps grant what it reads, and which reads
/// each face on by told fetches where they do not ([`tail_faces`]).
struct TailLink {
    conn: Conn,
    closer: transport::Closer,
    /// What the tail reads: its `--filter`, or the fleet's root.
    whole: String,
    /// Where the caps do not grant the fleet's root: the faces the tail reads apart.
    faces: Option<Vec<String>>,
    /// The log's head when the link opened: where a tail with no `--from` starts.
    head: u64,
    /// The read bound [`TailLink::open`] set on `closer`, as it recorded it (astream's closer
    /// has no getter for it): the connect's ([`io_timeout`]), or, where it reads `faces` on by
    /// fetches, the deadline it opened with.
    read_timeout: Duration,
}

impl TailLink {
    /// The link [`tail`] reads on: where it reads faces apart, every read on it waits as
    /// astream lets a stream wait ([`astream_broker::IDLE_MARK_DEADLINE`]), not for the 3 s of
    /// a one-shot request ([`IO_TIMEOUT`]), as a live tail fetches for as long as it runs.
    fn for_tail(cfg: &Config, want: Option<&str>) -> io::Result<Self> {
        Self::open(cfg, want, astream_broker::IDLE_MARK_DEADLINE)
    }

    /// Dial `cfg`'s broker, attach its caps and read the head of what the tail reads (`want`,
    /// else the fleet's root). Where the broker refuses the fleet's root to those caps, the
    /// tail reads apart each face they grant ([`readable_faces`]), and every read on the link
    /// then waits `deadline` at most.
    fn open(cfg: &Config, want: Option<&str>, deadline: Duration) -> io::Result<Self> {
        let broker = safe(&cfg.broker, 256);
        let (mut conn, closer) = transport::connect(&cfg.broker, io_timeout())
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
        let read_timeout = if faces.is_some() {
            closer.set_read_timeout(Some(deadline))?;
            deadline
        } else {
            io_timeout()
        };
        Ok(Self {
            conn,
            closer,
            whole,
            faces,
            head,
            read_timeout,
        })
    }
}

fn tail(
    cfg: &Config,
    fmt: TailFmt,
    from: Option<u64>,
    want: Option<&str>,
    out: &mut impl Write,
) -> io::Result<()> {
    tail_on(cfg, TailLink::for_tail(cfg, want)?, fmt, from, out)
}

/// [`tail`] on `link`, from `from`, else from the head the link read.
fn tail_on(
    cfg: &Config,
    link: TailLink,
    fmt: TailFmt,
    from: Option<u64>,
    out: &mut impl Write,
) -> io::Result<()> {
    let TailLink {
        mut conn,
        closer,
        whole,
        faces,
        head,
        read_timeout,
    } = link;
    let broker = safe(&cfg.broker, 256);
    let filter = faces
        .as_ref()
        .map_or_else(|| whole.clone(), |f| f.join(" "));
    let start = from.unwrap_or(head);
    let offset = local_offset_at_ms(crate::now_ms());
    let fmt = TailFmt { offset, ..fmt };
    eprintln!(
        "aterm fabric tail — {filter} on {broker}, from @{start}{} ({}); Ctrl-C stops{}",
        if from.is_some() {
            ""
        } else {
            " (new records only)"
        },
        zone_label(offset),
        if fmt.bodies {
            ""
        } else {
            "; bodies hidden (--bodies shows them)"
        }
    );
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
    if !fmt.json
        && writeln!(out, "{header}")
            .and_then(|()| out.flush())
            .is_err()
    {
        return Ok(());
    }
    if let Some(faces) = faces {
        return tail_faces(cfg, &mut conn, &faces, start, read_timeout, fmt, out);
    }
    let mut sub = conn.subscribe(start, &filter)?;
    closer.set_read_timeout(None)?;
    loop {
        let read = sub.recv_live(&closer, astream_broker::IDLE_MARK_DEADLINE);
        if let Some(line) = sub.take_gap().and_then(|gap| fmt.gap(&filter, &gap)) {
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return Ok(());
            }
        }
        let Some((off, subject, raw)) = read.map_err(|e| silent_link(e, &broker))? else {
            break;
        };
        let line = fmt.record(&cfg.fleet, off, &subject, &raw);
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            return Ok(());
        }
    }
    Err(io::Error::other(format!(
        "the broker at {broker} closed the stream"
    )))
}

fn tail_gap(filter: &str, gap: &astream_broker::Gap) {
    eprintln!(
        "aterm fabric tail: {filter}: the broker's retention gave up {} before this tail read \
         them",
        transport::gap_lost(gap)
    );
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

/// What one face's reader hands [`tail_faces`]: a record, where its stream stands, or a gap
/// retention made in it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum FaceRead {
    Record(u64, String, Vec<u8>),
    /// The stream's `Mark`: it delivered every record of its face below this offset, and it is
    /// caught up (the first one), or idle since (each later one).
    Caught(u64),
    Gap(astream_broker::Gap),
}

/// How many records [`tail_faces`] holds for a face still delivering its backlog before it
/// reads that face on by a fetch instead of waiting for its stream.
const FACE_HOLD_MAX: usize = 4096;

/// THE FACES MERGED BY OFFSET (audit finding BUS2-5): [`tail_faces`] reads each face on its own
/// subscription, which delivers that face's records in offset order, and prints a record only
/// once every face is past it, so `tail --json` prints offsets strictly increasing and a
/// program that stops after any line misses nothing by resuming with `--from <off>+1`. A face
/// is past `off` once every record of it below `off + 1` is taken: delivered, or read by a
/// fetch, or below the `next` of its stream's `Mark`. A record two faces deliver is printed
/// once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FaceMerge {
    /// Per face: every record of it below this offset is taken (held or printed).
    below: Vec<u64>,
    /// Per face: whether its stream said it is caught up (or its broker marks no tail). Only a
    /// face that has not may hold the merge back by waiting; one that has is read on by a
    /// fetch, never waited for, as an idle stream says nothing until its next idle mark.
    caught: Vec<bool>,
    /// Taken and not yet printed, by offset.
    held: BTreeMap<u64, (String, Vec<u8>)>,
    /// Every record below this offset is printed (or was below the tail's start).
    floor: u64,
    /// How many may be held while a face still delivering its backlog holds them back
    /// ([`FACE_HOLD_MAX`]); past it that face is read on by a fetch.
    hold: usize,
}

/// What [`FaceMerge::next`] says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeStep {
    /// Print this record: every face is past it.
    Print(u64, String, Vec<u8>),
    /// Face `face` may still hold a record below the oldest held: read it on by a fetch from
    /// its `below` (`fetch`), or wait for its stream.
    Behind { face: usize, fetch: bool },
    /// Nothing is held.
    Empty,
}

impl FaceMerge {
    /// `faces` faces, every one read from `start`, none caught up yet (a face of a broker that
    /// marks no tail is [`FaceMerge::caught`] at `start`).
    fn new(faces: usize, start: u64) -> Self {
        Self {
            below: vec![start; faces],
            caught: vec![false; faces],
            held: BTreeMap::new(),
            floor: start,
            hold: FACE_HOLD_MAX,
        }
    }

    /// Take `face`'s record at `off`: one below its `below` was read already (by a fetch).
    fn record(&mut self, face: usize, off: u64, subject: String, raw: Vec<u8>) {
        let Some(below) = self.below.get_mut(face) else {
            return;
        };
        if off < *below {
            return;
        }
        *below = off.saturating_add(1);
        if off >= self.floor {
            self.held.entry(off).or_insert((subject, raw));
        }
    }

    /// `face`'s stream marked `next`: it is caught up, and holds nothing more below `next`.
    fn caught(&mut self, face: usize, next: u64) {
        if let (Some(below), Some(caught)) = (self.below.get_mut(face), self.caught.get_mut(face)) {
            *below = (*below).max(next);
            *caught = true;
        }
    }

    /// A fetch of `face` from its `below`: `page`, in offset order, then `next`, the offset
    /// after the last record the broker scanned. Whether it moved `below` on.
    fn fetched(&mut self, face: usize, page: Vec<astream_broker::Record>, next: u64) -> bool {
        let Some(before) = self.below.get(face).copied() else {
            return false;
        };
        for (off, subject, raw) in page {
            self.record(face, off, subject, raw);
        }
        if let Some(below) = self.below.get_mut(face) {
            *below = (*below).max(next);
            *below > before
        } else {
            false
        }
    }

    /// The oldest held record once every face is past it, else the face that may not be.
    fn next(&mut self) -> MergeStep {
        let Some(off) = self.held.first_key_value().map(|(off, _)| *off) else {
            return MergeStep::Empty;
        };
        if let Some(face) = self.below.iter().position(|below| *below <= off) {
            let caught = self.caught.get(face).copied().unwrap_or(true);
            return MergeStep::Behind {
                face,
                fetch: caught || self.held.len() >= self.hold,
            };
        }
        let Some((subject, raw)) = self.held.remove(&off) else {
            return MergeStep::Empty;
        };
        self.floor = off.saturating_add(1);
        MergeStep::Print(off, subject, raw)
    }
}

/// The words a tail's error ends with: where a resume starts, as every record below `floor` is
/// printed.
fn resume_from(floor: u64) -> String {
    format!("(resume with --from {floor})")
}

/// [`tail_faces`]'s merge, run until it must wait for a stream: print what `merge` says to
/// (`print` false: the reader is gone), and read a face on where it says to by `fetch(face,
/// filter, from)`, a page in offset order and the offset after the last record the broker
/// scanned, which waits `deadline` at most for the broker. Whether the reader is still there.
/// A fetch that fails, or moves nothing, ends the tail with an error that names the face and
/// the `--from` a resume starts at ([`resume_from`]), as every record below the merge's floor
/// is printed. LAWS: it returns only where nothing is held or a face that has not said it is
/// caught up holds the merge with fewer than its hold held.
fn drain_merge(
    merge: &mut FaceMerge,
    faces: &[String],
    broker: &str,
    deadline: Duration,
    mut fetch: impl FnMut(usize, &str, u64) -> io::Result<(Vec<astream_broker::Record>, u64)>,
    mut print: impl FnMut(u64, &str, &[u8]) -> bool,
) -> io::Result<bool> {
    loop {
        match merge.next() {
            MergeStep::Print(off, subject, raw) => {
                if !print(off, &subject, &raw) {
                    return Ok(false);
                }
            }
            MergeStep::Behind { face, fetch: true } => {
                let (Some(&from), Some(filter)) = (merge.below.get(face), faces.get(face)) else {
                    return Ok(true);
                };
                let failed = |e: io::Error, floor: u64| {
                    let why = match e.kind() {
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                            format!("no answer in {} s", deadline.as_secs())
                        }
                        _ => e.to_string(),
                    };
                    io::Error::new(
                        e.kind(),
                        format!(
                            "the broker at {broker} did not read {} on from @{from}: {why} {}",
                            safe(filter, 128),
                            resume_from(floor)
                        ),
                    )
                };
                let (page, next) = fetch(face, filter, from).map_err(|e| failed(e, merge.floor))?;
                if !merge.fetched(face, page, next) {
                    return Err(failed(io::Error::other("it moved nothing"), merge.floor));
                }
            }
            MergeStep::Behind { fetch: false, .. } | MergeStep::Empty => return Ok(true),
        }
    }
}

/// One face's reader: its stream's events, each sent to [`tail_faces`] with the face's index,
/// until the stream fails or ends or nobody takes them.
fn read_face(
    face: usize,
    mut sub: astream_broker::AnySubscription,
    closer: &astream_broker::Closer,
    broker: &str,
    tx: &std::sync::mpsc::SyncSender<(usize, io::Result<FaceRead>)>,
) {
    use astream_broker::Event;
    loop {
        let read = match sub.recv_live_event(closer, astream_broker::IDLE_MARK_DEADLINE) {
            Ok(Some(Event::Delivery {
                offset,
                subject,
                body,
            })) => Ok(FaceRead::Record(offset, subject, body)),
            Ok(Some(Event::Mark { next, .. })) => Ok(FaceRead::Caught(next)),
            Ok(Some(Event::Gap(gap))) => {
                let _ = sub.take_gap();
                Ok(FaceRead::Gap(gap))
            }
            Ok(None) => Err(io::Error::other(format!(
                "the broker at {broker} closed a face's stream"
            ))),
            Err(e) => Err(silent_link(e, broker)),
        };
        let last = read.is_err();
        if tx.send((face, read)).is_err() || last {
            return;
        }
    }
}

/// `tail` where the caps do not grant the fleet's root: one subscription per readable face
/// ([`readable_faces`]), merged by offset ([`FaceMerge`], [`drain_merge`]). `conn` reads a face
/// on by a fetch where the merge says to, each read on it bounded by `deadline` (its
/// [`TailLink`]'s, which for [`tail`] is astream's bound on a stream,
/// [`astream_broker::IDLE_MARK_DEADLINE`]). A face of a broker that refuses `IdleMarks` (one
/// that predates them) is caught from `start`, as its stream marks nothing: it is read on by
/// fetches, never waited for.
fn tail_faces(
    cfg: &Config,
    conn: &mut Conn,
    faces: &[String],
    start: u64,
    deadline: Duration,
    fmt: TailFmt,
    out: &mut impl Write,
) -> io::Result<()> {
    use std::sync::mpsc;
    let broker = safe(&cfg.broker, 256);
    let (tx, rx) = mpsc::sync_channel::<(usize, io::Result<FaceRead>)>(0);
    let mut merge = FaceMerge::new(faces.len(), start);
    for (i, face) in faces.iter().enumerate() {
        let (mut sub_conn, closer) = transport::connect(&cfg.broker, io_timeout())?;
        transport::attach_cap_files(&mut sub_conn, &cfg.cap_files)?;
        if !sub_conn.idle_marks()? {
            merge.caught(i, start);
        }
        let sub = sub_conn.subscribe(start, face)?;
        closer.set_read_timeout(None)?;
        let tx = tx.clone();
        let broker = broker.clone();
        std::thread::spawn(move || read_face(i, sub, &closer, &broker, &tx));
    }
    drop(tx);
    loop {
        let there = drain_merge(
            &mut merge,
            faces,
            &broker,
            deadline,
            |_, filter, from| {
                transport::fetch_told_kept(conn, from, filter, transport::POST_PAGE)
                    .map(|((page, (next, _)), _)| (page, next))
            },
            |off, subject, raw| {
                let line = fmt.record(&cfg.fleet, off, subject, raw);
                writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
            },
        )?;
        if !there {
            return Ok(());
        }
        let Ok((face, read)) = rx.recv() else {
            break;
        };
        match read? {
            FaceRead::Record(off, subject, raw) => merge.record(face, off, subject, raw),
            FaceRead::Caught(next) => merge.caught(face, next),
            FaceRead::Gap(gap) => {
                let filter = faces.get(face).map_or("", String::as_str);
                if let Some(line) = fmt.gap(filter, &gap) {
                    if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
    Err(io::Error::other(format!(
        "the broker at {broker} closed a face's stream"
    )))
}

#[cfg(test)]
#[path = "fabric_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "fabric_running_argv_tests.rs"]
mod running_argv_tests;
