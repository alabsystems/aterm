// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric`: fabric status, live traffic (`tail`), session-log brief (`brief`), lifecycle
//! (`on`, `off`, `doctor` in [`crate::enable`]), and person commands (`mint-for`, `halt`,
//! `inbox`, `post` in [`crate::person`]).

#[cfg(any(test, not(target_os = "macos")))]
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
       aterm fabric on  [--dry-run] [--fleet <F>] [--service launchd|systemd|none]
                        [--durability strict|ordered|relaxed]
       aterm fabric off [--dry-run] [--service launchd|systemd|none]
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
  --bodies         tail: print each record's text after its trust label (--json:
                   `body`); an `ev` record's text is the session log's bus line, whose
                   grammar `aterm help fabric` gives under THE SESSION LOG ON THE BUS
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
  --service <s>    on/off: `launchd`, `systemd`, or `none` (a broker run some other way)
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
                   guidance to stderr
  --expires <N>d|<N>h
                   mint-for: stamp each grant `exp=` so the broker refuses it after that
  halt <reason>    as a human's cap: hold every session on the fleet (`hold=1
                   origin=fleet`); `halt off` withdraws this human's halt
  inbox            as a person's cap: print its unread mail and mark it read (`--peek`
                   leaves it unread)
  post             as a person's cap: send to `@<sid>@<node>` with `kind=ask|answer|task|
                   report|note` (default `note`) and optional `re=<off>`
  --cap-file <c>   halt/inbox/post: the person's cap (`mint-for h-<name> --out <c>`);
                   brief: read under it instead of the node's cap

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
/// dial, and each read and write ([`transport::connect`]). This module, the roster (`cli`) and
/// the brief read it through [`io_timeout`]; a person's commands (`person`) keep it fixed,
/// beneath their subscription's silence bound.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// The development seam that replaces [`IO_TIMEOUT`] (milliseconds; debug builds and
/// `dev-seams` only): a test on a loaded machine bounds these sockets, the doctor's read of an
/// instance among them, within its one-minute hang detector, so an instance or broker the load
/// keeps from answering for 3 s is waited for rather than reported as unanswerable.
const IO_TIMEOUT_SEAM: &str = "ATERM_FABRIC_IO_TIMEOUT_MS";

/// [`IO_TIMEOUT`], or the [`IO_TIMEOUT_SEAM`] a development build was given. The unit tests'
/// brokers and instances run in-process, so a socket one of them answers late is a loaded
/// machine, not a peer gone silent: unseamed, theirs wait the hang detector.
pub(crate) fn io_timeout() -> Duration {
    #[cfg(not(test))]
    const UNSEAMED: Duration = IO_TIMEOUT;
    #[cfg(test)]
    const UNSEAMED: Duration = aterm_spec::wait::MUST_HAPPEN;
    static AT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        aterm_types::dev_seam!(IO_TIMEOUT_SEAM)
            .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
            .map_or(UNSEAMED, Duration::from_millis)
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
#[must_use]
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

/// `[fabric] receipts` from the aterm.toml [`config_path`] resolves, for a `serve` whose
/// command line said neither `--receipts` nor `--no-receipts`.
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

/// One process, as `ps` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
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

/// The machine's process table, or empty when it cannot be read (a sandbox), so every consumer
/// says "not found" rather than guessing.
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
    aterm_uds::spawnfd::command(aterm_uds::os_path!("ps"))
        .args(["-A", "-ww", "-o", "pid=", "-o", "ppid=", "-o", "command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// The process serving the broker at `sock`: the one process of this user holding the listening
/// socket bound there, by the socket's own record ([`aterm_uds::holders::scan_listener_holders`]),
/// never by a command line that names the socket. `None` when no process holds it, two do, or the
/// scan cannot run here.
pub(crate) fn listener_pid(sock: &str) -> Option<u32> {
    aterm_uds::holders::scan_listener_holders(Path::new(sock), &[])
        .ok()
        .as_ref()
        .and_then(the_one_holder)
}

/// The holder a scan found, when it found exactly one. A table the scan could not read does not
/// unseat it: Linux seals every non-dumpable process's table (an `ssh-agent`), so a scan that
/// had to read every table would name no broker on most hosts, and a broker never seals its own.
fn the_one_holder(scan: &aterm_uds::holders::HolderScan) -> Option<u32> {
    match scan.holders.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// The launchd job label whose running pid is `pid`, from `launchctl list`'s
/// `PID  Status  Label` rows.
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
    /// Whether a broker answered: a connect and a `Hello` round trip in this build's astream
    /// protocol.
    pub reachable: bool,
    /// Why it is not reachable, or why the cap file was refused.
    pub error: Option<String>,
    /// A broker that answered the `Hello` in another astream protocol: neither side can read
    /// what the other sends, so it is not `reachable`, and this names why.
    pub protocol: Option<astream_broker::ProtocolMismatch>,
    /// The last line the broker's supervisor captured from its stderr (`<root>/broker.err`) and
    /// its age, read only when the broker does not answer: the broker's own reason it is not
    /// running, such as a log it refuses to open or an address in use
    /// ([`crate::enable::supervisor_last_word`]).
    pub last_word: Option<String>,
    /// Whether every cap was attached, so the bus can be read.
    pub attached: bool,
    /// Whether the broker refused the ring as lapsed
    /// ([`astream_broker::BrokerRefusal::is_expired`]): genuine grants whose `exp=` has passed,
    /// at the attach or on the head query.
    pub cap_expired: bool,
    /// The pid serving it: the one process of this user holding its listener ([`listener_pid`]).
    pub pid: Option<u32>,
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
    /// beside which astream keeps its health snapshot.
    pub log: Option<PathBuf>,
    /// astream's health snapshot beside that log (`<log>.health`: what the log holds, who is
    /// attached, its flushes and its poison), as [`astream_broker::read_health`] found it, with
    /// whether the process that wrote it still serves the log.
    pub snapshot: Option<Result<astream_broker::HealthRead, String>>,
}

const GUARD_PROBE_SECRET: &[u8] = b"aterm fabric guard probe: no broker's secret";

impl BrokerView {
    /// Whether the bus was read: connect, `Hello`, every attach and the head query all answered.
    #[must_use]
    pub const fn read_ok(&self) -> bool {
        self.reachable && self.attached && self.head.is_some()
    }

    /// The tier the report names: the one the serving process's argv names
    /// ([`Self::durability`]), else, for a broker whose argv this host cannot read, the one its
    /// `Hello` says it was opened at ([`Self::served`]).
    #[must_use]
    pub fn tier(&self) -> Option<crate::durability::Durability> {
        self.durability.or_else(|| {
            self.served
                .and_then(|s| crate::durability::Durability::of_tier(s.requested))
        })
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

    /// Name the pid serving [`Self::endpoint`] ([`listener_pid`]), its launchd job, and what
    /// its own argv says it serves; no other process is read.
    pub(crate) fn locate(&mut self) {
        self.locate_with(listener_pid, launchd_label, replaced_build, |pid| {
            aterm_uds::process::read_process_argv(i32::try_from(pid).ok()?)
        });
    }

    fn locate_with(
        &mut self,
        mut holder: impl FnMut(&str) -> Option<u32>,
        mut label: impl FnMut(u32) -> Option<String>,
        mut replaced: impl FnMut(u32, &str) -> Option<String>,
        mut argv: impl FnMut(u32) -> Option<Vec<std::ffi::OsString>>,
    ) {
        self.pid = holder(&self.endpoint);
        self.launchd = self.pid.and_then(&mut label);
        let words = self.pid.and_then(&mut argv);
        let exe = words
            .as_ref()
            .and_then(|w| w.first())
            .map(|w| w.to_string_lossy().into_owned());
        self.replaced = self.pid.zip(exe).and_then(|(pid, exe)| replaced(pid, &exe));
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
    }
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
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("lsof"))
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

/// The words of `aterm fabric`'s lapsed-ring warning ([`BrokerView::cap_expired`]), by which
/// `doctor` picks the renewal ([`crate::enable::fix_for`]).
pub const CAP_LAPSED: &str = "refuses this node's cap as lapsed";

/// The words of `aterm fabric`'s poisoned-log warning ([`BrokerView::poisoned`]), by which
/// `doctor` picks its remedy ([`crate::enable::fix_for`]).
pub const POISONED_WARNING: &str = "says its log is POISONED";

/// The words of the warning for a broker whose flushes failed without poisoning its log, by
/// which `doctor` picks the disk's remedy.
pub const FLUSH_FAILED_WARNING: &str = "says a flush of its log failed";

/// Name the broker's process ([`BrokerView::locate`]), read the health snapshot beside its log,
/// and reach it as [`reach_broker`] does.
pub(crate) fn probe_broker(cfg: &Config) -> (BrokerView, Option<Conn>) {
    let mut view = BrokerView {
        endpoint: cfg.broker.clone(),
        ..BrokerView::default()
    };
    view.locate();
    view.snapshot = view
        .log
        .as_deref()
        .and_then(|log| match astream_broker::read_health(log) {
            Ok(read) => read.map(Ok),
            Err(e) => Some(Err(e.to_string())),
        });
    reach(cfg, view)
}

/// Reach the broker, naming no process: connect, `Hello`, the whole ring attached
/// ([`transport::attach_ring`]), and a head query.
pub(crate) fn reach_broker(cfg: &Config) -> (BrokerView, Option<Conn>) {
    reach(
        cfg,
        BrokerView {
            endpoint: cfg.broker.clone(),
            ..BrokerView::default()
        },
    )
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
        (Ok(cfg), None) => summary_of(&probe(&cfg)),
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

/// [`doctor_summary`]'s verdict on one probe, pure: the broker's own words (its `Hello`'s
/// served tier and health entries), never the argv's.
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
    /// Who sent it: the cap-forced `<src>` segment, or `<sid>@<node>` when a node attests which
    /// of its sessions spoke (the bridge's own rule).
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

/// TRAFFIC: the newest `n` records the broker holds of the fleet (or of `faces`), read TOLD
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
                    scan.feed(fleet, *off, subject, raw);
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
        scan.feed(fleet, *off, subject, raw);
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
    fn feed(&mut self, fleet: &str, off: u64, subject: &str, raw: &[u8]) {
        let (body, _) = Body::decode(raw);
        let (dl, re) = (body.dl, body.re);
        let lane = verdict_lane(fleet, subject, body.from.as_deref());
        let t = traffic_of_body(fleet, off, subject, body);
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
        let (from, to) = range.strip_prefix('@')?.split_once("..@")?;
        let (from, to) = (from.parse::<u64>().ok()?, to.parse::<u64>().ok()?);
        (from <= to).then(|| Self {
            from,
            to,
            records: count.and_then(|n| n.parse().ok()),
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
    /// Its bridge child pid(s), from the process table.
    pub bridge_pids: Vec<u32>,
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
    let num = |k: &str| kv(header0, k).and_then(|v| v.parse::<u64>().ok());
    s.hold = kv(header0, "hold").and_then(|v| v.parse::<u8>().ok());
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
            v.rtt_ms = kv(h, "rtt_ms").and_then(|n| n.parse().ok());
            v.link_age_ms = kv(h, "link_age_ms").and_then(|n| n.parse().ok());
            v.supervised = kv(h, "supervised").map(|s| s == "1");
            v.build = kv(h, "build").filter(|b| *b != "-").map(str::to_string);
            v.bridge_build = kv(h, "bridge_build")
                .filter(|b| *b != "-")
                .map(str::to_string);
            v.lane_faults = kv(h, "lane_faults").and_then(|n| n.parse().ok());
            v.mail_gaps = kv(h, "mail_gaps").and_then(|n| n.parse().ok());
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
            Ok(d) => {
                report.overdue = d.overdue;
                report.overdue_unjudged = d.unjudged;
            }
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

/// The words each warning of records the broker's retention gave up says what happened with,
/// which `doctor` matches for its fix ([`crate::enable::fix_for`]).
pub const RETENTION_GAVE_UP: &str = "the broker's retention gave up";

/// The words of the warning that counts the asks past their deadline the deadline scan cannot
/// judge ([`Report::overdue_unjudged`]), which `doctor` matches for its fix
/// ([`crate::enable::fix_for`]).
pub const OVERDUE_UNJUDGED: &str = "past their deadline cannot be judged";

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
        if let Some(line) = node_mail_given_up(inst) {
            w.push(line);
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
            if let Some(lost) = session_given_up(s) {
                w.push(format!(
                    "{at}: {RETENTION_GAVE_UP} records of its own before it handled them: {lost}"
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
            match o.expired {
                Some(true) => "the asker's bridge recorded it expired",
                Some(false) => "not yet recorded expired by the asker's bridge",
                None => "whether the asker's bridge recorded it expired is not kept on the bus",
            }
        ));
    }
    if r.overdue_unjudged > 0 {
        w.push(format!(
            "{} held ask(s) {OVERDUE_UNJUDGED}: the bus no longer keeps (or this cap does not \
             read) every answer, report and ack above them on their asker's lane, so status \
             cannot say whether they had one",
            r.overdue_unjudged
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
                 hold at once: a departed session's presence row and the lanes this node wrote \
                 to it go back five minutes after it leaves (its `ev` face once the session-log \
                 mirror landed its tail), but each topic a session broadcast on keeps one \
                 subject for good, so what stays is held by live sessions, every topic face a \
                 session of this node ever broadcast on, mail still owed and the node's own \
                 faces; past the bound a new session gets no presence row and its posts are \
                 refused",
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

/// The local clock's offset from UTC in seconds, read once per process (`tm_gmtoff`); `None`
/// where the C library cannot say, and the report then prints UTC and says so.
pub(crate) fn local_offset() -> Option<i64> {
    static OFFSET: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(aterm_uds::zone::local_offset_now)
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

/// The node and session a record's subject names: `/f/<F>/pub|in|term/<node>/<sid>/…`, the
/// node an `n-` principal and the sid an `s-` one (a node's own face, a person's lane and the
/// fleet's lanes name no session).
fn subject_session<'a>(fleet: &str, subject: &'a str) -> (Option<&'a str>, Option<&'a str>) {
    let segs: Vec<&str> = subject.split('/').collect();
    let faced = segs.len() >= 6
        && segs[0].is_empty()
        && segs[1] == "f"
        && segs[2] == fleet
        && matches!(segs[3], "pub" | "in" | "term");
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
    /// The local clock's offset from UTC, seconds (the table's TIME column).
    offset: i64,
}

impl TailFmt {
    const fn new(bodies: bool, json: bool) -> Self {
        Self {
            bodies,
            json,
            offset: 0,
        }
    }

    fn record(self, fleet: &str, off: u64, subject: &str, raw: &[u8]) -> String {
        let t = traffic_of(fleet, off, subject, raw);
        if self.json {
            tail_json(fleet, subject, &t, self.bodies)
        } else {
            tail_line(&t, crate::now_ms(), self.offset, self.bodies)
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

fn tail(
    cfg: &Config,
    fmt: TailFmt,
    from: Option<u64>,
    want: Option<&str>,
    out: &mut impl Write,
) -> io::Result<()> {
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
    let filter = faces
        .as_ref()
        .map_or_else(|| whole.clone(), |f| f.join(" "));
    let start = from.unwrap_or(head);
    let offset = local_offset();
    let fmt = TailFmt {
        offset: offset.unwrap_or(0),
        ..fmt
    };
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
        closer.set_read_timeout(Some(astream_broker::IDLE_MARK_DEADLINE))?;
        return tail_faces(cfg, &mut conn, &faces, start, fmt, out);
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

/// [`tail_faces`]'s merge, run until it must wait for a stream: print what `merge` says to
/// (`print` false: the reader is gone), and read a face on where it says to by `fetch(face,
/// filter, from)`, a page in offset order and the offset after the last record the broker
/// scanned. Whether the reader is still there. A fetch that fails, or moves nothing, ends the
/// tail with an error that names the face and the `--from` a resume starts at, as every record
/// below the merge's floor is printed. LAWS: it returns only where nothing is held or a face
/// that has not said it is caught up holds the merge with fewer than its hold held.
fn drain_merge(
    merge: &mut FaceMerge,
    faces: &[String],
    broker: &str,
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
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => format!(
                            "no answer in {} s",
                            astream_broker::IDLE_MARK_DEADLINE.as_secs()
                        ),
                        _ => e.to_string(),
                    };
                    io::Error::new(
                        e.kind(),
                        format!(
                            "the broker at {broker} did not read {} on from @{from}: {why} \
                             (resume with --from {floor})",
                            safe(filter, 128)
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
/// on by a fetch where the merge says to; `tail` bounds its reads as astream bounds a stream's
/// ([`astream_broker::IDLE_MARK_DEADLINE`]), not by the 3 s of a one-shot request, as a live
/// tail fetches for as long as it runs.
fn tail_faces(
    cfg: &Config,
    conn: &mut Conn,
    faces: &[String],
    start: u64,
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
        for help in ["brief --help", "brief -h", "brief --json -h", "tail --help"] {
            assert_eq!(parse_args(&argv(help)), Ok(Cmd::Help), "{help}");
        }
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
                json: false,
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
            "doctor --json",
            "on --json",
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
    fn tail_takes_json_beside_its_other_flags_and_the_others_name_where_it_is_valid() {
        let tail = |bodies: bool, json: bool, from: Option<u64>| {
            Ok(Cmd::Tail {
                bodies,
                json,
                from,
                filter: None,
            })
        };
        assert_eq!(parse_args(&argv("tail --json")), tail(false, true, None));
        assert_eq!(
            parse_args(&argv("tail --bodies --json --from @5")),
            tail(true, true, Some(5))
        );
        assert_eq!(parse_args(&argv("tail --bodies")), tail(true, false, None));
        for verb in ["doctor", "on", "off", "mint-for h-jsmith"] {
            let e = parse_args(&argv(&format!("{verb} --json"))).expect_err(verb);
            assert!(
                e.contains("`aterm fabric tail --json`") && e.contains("`aterm fabric --json`"),
                "{verb}: {e}"
            );
        }
        let help = usage();
        assert!(
            help.contains("aterm fabric tail [--bodies] [--json]"),
            "{help}"
        );
        assert_lists(&help, &TAIL_KEYS, "the keys tail_json writes");
    }

    /// `help` prints `keys` as one whole list, and [`key_list`] names exactly them, in order.
    fn assert_lists(help: &str, keys: &[&str], what: &str) {
        let list = key_list(keys);
        let named: Vec<&str> = list
            .split_whitespace()
            .map(|w| w.trim_matches('`'))
            .collect();
        assert_eq!(named, keys, "{what}: the list names them, in order");
        assert!(
            help.contains(&format!("\n{list}")),
            "{what}: the help prints that list whole: {help}"
        );
    }

    #[test]
    fn the_help_lists_the_keys_the_brief_writes_for_each_session() {
        let help = usage();
        assert_lists(
            &help,
            &crate::fabric_brief::SESSION_KEYS,
            "the keys session_json writes",
        );
        assert!(
            !help.contains("@SESSION_KEYS@") && !help.contains("@TAIL_KEYS@"),
            "every key list is filled: {help}"
        );
    }

    #[test]
    fn tail_from_reads_a_bus_offset_by_the_shared_grammar() {
        for from in ["5", "@5"] {
            let args = ["tail", "--from", from].map(str::to_string);
            assert_eq!(
                parse_args(&args),
                Ok(Cmd::Tail {
                    bodies: false,
                    json: false,
                    from: Some(5),
                    filter: None,
                }),
                "{from}"
            );
        }
        for from in ["+5", "@", "", "@-1"] {
            let args = ["tail", "--from", from].map(str::to_string);
            let refused = parse_args(&args);
            assert!(
                refused
                    .as_ref()
                    .is_err_and(|e| e.contains("--from") && e.contains(from)),
                "{from:?}: refused, naming the flag and the value: {refused:?}"
            );
        }
    }

    /// A writer that keeps what it is given and refuses a write once it holds `lines` lines,
    /// as a closed pipe does (the tail then ends, as it does when its reader goes).
    struct Lines {
        got: Vec<u8>,
        lines: usize,
    }

    impl Write for Lines {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.got.iter().filter(|c| **c == b'\n').count() >= self.lines {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.got.extend_from_slice(b);
            Ok(b.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    #[test]
    fn tail_json_prints_one_object_per_record_and_a_body_only_under_bodies() {
        let dir = std::env::temp_dir().join(format!("atlink-tail-json-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let broker = crate::test_broker(dir.join("t.log")).expect("a broker");
        let sock = dir.join("t.sock");
        let mut server = broker.serve(&sock).expect("serves");
        let (mut conn, _) =
            transport::connect(&sock.to_string_lossy(), aterm_spec::wait::MUST_HAPPEN)
                .expect("dial");
        let records = [
            (
                "/f/f1/in/n-a/s-x/h-jsmith/note",
                "v=1 t=1700000000000 text=QZP8NOTE",
            ),
            ("/f/f1/pub/n-a/node/presence", "v=1 t=1700000000001 inc=3"),
            (
                "/f/f1/pub/n-a/s-x/presence",
                "v=1 t=1700000000002 state=live",
            ),
        ];
        let mut offs = Vec::new();
        for (seq, (subject, body)) in (1..).zip(records) {
            offs.push(
                conn.publish(7, seq, subject, body.as_bytes())
                    .expect("lands")
                    .0,
            );
        }
        let sock = sock.to_string_lossy().into_owned();
        let cfg = crate::cli::parse(&[
            "--fleet".to_string(),
            "f1".to_string(),
            "--broker".to_string(),
            sock,
        ])
        .expect("a config")
        .cfg;
        for bodies in [false, true] {
            let mut out = Lines {
                got: Vec::new(),
                lines: 2,
            };
            tail(&cfg, TailFmt::new(bodies, true), Some(0), None, &mut out)
                .expect("the tail ends when its reader does");
            let text = String::from_utf8(out.got).expect("utf-8");
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(
                lines.len(),
                2,
                "{bodies}: no header, one line per record: {text}"
            );
            for (i, line) in lines.iter().enumerate() {
                let v: aterm_json::Value = aterm_json::from_str(line)
                    .unwrap_or_else(|e| panic!("{bodies}: one JSON object: {e}: {line}"));
                let aterm_json::Value::Object(map) = &v else {
                    panic!("{bodies}: an object: {line}");
                };
                let keys: BTreeSet<&str> = map.keys().map(String::as_str).collect();
                let want: BTreeSet<&str> = TAIL_KEYS
                    .iter()
                    .copied()
                    .filter(|k| bodies || *k != "body")
                    .collect();
                assert_eq!(keys, want, "{bodies}: {line}");
                let (subject, body) = records[i];
                let field = |k: &str| v.get(k).and_then(aterm_json::Value::as_str);
                assert_eq!(
                    v.get("off").and_then(aterm_json::Value::as_u64),
                    Some(offs[i])
                );
                assert_eq!(field("subject"), Some(subject));
                assert_eq!(field("node"), Some("n-a"));
                assert_eq!(field("sid"), [Some("s-x"), None][i], "{line}");
                assert_eq!(field("kind"), Some(["note", "presence"][i]));
                let t = traffic_of("f1", offs[i], subject, body.as_bytes());
                let table = tail_line(&t, 0, 0, true);
                let printed = table.split_once("  text=").map_or("", |(_, text)| text);
                assert_eq!(
                    field("body"),
                    bodies.then_some(printed),
                    "{bodies}: the body is the text --bodies prints, and only under it"
                );
                assert_eq!(
                    line.contains("QZP8NOTE"),
                    bodies && i == 0,
                    "{bodies}: {line}"
                );
            }
        }
        server.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fetch of face `face` of `log` from `from` on a broker whose head is `head`: the face's
    /// records in `from..head`, then `head` as the offset after the last one scanned.
    fn face_page(
        log: &[Vec<u64>],
        head: u64,
        face: usize,
        from: u64,
    ) -> (Vec<astream_broker::Record>, u64) {
        let page = log[face]
            .iter()
            .filter(|off| (from..head).contains(*off))
            .map(|off| (*off, format!("/f/f1/s{off}"), Vec::new()))
            .collect();
        (page, head)
    }

    /// [`tail_faces`]'s loop over `merge`, as it runs: [`drain_merge`] (a face read on by
    /// `fetch(face, below)`), then the next of `events`. What it printed. LAWS: the merge waits
    /// only for a face that has not said it is caught up, and holds fewer than its hold while it
    /// does.
    fn drive_merge(
        merge: &mut FaceMerge,
        events: Vec<(usize, FaceRead)>,
        fetch: impl Fn(usize, u64) -> (Vec<astream_broker::Record>, u64),
    ) -> Vec<u64> {
        let faces: Vec<String> = (0..merge.below.len()).map(|f| format!("face{f}")).collect();
        let mut printed = Vec::new();
        let mut events = events.into_iter();
        loop {
            let there = drain_merge(
                merge,
                &faces,
                "/b",
                |face, _, from| Ok(fetch(face, from)),
                |off, _, _| {
                    printed.push(off);
                    true
                },
            )
            .expect("every fetch moves its face on");
            assert!(there, "the reader is there");
            if let MergeStep::Behind { face, fetch } = merge.next() {
                assert!(!fetch, "drain_merge returned with face {face} to read on");
                assert!(
                    !merge.caught[face],
                    "the merge waits for face {face}, which said it is caught up"
                );
                assert!(
                    merge.held.len() < merge.hold,
                    "the merge waits for face {face} holding {} records, its hold {}",
                    merge.held.len(),
                    merge.hold
                );
            }
            let Some((face, read)) = events.next() else {
                return printed;
            };
            match read {
                FaceRead::Record(off, subject, raw) => merge.record(face, off, subject, raw),
                FaceRead::Caught(next) => merge.caught(face, next),
                FaceRead::Gap(_) => {}
            }
        }
    }

    /// THE FACES ARE MERGED BY OFFSET (audit findings BUS2-5 and LOOP2-3; [`FaceMerge`]): four
    /// faces of one log, the fourth the same as the first (as `pub/>` and `pub/<node>/>` are),
    /// each delivering its own records in offset order while the faces interleave as their
    /// threads race, a sparse one caught up early, and offsets 5 and 8 on none. The merge
    /// prints every record once, offsets strictly increasing, waiting only for a face still
    /// delivering its backlog. A caught-up face is read on by a fetch: a live record on one face
    /// prints while the others are idle, and one a face's stream has not delivered yet is
    /// fetched and printed first, its delivery then taken as read. Past its hold, a face still
    /// delivering its backlog is read on by a fetch too, though its stream says nothing. Every
    /// face of a broker that marks no tail is caught from its start ([`tail_faces`]), so every
    /// record prints though no stream ever marks. A fetch that fails ends the merge naming the
    /// `--from` a resume starts at.
    /// NEGATIVE CONTROLS (run): printing the oldest held at once (arrival order) prints 1 first,
    /// and 0 never; waiting for a caught-up face's stream breaks the law above; waiting past the
    /// hold for a face not caught up prints nothing past the hold; a fetch's error passed on
    /// as it came names no `--from`.
    #[test]
    fn faces_merge_by_offset_and_a_caught_up_face_is_read_on_never_waited_for() {
        let rec = |off: u64| FaceRead::Record(off, format!("/f/f1/s{off}"), Vec::new());
        let mut log = vec![
            vec![0, 3, 6, 9],
            vec![1, 4, 7, 10],
            vec![2, 11],
            vec![0, 3, 6, 9],
        ];
        let mut merge = FaceMerge::new(4, 0);
        let backlog = vec![
            (1, rec(1)),
            (0, rec(0)),
            (2, rec(2)),
            (3, rec(0)),
            (0, rec(3)),
            (1, rec(4)),
            (3, rec(3)),
            (0, rec(6)),
            (2, rec(11)),
            (2, FaceRead::Caught(12)),
            (1, rec(7)),
            (3, rec(6)),
            (0, rec(9)),
            (1, rec(10)),
            (3, rec(9)),
            (0, FaceRead::Caught(12)),
            (1, FaceRead::Caught(12)),
            (3, FaceRead::Caught(12)),
        ];
        let printed = drive_merge(&mut merge, backlog, |f, from| face_page(&log, 12, f, from));
        assert_eq!(printed, vec![0, 1, 2, 3, 4, 6, 7, 9, 10, 11], "the backlog");

        log[1].push(12);
        let printed = drive_merge(&mut merge, vec![(1, rec(12))], |f, from| {
            face_page(&log, 13, f, from)
        });
        assert_eq!(printed, vec![12], "a live record, the other faces idle");

        log[2].push(13);
        log[0].push(14);
        log[3].push(14);
        let live = vec![(0, rec(14)), (2, rec(13)), (3, rec(14))];
        let printed = drive_merge(&mut merge, live, |f, from| face_page(&log, 15, f, from));
        assert_eq!(
            printed,
            vec![13, 14],
            "a record a face's stream has not delivered yet comes first, and once"
        );
        assert!(merge.held.is_empty(), "{merge:?}");

        let log = vec![vec![0, 1, 2, 3], vec![5]];
        let mut merge = FaceMerge::new(2, 0);
        merge.hold = 2;
        let events = vec![
            (0, rec(0)),
            (0, rec(1)),
            (0, rec(2)),
            (0, rec(3)),
            (0, FaceRead::Caught(6)),
        ];
        let printed = drive_merge(&mut merge, events, |f, from| face_page(&log, 6, f, from));
        assert_eq!(
            printed,
            vec![0, 1, 2, 3, 5],
            "past the hold, the silent face is fetched"
        );

        let log = vec![vec![10, 12], vec![11, 13]];
        let mut merge = FaceMerge::new(2, 10);
        for face in 0..2 {
            merge.caught(face, 10);
        }
        let events = vec![(1, rec(11)), (0, rec(10)), (1, rec(13)), (0, rec(12))];
        let printed = drive_merge(&mut merge, events, |f, from| face_page(&log, 14, f, from));
        assert_eq!(printed, vec![10, 11, 12, 13], "a broker that marks no tail");

        let faces = ["/f/f1/pub/>".to_string(), "/f/f1/in/n-a/>".to_string()];
        let mut merge = FaceMerge::new(2, 0);
        merge.caught(0, 0);
        merge.record(1, 0, "/f/f1/s0".to_string(), Vec::new());
        merge.record(1, 3, "/f/f1/s3".to_string(), Vec::new());
        let mut printed = Vec::new();
        let ended = drain_merge(
            &mut merge,
            &faces,
            "/b",
            |face, _, from| match from {
                0 => Ok(face_page(&[vec![1, 2], vec![]], 3, face, from)),
                _ => Err(io::Error::from(io::ErrorKind::TimedOut)),
            },
            |off, _, _| {
                printed.push(off);
                true
            },
        );
        assert_eq!(printed, vec![0, 1, 2], "printed before the fetch failed");
        assert!(
            ended
                .as_ref()
                .is_err_and(|e| e.kind() == io::ErrorKind::TimedOut
                    && e.to_string().contains("--from 3")
                    && e.to_string().contains(&faces[0])),
            "{ended:?}"
        );
    }

    /// A writer that hands each line it is given to `tx` and refuses a write once it has sent
    /// `lines` lines, as a closed pipe does (the tail then ends).
    struct Sent {
        tx: std::sync::mpsc::Sender<String>,
        buf: Vec<u8>,
        lines: usize,
    }

    impl Write for Sent {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            if self.lines == 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.buf.extend_from_slice(b);
            while let Some(at) = self.buf.iter().position(|c| *c == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=at).collect();
                let _ = self
                    .tx
                    .send(String::from_utf8_lossy(&line).trim_end().to_string());
                self.lines = self.lines.saturating_sub(1);
            }
            Ok(b.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The `off` of a `tail --json` record line.
    fn tail_off(line: &str) -> Option<u64> {
        aterm_json::from_str::<aterm_json::Value>(line)
            .ok()?
            .get("off")?
            .as_u64()
    }

    /// `tail --json` UNDER A NODE CAP PRINTS OFFSETS STRICTLY INCREASING, SO A RESUME FROM THE
    /// LAST ONE PLUS ONE MISSES NOTHING (audit findings BUS2-5 and LOOP2-3): on a guarded broker
    /// a node's cap (`enable::node_grants`) does not grant the fleet's root, so the tail reads
    /// each of its seven faces apart. 250 records cycle over subjects on four faces (one on two,
    /// `pub/>` and `pub/n-a/>`) and one the node cannot read. A tail from 0 stopped after 100
    /// lines and one resumed from its last offset plus one print, between them, every record the
    /// node reads, once, in offset order, and the resumed tail then prints each record published
    /// live on one face while the others are idle. The broker keeps its default idle mark, so
    /// no phase races astream's link deadline; that a caught-up face is read on, never waited
    /// for, is [`faces_merge_by_offset_and_a_caught_up_face_is_read_on_never_waited_for`]'s law.
    /// NEGATIVE CONTROLS (run): printing each record as it arrives, deduplicated (the code before
    /// this fix), prints `[1, 0, 2, 3, 6, 11, 7, …]`; printing in arrival order fails it too.
    #[cfg(unix)]
    #[test]
    fn tail_json_under_a_node_cap_prints_offsets_in_order_so_a_resume_misses_none() {
        use aterm_spec::wait::MUST_HAPPEN;
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-tail-faces")
            .tempdir_in("/tmp")
            .expect("scratch");
        let secret = vec![0x5a_u8; 32];
        let broker = astream_broker::Broker::open_guarded_with(
            dir.path().join("b.log"),
            secret.clone(),
            astream_broker::Durability::Relaxed,
        )
        .expect("a guarded broker");
        let socket = dir.path().join("b.sock");
        let _server = broker.serve(&socket).expect("serve the broker");
        let sock = socket.to_string_lossy().into_owned();
        let (mut god, _) = transport::connect(&sock, MUST_HAPPEN).expect("dial");
        let all = astream_cap::mint(&secret, "/f/f1/>").expect("mint the fleet's cap");
        god.attach(&all.filter, &all.tag).expect("attach it");
        let cap = dir.path().join("node.cap");
        let lines: String = crate::enable::node_grants("f1", "n-a")
            .iter()
            .map(|g| {
                let c = astream_cap::mint(&secret, g).expect("mint");
                format!("{}\n", astream_cap::capfile::format_line(&c.filter, &c.tag))
            })
            .collect();
        std::fs::write(&cap, lines).expect("the node's cap");
        let subjects = [
            ("/f/f1/pub/n-b/s-x/presence", true),
            ("/f/f1/in/n-a/s-y/n-b/note", true),
            ("/f/f1/pub/n-a/s-y/presence", true),
            ("/f/f1/term/n-a/s-y/screen", true),
            ("/f/f1/in/n-b/s-z/n-c/note", false),
        ];
        let mut seq = 0u64;
        let mut publish = |i: usize| {
            seq += 1;
            let (subject, read) = subjects[i % subjects.len()];
            let body = format!("v=1 t={} state=live", 1_700_000_000_000 + seq);
            let off = god
                .publish(7, seq, subject, body.as_bytes())
                .expect("lands")
                .0;
            (off, read)
        };
        let readable: Vec<u64> = (0..250)
            .map(&mut publish)
            .filter_map(|(off, read)| read.then_some(off))
            .collect();
        let cfg = crate::cli::parse(&[
            "--fleet".to_string(),
            "f1".to_string(),
            "--broker".to_string(),
            sock,
            "--cap-file".to_string(),
            cap.to_string_lossy().into_owned(),
        ])
        .expect("a config")
        .cfg;
        let json = TailFmt::new(false, true);

        let stop = 100;
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let mut out = Sent {
            tx,
            buf: Vec::new(),
            lines: stop,
        };
        let from_zero = cfg.clone();
        std::thread::spawn(move || {
            let ended = tail(&from_zero, json, Some(0), None, &mut out);
            let _ = done_tx.send(ended.map_err(|e| e.to_string()));
        });
        let mut first = Vec::new();
        for _ in 0..stop {
            let line = rx
                .recv_timeout(MUST_HAPPEN)
                .unwrap_or_else(|e| panic!("the first tail's lines ({first:?}): {e}"));
            first.extend(tail_off(&line));
        }
        assert_eq!(
            done_rx
                .recv_timeout(MUST_HAPPEN)
                .expect("the first tail ends"),
            Ok(()),
            "it ends when its reader does"
        );
        assert!(
            first.windows(2).all(|w| w[0] < w[1]),
            "offsets strictly increasing: {first:?}"
        );
        assert_eq!(first.as_slice(), &readable[..stop], "the first {stop}");

        let live = 4;
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let resume = first.last().map_or(0, |off| off + 1);
        let mut out = Sent {
            tx,
            buf: Vec::new(),
            lines: readable.len() - stop + live,
        };
        let resumed = cfg.clone();
        std::thread::spawn(move || {
            let ended = tail(&resumed, json, Some(resume), None, &mut out);
            let _ = done_tx.send(ended.map_err(|e| e.to_string()));
        });
        let mut rest = Vec::new();
        while rest.len() < readable.len() - stop {
            let line = rx
                .recv_timeout(MUST_HAPPEN)
                .unwrap_or_else(|e| panic!("the resumed tail's backlog ({rest:?}): {e}"));
            rest.extend(tail_off(&line));
        }
        assert!(
            rest.windows(2).all(|w| w[0] < w[1]),
            "offsets strictly increasing: {rest:?}"
        );
        assert_eq!(
            [first.as_slice(), rest.as_slice()].concat(),
            readable,
            "the two runs read every record the node reads, once: no hole"
        );
        for i in 0..live {
            let (off, _) = publish(i);
            let line = rx
                .recv_timeout(MUST_HAPPEN)
                .unwrap_or_else(|e| panic!("the live record @{off} on face {i}: {e}"));
            assert_eq!(tail_off(&line), Some(off), "{line}");
        }
        publish(0);
        assert_eq!(
            done_rx.recv_timeout(MUST_HAPPEN).expect("the tail ends"),
            Ok(()),
            "it ends when its reader does"
        );
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
        assert_eq!(
            procs[1],
            Proc {
                pid: 48078,
                ppid: 1,
                cmd: "/Users//u/.local/bin/aterm link broker /Users//u/f/bus.sock /Users//u/f/bus.log"
                    .to_string()
            }
        );
        assert_eq!(
            (procs[6].pid, procs[6].ppid, procs[6].cmd.as_str()),
            (
                700,
                1,
                "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm link broker /Users//u/g/bus.sock /Users//u/g/bus.log"
            ),
            "a command keeps its spaces"
        );
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
            overdue_unjudged: 0,
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

    /// The renewal's health read: a broker answers when its `Hello` comes back, in this protocol
    /// or another; a socket nobody holds, or one that accepts and says nothing, does not.
    /// NEGATIVE CONTROLS: an `answers` that only connects passes the silent socket; one with no
    /// protocol arm fails the broker of another protocol.
    #[cfg(unix)]
    #[test]
    fn a_broker_answers_when_its_hello_comes_back() {
        use astream_broker::proto::PROTO_VERSION;
        use astream_broker::proto::{encode_response, read_frame, write_frame, Response};
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-answers")
            .tempdir()
            .expect("scratch");
        let endpoint = |name: &str| dir.path().join(name).to_string_lossy().into_owned();

        let why = answers(&endpoint("none.sock")).expect_err("nobody holds it");
        assert!(why.starts_with("connect: "), "{why}");

        let silent =
            std::os::unix::net::UnixListener::bind(dir.path().join("silent.sock")).expect("bind");
        let closer = std::thread::spawn(move || drop(silent.accept()));
        let why = answers(&endpoint("silent.sock")).expect_err("accepted, said nothing");
        assert!(
            why.starts_with("the socket accepted but no broker answered"),
            "{why}"
        );
        closer.join().expect("the listener");

        let other =
            std::os::unix::net::UnixListener::bind(dir.path().join("other.sock")).expect("bind");
        let replier = std::thread::spawn(move || {
            let (mut c, _) = other.accept().expect("accept");
            if let Ok(Some(_hello)) = read_frame(&mut c) {
                let mut reply = encode_response(&Response::Error {
                    code: astream_broker::proto::ERR_REFUSED,
                    msg: "malformed request".to_string(),
                });
                reply[0] = PROTO_VERSION + 1;
                let _ = write_frame(&mut c, &reply);
            }
        });
        assert_eq!(answers(&endpoint("other.sock")), Ok(()));
        replier.join().expect("the replier");

        let broker = crate::test_broker(dir.path().join("b.log")).expect("broker");
        let mut server = broker.serve(dir.path().join("b.sock")).expect("serve");
        assert_eq!(answers(&endpoint("b.sock")), Ok(()));
        server.shutdown();
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
            let (view, _) = reach_broker(&unix_config(&sock));
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

    #[test]
    fn a_withheld_fabric_row_says_why_and_probes_nothing() {
        let absent = std::env::temp_dir().join(format!("atl-withheld-{}", std::process::id()));
        let bridge = words(&format!(
            "aterm link serve --fleet lab --broker {b} --cap-file /c --state {s}",
            b = absent.join("b.sock").display(),
            s = absent.join("state").display()
        ));
        let unread = |_: &Config| -> BrokerView { panic!("the broker was probed while withheld") };
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
        let probed = summary_for(Ok(Some(bridge)), None, |cfg| {
            read.set(true);
            reach_broker(cfg).0
        });
        assert!(read.get(), "CONTROL: nothing withheld probes the broker");
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
        let (view, _) = reach_broker(&cfg);
        assert_eq!(view.last_word, None, "nothing was written");
        std::fs::write(
            dir.path().join("broker.err"),
            "aterm link broker: open bus.log: earlier\n\
             aterm link broker: open bus.log: record format v9: refused\n",
        )
        .expect("broker.err");
        let (view, _) = reach_broker(&cfg);
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
            let (view, _) = reach_broker(&cfg);
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
            let (view, _) = reach_broker(&cfg);
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
        let expected = format!("aterm -c read x link broker {sock} a\\012b");
        let started = std::time::Instant::now();
        while !process_table()
            .iter()
            .any(|p| p.pid == fixture.id() && p.cmd == expected)
        {
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
        assert_eq!(a[&id].cmd, expected);
        assert_eq!(
            oracle.iter().find(|p| p.pid == id).map(|p| p.cmd.as_str()),
            Some(expected.as_str()),
            "ps prints the fixture's row as the table does"
        );
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

    /// R5: WHAT THE BROKER'S RETENTION GAVE UP IS A WARNING, ONCE WHERE IT WAS TOLD. The fields
    /// parse as aterm writes them (`inbox`'s `given_up=`, `given_up_count=`, `lane_given_up=`; a
    /// `topic ls` row's `given_up=`; `fabric status`'s `mail_gaps=`, `mail_gap=`,
    /// `mail_gap_count=`). The node's unread mail is one line per instance, from its counter
    /// alone (a range no session was there to be told of, or one on a log since replaced), from
    /// its sessions alone (after a seamless update, which carries theirs and not the counter), or
    /// both, each range once and newest first, the fourth past counted; a session's own lane
    /// and topics are one line for that session, never its copy of the node's range. Each gets
    /// `doctor`'s retention fix. NC (run): a `node_mail_given_up` that returns `None` fails the
    /// counter's line; one that takes no session's range fails the update's; a
    /// `session_given_up` that returns `None` fails the session's line; one that keeps each
    /// session's copy of a range apart fails "once for both".
    #[test]
    fn records_the_brokers_retention_gave_up_are_listed_once_where_they_were_told() {
        let mut s = LocalSession::default();
        fill_inbox(
            &mut s,
            "OK 0 hold=0 holder=- seen=3 bus_head=90 oldest_on_bus=@44 dropped=0 pending=0 \
             given_up=@10..@42 given_up_count=3 lane_given_up=@50..@60",
            &[],
            &[],
        );
        let range = |from, to, records| GivenUpRange { from, to, records };
        assert_eq!(s.given_up, Some(range(10, 42, Some(3))));
        assert_eq!(s.lane_given_up, Some(range(50, 60, None)));
        fill_topics(
            &mut s,
            &[
                "topic deploy since=@0 given_up=@70..@80".to_string(),
                "topic news since=head".to_string(),
            ],
        );
        assert_eq!(s.topics, ["deploy", "news"]);
        assert_eq!(
            s.topic_given_up,
            [("deploy".to_string(), range(70, 80, None))]
        );
        for bad in ["@5..@3", "10..42", "@a..@b", "@1..", ""] {
            assert_eq!(GivenUpRange::parse(bad, Some("2")), None, "{bad}");
        }
        assert_eq!(
            GivenUpRange::parse("@1..@2", Some("x")),
            Some(range(1, 2, None))
        );

        let node_lines = |r: &Report| -> Vec<String> {
            warned(r)
                .into_iter()
                .filter(|l| l.contains("this node's unread mail"))
                .collect()
        };
        let fix = |l: &str| crate::enable::fix_for(l, &crate::enable::Role::default());

        let mut r = healthy();
        r.instances[0].mail_gaps = Some(2);
        r.instances[0].mail_gap = Some(range(70, 75, Some(1)));
        assert_eq!(
            warned(&r),
            [
                "instance 100: the broker's retention gave up this node's unread mail before its \
              bridge read it (2 range(s) since the instance started): @70..@75 (1 record); the \
              sessions it served then were told (`inbox` given_up=, a `mail-gap` mark in each \
              log)"
            ],
            "the counter alone, no session told"
        );
        assert!(fix(&warned(&r)[0]).starts_with("nothing brings those records back"));
        r.instances[0].mail_gap = None;
        assert_eq!(
            node_lines(&r),
            [
                "instance 100: the broker's retention gave up this node's unread mail before its \
              bridge read it (2 range(s) since the instance started), its offsets on a bus log \
              since replaced; the sessions it served then were told (`inbox` given_up=, a \
              `mail-gap` mark in each log)"
            ]
        );
        r.instances[0].mail_gaps = Some(0);
        assert_eq!(warned(&r), Vec::<String>::new(), "none told");

        let mut r = healthy();
        let mut two = r.instances[0].sessions[0].clone();
        two.sid = "s-two".to_string();
        r.instances[0].sessions.push(two);
        for s in &mut r.instances[0].sessions {
            s.given_up = Some(range(10, 42, Some(3)));
        }
        assert_eq!(
            warned(&r),
            [
                "instance 100: the broker's retention gave up this node's unread mail before its \
              bridge read it: @10..@42 (3 records); the sessions it served then were told \
              (`inbox` given_up=, a `mail-gap` mark in each log)"
            ],
            "the sessions' range after an update, once for both"
        );
        r.instances[0].mail_gaps = Some(2);
        r.instances[0].mail_gap = Some(range(100, 120, None));
        r.instances[0].sessions[1].given_up = Some(range(100, 120, Some(2)));
        assert_eq!(
            node_lines(&r),
            [
                "instance 100: the broker's retention gave up this node's unread mail before its \
              bridge read it (2 range(s) since the instance started): @100..@120 (2 records), \
              @10..@42 (3 records); the sessions it served then were told (`inbox` given_up=, a \
              `mail-gap` mark in each log)"
            ],
            "the counter's and the sessions', each range once, newest first"
        );
        for (i, from) in [130, 140, 150, 160].into_iter().enumerate() {
            let mut s = r.instances[0].sessions[0].clone();
            s.sid = format!("s-more{i}");
            s.given_up = Some(range(from, from + 5, None));
            r.instances[0].sessions.push(s);
        }
        let lines = node_lines(&r);
        assert!(
            lines.len() == 1
                && lines[0]
                    .contains(": @160..@165, @150..@155, @140..@145, @130..@135 and 2 more;"),
            "{lines:#?}"
        );

        let mut r = healthy();
        let s = &mut r.instances[0].sessions[0];
        s.given_up = Some(range(10, 42, Some(3)));
        s.lane_given_up = Some(range(50, 60, None));
        s.topic_given_up = vec![
            ("deploy".to_string(), range(70, 80, None)),
            ("news".to_string(), range(81, 90, Some(4))),
        ];
        let w = warned(&r);
        let own: Vec<&String> = w.iter().filter(|l| l.starts_with("@s-one ")).collect();
        assert_eq!(
            own,
            ["@s-one (instance 100): the broker's retention gave up records of its own before it \
              handled them: its mail lane @50..@60 (`inbox` lane_given_up=); topic deploy \
              @70..@80, topic news @81..@90 (4 records) (`topic ls` given_up=)"],
            "{w:#?}"
        );
        assert!(fix(own[0]).starts_with("nothing brings those records back"));
        assert_eq!(w.len(), 2, "the node's range is its own line: {w:#?}");
        r.instances[0].sessions[0].lane_given_up = None;
        one(&r, "@s-one (instance 100): the broker's retention gave up records of its own before it handled them: topic deploy");
        r.instances[0].sessions[0].topic_given_up.clear();
        assert_eq!(warned(&r).len(), 1, "only the node's: {:#?}", warned(&r));
    }

    /// R5 review: OVERLAPPING RANGES OF THE NODE'S GIVEN-UP MAIL ARE ONE LOSS. A later notice on
    /// one subscription can overlap an earlier one (astream's `Gap::and`, whose counts sum): the
    /// instance's counter widens its newest over both (`fabric status` `mail_gap=@4..@12`, no
    /// count) while each session keeps the later (`given_up=@6..@12 given_up_count=1`). The
    /// warning merges the counter's and the sessions' ranges that overlap, keeps a count only
    /// where the range it counted is the whole merged one, keeps ranges that only touch apart,
    /// and lists no empty range. NC (run): a `node_mail_given_up` keyed by the exact range says
    /// `@6..@12 (1 record), @4..@12` for one range.
    #[test]
    fn overlapping_ranges_of_the_nodes_given_up_mail_are_listed_as_one() {
        let range = |from, to, records| GivenUpRange { from, to, records };
        let node_lines = |r: &Report| -> Vec<String> {
            warned(r)
                .into_iter()
                .filter(|l| l.contains("this node's unread mail"))
                .collect()
        };
        let said = |since: &str, ranges: &str| {
            vec![format!(
                "instance 100: the broker's retention gave up this node's unread mail before its \
                 bridge read it{since}: {ranges}; the sessions it served then were told (`inbox` \
                 given_up=, a `mail-gap` mark in each log)"
            )]
        };
        let one_told = " (1 range(s) since the instance started)";

        let mut r = healthy();
        r.instances[0].mail_gaps = Some(1);
        r.instances[0].mail_gap = Some(range(4, 12, None));
        r.instances[0].sessions[0].given_up = Some(range(6, 12, Some(1)));
        assert_eq!(
            node_lines(&r),
            said(one_told, "@4..@12"),
            "one range; the later notice's count is of part of it"
        );
        r.instances[0].mail_gap = Some(range(4, 12, Some(3)));
        assert_eq!(
            node_lines(&r),
            said(one_told, "@4..@12 (3 records)"),
            "a count of the whole merged range stays"
        );

        let mut r = healthy();
        let mut two = r.instances[0].sessions[0].clone();
        two.sid = "s-two".to_string();
        r.instances[0].sessions.push(two);
        r.instances[0].sessions[0].given_up = Some(range(10, 42, Some(3)));
        r.instances[0].sessions[1].given_up = Some(range(30, 50, Some(2)));
        assert_eq!(
            node_lines(&r),
            said("", "@10..@50"),
            "the sessions' alone, after an update"
        );
        let mut three = r.instances[0].sessions[0].clone();
        three.sid = "s-three".to_string();
        three.given_up = Some(range(50, 60, Some(1)));
        r.instances[0].sessions.push(three);
        let mut four = r.instances[0].sessions[0].clone();
        four.sid = "s-four".to_string();
        four.given_up = Some(range(60, 60, None));
        r.instances[0].sessions.push(four);
        assert_eq!(
            node_lines(&r),
            said("", "@50..@60 (1 record), @10..@50"),
            "ranges that only touch stay apart, and an empty one is no loss"
        );
    }

    /// A bus where retention holds overdue asks below the `in` lanes' oldest and gave up the
    /// verdicts of some ([`held_ask_bus`]).
    #[cfg(unix)]
    struct HeldAsk {
        _server: astream_broker::BrokerHandle,
        _broker: astream_broker::Broker,
        _dir: aterm_tempfile::TempDir,
        path: String,
        /// The ask nothing answered or expired.
        ask: u64,
        /// The ask its asker's bridge expired, the `expired` given up.
        lapsed: u64,
        /// The ask the person answered, the answer given up.
        answered: u64,
        /// Each ask's `t=`.
        t: u64,
        /// When their deadlines are ten minutes past.
        now: u64,
        /// The bus head after the cut.
        head: u64,
        /// The node cap's faces (`enable::node_grants`), its group grant left out as
        /// `readable_faces` leaves it.
        node_faces: Vec<String>,
    }

    #[cfg(unix)]
    impl HeldAsk {
        fn dial(&self) -> Conn {
            transport::connect(&self.path, aterm_spec::wait::MUST_HAPPEN)
                .expect("connect")
                .0
        }
    }

    /// RETENTION HOLDS OVERDUE ASKS BELOW THE `in` LANES' OLDEST (audit finding BUS-1, on one
    /// node): `s-one@n-a` asks the person `h-jsmith` three times with a deadline of 10 min, 20
    /// min before `now`; the person lanes' reader group (`/f/lab/cur/p/node/inbox`) is one
    /// nothing commits, so retention holds the asks. On `s-one`'s own lane, the person answers
    /// the first (`answered`) and `s-one`'s bridge records the second expired (`lapsed`); the
    /// third (`ask`) gets neither. Last, a mail between two of the node's sessions. The node's
    /// inbox group reads all three, and the bulk stream's cut (aterm's rules, its bytes
    /// scaled) gives them up past the asks, which raises their subjects' floors, and so the
    /// oldest of the whole fleet's `in` lanes (`/f/lab/in/>`) and of the node cap's
    /// `in/*/*/n-a/*`, above every ask. CONTROL (each filter): read on at its floor
    /// (`transport::fetch_kept`, every read status made before audit fix unit F7), it misses
    /// the asks.
    #[cfg(unix)]
    fn held_ask_bus(tag: &str) -> HeldAsk {
        const BODY: u64 = 4096;
        let dir = aterm_tempfile::Builder::new()
            .prefix(tag)
            .tempdir_in("/tmp")
            .expect("scratch");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        broker.set_retention_interval(Duration::from_secs(3600));
        broker
            .set_retention(crate::broker_rules::rules_scaled(
                3 * BODY / 2,
                1 << 30,
                1 << 30,
            ))
            .expect("aterm's rules, scaled");
        let server = broker.serve(&socket).expect("serve the broker");
        let path = socket.to_string_lossy().into_owned();
        let (mut writer, _) =
            transport::connect(&path, aterm_spec::wait::MUST_HAPPEN).expect("connect");
        let t = 1_000_000_u64;
        let now = t + 20 * 60 * 1000;
        let mut seq = 0u64;
        let mut publish = |subject: &str, body: &str| {
            seq += 1;
            writer
                .publish(91, seq, subject, body.as_bytes())
                .unwrap_or_else(|e| panic!("publish on {subject}: {e}"))
                .0
        };
        let to_person = format!(
            "{}/ask",
            crate::subject::person_lane("lab", "h-jsmith", "n-a")
        );
        let asked = format!("v=1 t={t} from=s-one dl=600000 text=which");
        let answered = publish(&to_person, &asked);
        publish(
            &crate::subject::post_lane("lab", "n-a", "s-one", "h-jsmith", "answer"),
            &format!("v=1 t=1 re={answered} text=yes"),
        );
        let lapsed = publish(&to_person, &asked);
        publish(
            &crate::subject::post_lane("lab", "n-a", "s-one", "n-a", "expired"),
            &format!("v=1 t=1 re={lapsed} dl=600000 text=expired"),
        );
        let ask = publish(&to_person, &asked);
        let read = publish(
            &crate::subject::post_lane("lab", "n-a", "s-two", "n-a", "note"),
            "v=1 t=1 from=s-one text=fyi",
        );
        let filler = "e".repeat(BODY as usize);
        for _ in 0..3 {
            publish("/f/lab/pub/n-a/s-one/ev", &filler);
        }
        writer
            .commit(&crate::subject::inbox_group("lab", "n-a"), read)
            .expect("the node's inbox group reads the answer, the verdict and the mail");
        broker
            .retention_tick_at(broker.now_ms())
            .expect("the retention tick");
        let (_, (_, head)) = writer.fetch(0, "/f/lab/>", 0).expect("the head");
        let node_faces: Vec<String> = crate::enable::node_grants("lab", "n-a")
            .iter()
            .filter_map(|g| astream_cap::Grant::parse(g).ok())
            .map(|g| g.filter)
            .filter(|f| !f.starts_with("/f/lab/cur/"))
            .collect();
        for filter in ["/f/lab/in/>", "/f/lab/in/*/*/n-a/*"] {
            let of = astream_wire::Filter::new(filter).expect("a filter");
            assert!(
                broker.oldest(&of) > ask,
                "PRECONDITION: {filter}'s oldest lies above the asks retention holds"
            );
            let ((rows, _), floor) =
                transport::fetch_kept(&mut writer, 0, filter, transport::POST_PAGE)
                    .expect("a read on at the floor");
            assert!(
                floor.is_some() && rows.iter().all(|(off, _, _)| *off > ask),
                "CONTROL: {filter} read on at its floor misses the held asks: {floor:?} {rows:?}"
            );
        }
        HeldAsk {
            _server: server,
            _broker: broker,
            _dir: dir,
            path,
            ask,
            lapsed,
            answered,
            t,
            now,
            head,
            node_faces,
        }
    }

    /// `aterm fabric status` LISTS AN ASK RETENTION HOLDS BELOW THE `in` LANES' OLDEST, JUDGED ON
    /// THE VERDICTS THE BUS KEEPS (audit fix unit F7, C12, finding BUS-1; [`held_ask_bus`]):
    /// read told, the deadline scan over the whole fleet's `in` lanes and over the node cap's
    /// `in` faces both list the ask nothing answered or expired (`expired` `Some(false)`), and
    /// the one whose `expired` retention gave up with `expired` `None` (`null` in `--json`),
    /// and leave out the one whose answer retention gave up: status cannot say it had none, and
    /// counts it unjudged (audit finding BUS2-3).
    /// NEGATIVE CONTROLS (run): `overdue_work` reading through `transport::fetch_kept` lists
    /// nothing; skipping its check of the asker's answers lists the answered ask; skipping its
    /// check of the asker's `expired` verdicts says `Some(false)` for the lapsed one.
    #[cfg(unix)]
    #[test]
    fn an_ask_held_below_the_in_lanes_oldest_is_listed_overdue() {
        let bus = held_ask_bus("al-held-ask");
        assert!(bus.answered < bus.lapsed && bus.lapsed < bus.ask);
        for faces in [None, Some(bus.node_faces.as_slice())] {
            let found = overdue_work(&mut bus.dial(), "lab", faces, bus.head, bus.now)
                .expect("the deadlines");
            assert_eq!(
                found
                    .overdue
                    .iter()
                    .map(|o| (o.off, o.expired))
                    .collect::<Vec<_>>(),
                vec![(bus.lapsed, None), (bus.ask, Some(false))],
                "{faces:?}: {found:#?}"
            );
            assert_eq!(found.unjudged, 1, "{faces:?}: the answered ask");
            let mut r = healthy();
            r.overdue = found.overdue;
            let json = render_json(&r);
            assert!(
                json.contains(&format!("{{\"off\":{},", bus.lapsed))
                    && json.contains("\"expired\":null"),
                "{json}"
            );
        }
    }

    /// AN ASK STATUS CANNOT JUDGE IS COUNTED, NEVER LISTED (audit finding BUS2-3): `s-one@n-a`
    /// asks the person `h-jsmith` with a deadline that has passed (`ask`), then again with one
    /// that has not (`later`), and the person answers `later` on `s-one`'s lane. The node's
    /// inbox group reads the answer and a mail, and the bulk stream's cut (aterm's rules, its
    /// bytes scaled) gives them up, which raises the floor of `s-one`'s answers above `ask`: the
    /// bus no longer says whether `ask` had one. Over the whole fleet's `in` lanes and over the
    /// node cap's faces, the deadline scan lists nothing and counts `ask` unjudged, and status
    /// says so in one warning (exit 1), which `doctor` gives its own fix, and as
    /// `overdue_unjudged` in `--json`.
    /// NEGATIVE CONTROLS (run): `overdue_work`'s `continue` without the count gives 0 and no
    /// warning; `fix_for` without the warning's branch gives it the catch-all.
    #[cfg(unix)]
    #[test]
    fn an_ask_whose_askers_later_answer_was_given_up_is_counted_unjudged() {
        const BODY: u64 = 4096;
        let dir = aterm_tempfile::Builder::new()
            .prefix("al-unjudged")
            .tempdir_in("/tmp")
            .expect("scratch");
        let socket = dir.path().join("b.sock");
        let broker = crate::test_broker(dir.path().join("b.log")).expect("a broker");
        broker.set_retention_interval(Duration::from_secs(3600));
        broker
            .set_retention(crate::broker_rules::rules_scaled(
                3 * BODY / 2,
                1 << 30,
                1 << 30,
            ))
            .expect("aterm's rules, scaled");
        let _server = broker.serve(&socket).expect("serve the broker");
        let path = socket.to_string_lossy().into_owned();
        let dial = || {
            transport::connect(&path, aterm_spec::wait::MUST_HAPPEN)
                .expect("connect")
                .0
        };
        let mut writer = dial();
        let t = 1_000_000_u64;
        let now = t + 20 * 60 * 1000;
        let mut seq = 0u64;
        let mut publish = |subject: &str, body: &str| {
            seq += 1;
            writer
                .publish(91, seq, subject, body.as_bytes())
                .unwrap_or_else(|e| panic!("publish on {subject}: {e}"))
                .0
        };
        let to_person = format!(
            "{}/ask",
            crate::subject::person_lane("lab", "h-jsmith", "n-a")
        );
        let ask = publish(
            &to_person,
            &format!("v=1 t={t} from=s-one dl=600000 text=which"),
        );
        let later = publish(
            &to_person,
            &format!("v=1 t={now} from=s-one dl=600000 text=which"),
        );
        publish(
            &crate::subject::post_lane("lab", "n-a", "s-one", "h-jsmith", "answer"),
            &format!("v=1 t=1 re={later} text=yes"),
        );
        let read = publish(
            &crate::subject::post_lane("lab", "n-a", "s-two", "n-a", "note"),
            "v=1 t=1 from=s-one text=fyi",
        );
        let filler = "e".repeat(BODY as usize);
        for _ in 0..3 {
            publish("/f/lab/pub/n-a/s-one/ev", &filler);
        }
        writer
            .commit(&crate::subject::inbox_group("lab", "n-a"), read)
            .expect("the node's inbox group reads the answer and the mail");
        broker
            .retention_tick_at(broker.now_ms())
            .expect("the retention tick");
        let (_, (_, head)) = writer.fetch(0, "/f/lab/>", 0).expect("the head");
        let lane = verdict_lane("lab", &to_person, Some("s-one"));
        let (floor, _) = writer
            .oldest(&format!("{lane}/answer"))
            .expect("the answers' oldest");
        assert!(
            floor.is_some_and(|floor| floor > ask + 1),
            "PRECONDITION: the cut gave up the answer above the ask: {floor:?}"
        );
        let node_faces: Vec<String> = crate::enable::node_grants("lab", "n-a")
            .iter()
            .filter_map(|g| astream_cap::Grant::parse(g).ok())
            .map(|g| g.filter)
            .filter(|f| !f.starts_with("/f/lab/cur/"))
            .collect();
        for faces in [None, Some(node_faces.as_slice())] {
            let found = overdue_work(&mut dial(), "lab", faces, head, now).expect("the deadlines");
            assert_eq!(
                (found.overdue.len(), found.unjudged),
                (0, 1),
                "{faces:?}: {found:#?}"
            );
            let mut r = healthy();
            r.overdue = found.overdue;
            r.overdue_unjudged = found.unjudged;
            r.warnings = warned(&r);
            assert_eq!(r.warnings.len(), 1, "{faces:?}: {:?}", r.warnings);
            let role = crate::enable::Role::default();
            assert_ne!(
                crate::enable::fix_for(&r.warnings[0], &role),
                crate::enable::fix_for("a warning no remedy names", &role),
                "doctor gives the warning its own fix: {}",
                r.warnings[0]
            );
            assert_eq!(exit_of(&r), 1, "{faces:?}");
            let json: aterm_json::Value = aterm_json::from_str(&render_json(&r)).expect("JSON");
            assert_eq!(json["overdue_unjudged"].as_u64(), Some(1), "{faces:?}");
            assert_eq!(json["overdue"].as_array().map(Vec::len), Some(0));
        }
    }

    /// THE LANE AN ASK'S VERDICTS LAND ON, AND THE FACES THAT READ IT (`verdict_lane`,
    /// `filter_meet`): a session's ask (to a person or a session) is answered on its own lane,
    /// a person's on the person's lane, an asker the record does not name on every lane; the
    /// meet of two filters matches exactly what both match. NEGATIVE CONTROL (run): a meet
    /// that takes the first filter's `*` over the second's literal fails it.
    #[test]
    fn an_asks_verdicts_are_read_on_its_askers_lane() {
        let lane = |subject: &str, from: Option<&str>| verdict_lane("lab", subject, from);
        assert_eq!(
            lane("/f/lab/in/p/h-jsmith/n-a/ask", Some("s-one")),
            "/f/lab/in/n-a/s-one/*"
        );
        assert_eq!(
            lane("/f/lab/in/n-b/s-two/n-a/ask", Some("s-one")),
            "/f/lab/in/n-a/s-one/*"
        );
        assert_eq!(
            lane("/f/lab/in/n-a/s-one/h-jsmith/ask", None),
            "/f/lab/in/p/h-jsmith/*"
        );
        for (subject, from) in [
            ("/f/lab/in/n-b/s-two/n-a/ask", None),
            ("/f/lab/in/n-b/s-two/n-a/ask", Some("s-Bad")),
            ("/f/lab/in/n-b/s-two/s-one/ask", Some("s-one")),
        ] {
            assert_eq!(lane(subject, from), "/f/lab/in/*/*/*", "{subject} {from:?}");
        }
        let verdicts = "/f/lab/in/n-b/s-two/*/answer";
        assert_eq!(
            filter_meet(verdicts, "/f/lab/in/>").as_deref(),
            Some(verdicts)
        );
        assert_eq!(
            filter_meet(verdicts, "/f/lab/in/*/*/n-a/*").as_deref(),
            Some("/f/lab/in/n-b/s-two/n-a/answer")
        );
        assert_eq!(
            filter_meet("/f/lab/in/*/*/n-a/*", verdicts).as_deref(),
            Some("/f/lab/in/n-b/s-two/n-a/answer")
        );
        assert_eq!(filter_meet(verdicts, "/f/lab/in/n-a/>"), None);
        assert_eq!(filter_meet(verdicts, "/f/lab/in/*/*/*"), None);
        assert_eq!(
            filter_meet("/f/lab/in/>", "/f/lab/>").as_deref(),
            Some("/f/lab/in/>")
        );
    }

    /// A HELD RECORD BELOW ITS FILTER'S OLDEST HAS ITS AGE (`record_t`, [`held_ask_bus`]): read
    /// told on a connection that asked for nothing before, the record at the ask's offset is
    /// found on the whole fleet's `in` lanes and on the node cap's first `in` face. NEGATIVE
    /// CONTROL (run): `record_t` reading with a plain `fetch` gets no age.
    #[cfg(unix)]
    #[test]
    fn a_record_held_below_the_in_lanes_oldest_has_its_age() {
        let bus = held_ask_bus("al-held-age");
        for faces in [None, Some(bus.node_faces.as_slice())] {
            let mut fresh = bus.dial();
            assert_eq!(
                in_filters("lab", faces)
                    .iter()
                    .find_map(|f| record_t(&mut fresh, f, bus.ask)),
                Some(bus.t),
                "{faces:?}"
            );
        }
    }

    /// TRAFFIC SHOWS A RECORD HELD BELOW THE FLEET'S OLDEST (`last_records`,
    /// [`held_ask_bus`]): the fleet's root reaches the bulk stream's class, so its oldest is the
    /// cut, and read told the newest records include the ask held below it. NEGATIVE CONTROL
    /// (run): `last_records` reading through `transport::fetch_kept` leaves it out.
    #[cfg(unix)]
    #[test]
    fn traffic_shows_a_record_held_below_the_fleets_oldest() {
        let bus = held_ask_bus("al-held-traffic");
        let traffic = last_records(&mut bus.dial(), "lab", None, bus.head, TRAFFIC_ROWS)
            .expect("the traffic");
        assert!(
            traffic.iter().any(|t| t.off == bus.ask && t.kind == "ask"),
            "{traffic:#?}"
        );
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
        let offs: Vec<(u64, Option<bool>)> = overdue.iter().map(|o| (o.off, o.expired)).collect();
        assert_eq!(
            offs,
            vec![(40, Some(false)), (42, Some(true))],
            "{overdue:#?}"
        );
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

    fn words(args: &[&str]) -> Vec<OsString> {
        args.iter().map(|word| OsString::from(*word)).collect()
    }

    #[test]
    fn a_broker_report_uses_the_exact_argv_or_reports_an_unknown_tier() {
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
                |_| Some(71),
                |pid| {
                    assert_eq!(pid, 71);
                    None
                },
                |pid, exe| {
                    assert_eq!(pid, 71);
                    assert_eq!(exe, "/bin/aterm", "argv[0]");
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

    /// Every other read (label, argv, replaced build) is of the holder, and none is made when
    /// nothing holds it.
    #[test]
    fn the_broker_is_the_process_holding_its_listener() {
        let argv = words(&["/opt/aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"]);
        for (held, want) in [(Some(71), Some(71)), (None, None)] {
            let mut view = BrokerView {
                endpoint: "/r/bus.sock".into(),
                ..BrokerView::default()
            };
            let mut sockets = Vec::new();
            let asked = std::cell::RefCell::new(Vec::new());
            view.locate_with(
                |sock| {
                    sockets.push(sock.to_string());
                    held
                },
                |pid| {
                    asked.borrow_mut().push(("label", pid));
                    None
                },
                |pid, _| {
                    asked.borrow_mut().push(("replaced", pid));
                    None
                },
                |pid| {
                    asked.borrow_mut().push(("argv", pid));
                    Some(argv.clone())
                },
            );
            assert_eq!(
                sockets,
                ["/r/bus.sock"],
                "the holder is asked of the endpoint"
            );
            assert_eq!(view.pid, want, "held by {held:?}");
            let wanted: Vec<(&str, u32)> = want
                .map(|pid| vec![("label", pid), ("argv", pid), ("replaced", pid)])
                .unwrap_or_default();
            assert_eq!(asked.take(), wanted, "every other read is of the holder");
            assert_eq!(view.log.is_some(), want.is_some());
        }
    }

    /// The holder's argv names the endpoint by its bytes or its canonical spelling, as the
    /// listener scan names its bound path.
    #[cfg(unix)]
    #[test]
    fn the_holders_socket_word_names_the_endpoint_as_its_listener_does() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let base = dir.path().canonicalize().expect("its own spelling");
        let (real, link) = (base.join("real"), base.join("link"));
        std::fs::create_dir(&real).expect("the real folder");
        std::os::unix::fs::symlink(&real, &link).expect("a link to it");
        let at = |dir: &Path| dir.join("bus.sock").to_string_lossy().into_owned();
        let served = |endpoint: String, word: String| {
            let mut view = BrokerView {
                endpoint,
                ..BrokerView::default()
            };
            let log = format!("{word}.log");
            let argv = words(&["/bin/aterm", "link", "broker", &word, &log]);
            view.locate_with(|_| Some(71), |_| None, |_, _| None, |_| Some(argv.clone()));
            view.log
        };
        assert_eq!(
            served(at(&link), at(&real)),
            Some(PathBuf::from(format!("{}.log", at(&real)))),
            "the endpoint spelled through a link, the broker bound at its folder's own spelling"
        );
        assert!(served(at(&real), at(&real)).is_some(), "the same bytes");
        assert_eq!(
            served(at(&real), at(&link)),
            None,
            "a socket bound through a link is named by that spelling alone"
        );
    }

    /// A table the scan could not read still leaves the one holder it found the broker.
    #[test]
    fn one_holder_is_the_broker_and_two_are_none() {
        let scan = |holders: &[u32]| aterm_uds::holders::HolderScan {
            holders: holders.to_vec(),
            scanned: 9,
            ..aterm_uds::holders::HolderScan::default()
        };
        assert_eq!(the_one_holder(&scan(&[71])), Some(71));
        assert_eq!(the_one_holder(&scan(&[])), None);
        assert_eq!(the_one_holder(&scan(&[71, 72])), None);
        let incomplete = aterm_uds::holders::HolderScan {
            sealed: vec![80],
            unreadable: vec![81],
            ..scan(&[71])
        };
        assert_eq!(the_one_holder(&incomplete), Some(71));
    }

    /// With no argv read, the replaced-build check is not made.
    #[test]
    fn the_replaced_build_check_reads_the_whole_argv0() {
        let spaced = "/a b/aterm";
        let argv = words(&[spaced, "link", "broker", "/r/bus.sock", "/r/bus.log"]);
        for (argv, want) in [
            (Some(argv), Some(spaced)),
            (None, None),
            (Some(Vec::new()), None),
        ] {
            let mut view = BrokerView {
                endpoint: "/r/bus.sock".into(),
                ..BrokerView::default()
            };
            let mut asked = Vec::new();
            view.locate_with(
                |_| Some(72),
                |_| None,
                |pid, exe| {
                    assert_eq!(pid, 72);
                    asked.push(exe.to_string());
                    Some(exe.to_string())
                },
                |_| argv.clone(),
            );
            assert_eq!(
                asked,
                want.into_iter().map(str::to_string).collect::<Vec<_>>()
            );
            assert_eq!(view.replaced.as_deref(), want);
        }
    }

    #[test]
    fn swarm_enabled_false_disables_fabric_command_in_toml() {
        let off =
            "[swarm]\nenabled = false\n[fabric]\ncommand = [\"aterm\", \"link\", \"serve\"]\n";
        assert_eq!(command_in_toml(off), Ok(None));
        // Negative control: `[swarm] enabled = true` resolves the configured command.
        let on = "[swarm]\nenabled = true\n[fabric]\ncommand = [\"aterm\", \"link\", \"serve\"]\n";
        assert_eq!(
            command_in_toml(on),
            Ok(Some(vec![
                "aterm".to_string(),
                "link".to_string(),
                "serve".to_string(),
            ]))
        );
        assert!(command_in_toml("[swarm]\nenabled = 1\n").is_err());
    }
}
