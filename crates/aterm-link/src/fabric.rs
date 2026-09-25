// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric` — the fabric's state on one screen, and `aterm fabric tail`
//! its traffic as it happens.
//!
//! ```text
//! aterm fabric [status] [--json]
//! aterm fabric tail [--bodies] [--from <offset>]
//! aterm fabric on [--dry-run] [--fleet <F>] [--service launchd|systemd|none]
//!                 [--tcp <host:port> --key-file <path> [--allow-remote]]
//! aterm fabric off [--dry-run] [--service launchd|systemd|none]
//! aterm fabric doctor
//! aterm fabric mint-for <node-id>|new [--out <cap>] [--fleet <F>]
//! aterm fabric join --broker <host:port> --tcp --key-file <k> --cap-file <c> [...]
//! aterm fabric help
//! ```
//!
//! `on`, `off` and `doctor` are [`crate::enable`]: turning the fabric on in
//! one command, proving it, and naming the fix for each warning below.
//! `mint-for` and `join` are [`crate::join`]: a second host joins the fleet
//! over the sealed transport (round 16).
//!
//! THE OWNER'S COMMAND, AND IT TAKES NO ARGUMENTS. Everything it needs is in the
//! `[fabric] command` aterm launches its bridge from: the fleet, the broker, the
//! cap files and the state dir are that command's own flags, parsed by the
//! bridge's own parser ([`crate::cli::parse`]), and `ATERM_FABRIC_COMMAND`
//! overrides the file exactly as it does for the app (`aterm-gui`'s
//! `fabric_launch::configured_command`: env first, then the file, blank means
//! absent). With neither, the rendezvous file `aterm fabric on` wrote beside the
//! instance control sockets ([`crate::enable::Rendezvous`]) is read for the
//! command it mirrors; with none of the three, the report says the fabric is
//! off and where it looked. One more source is read when all are empty: a
//! running instance's bridge — armed by hand (`aterm ctl fabric attach <argv>`)
//! or launched with `$ATERM_FABRIC_COMMAND` set in that instance's environment —
//! is carrying mail all the same, so its command is used and the missing config
//! is a WARNING.
//!
//! ## What `status` reads, and from where
//!
//! * CONFIG — the command and where it came from, its fleet, the node id in its
//!   state dir, and how many grants each cap file holds.
//! * BROKER — a REAL reachability check: a connect, the broker's `Hello`, every
//!   cap attached, and the head query (`Fetch max=0`). A socket FILE is not a
//!   broker. `fabric=connected` IS a statement about one since round 13 — the
//!   bridge reports its own broker link and the endpoint says `connected` only
//!   while that link's last exchange was acked, `stalled` while a bridge is
//!   attached but its link is down (measured 2026-09-12, before that: a bridge
//!   pointed at a socket nothing served reported `connected` all the same) —
//!   but it is the BRIDGE's last observation, as old as `link_age_ms=` says,
//!   and this probe is the report's own, made now.
//!   `stale` is the third case and the one the other two cannot see (round 21):
//!   the endpoint gave the bridge a post and has had nothing back since — no
//!   ack, no landing, no `link` record. A dead helper closes its fds
//!   (`disconnected`) and a dead BROKER is reported within the bridge's own 5 s
//!   ack deadline (`stalled`); a WEDGED helper does neither, and before round 21
//!   its instance read `connected` for as long as it hung. It is not read off
//!   the ack's age alone: a quiet link's age grows without bound by design, so
//!   the clock runs only while an answer is owed.
//!   The pid is the process serving `link broker <socket>` in the process table,
//!   and on macOS the launchd job whose pid that is. THE HEAD QUERY IS PART OF
//!   THE CHECK ([`BrokerView::read_ok`]): a broker that acknowledges the `Hello`
//!   and every `Attach` and then errors the `Fetch` — a cap file whose grants do
//!   not cover `/f/<fleet>/>`, or a broker that dies between the two — is `yes,
//!   but the bus cannot be read: <its own reason>`, a WARNING, and exit 1. It is
//!   not `reachable yes`: with no head there is no roster, no traffic and no
//!   standing halt to report.
//! * BRIDGES — one row per aterm instance on this machine, found by the same
//!   rendezvous walk `aterm ctl instances` makes ([`aterm_ctl::local_instances`])
//!   and asked `fabric status` under the Owner token beside its socket: its
//!   `state=`, and for a `stalled` link the bridge's `reason=`, for a
//!   `connected` one the last acked round trip and its age. The bridge pid is
//!   that instance's child running `link serve`.
//! * SESSIONS — the bus's presence roster (the rows `aterm link ls` prints)
//!   joined by sid with what each local instance says about its own sessions:
//!   `meta` for the title and role, and `inbox --peek --meta` for the inbox
//!   numbers — `--peek` so no row becomes listed, `--meta` so no text is read.
//!   The row's own `detail=` and `phase=` (round 13's presence with meaning:
//!   the running program, and busy | idle | prompt | question | limited |
//!   survey — the server's agent verdict the bridge relays) are the bridge's,
//!   so a remote session shows them too; a local instance's `meta` wins for the
//!   title and role, the bus row fills them in for a remote one.
//!   A HELD session's `timeline` is read for the hold's reason and origin, which
//!   no other read verb reports; an unhandled task's or ask's age is the `t=` of
//!   its record on the bus.
//! * NODES — every node row on the presence roster, as the node published it
//!   (`host=`, `state=`, `fabric=`), with its live-session count, `this` for
//!   the node the command's state dir names, `local` for another instance on
//!   this machine, and `remote` for one only the bus knows — the second host
//!   of round 16. Before round 16 a remote node was a row of BRIDGES, a table
//!   whose header says it is this machine's instances.
//! * TRAFFIC — the last ten records under `/f/<F>/>`, metadata only.
//!
//! A GUARDED BROKER is read through the caps' own faces. A node's ring never
//! grants `/f/<F>/>`, and the broker a second host dials is always guarded
//! (`aterm link broker --tcp`), so on one the whole-fleet reads above are
//! refused `unauthorized`. The head query then asks the first granted face
//! (the head is global), and TRAFFIC, the deadline scan and the task ages read
//! each granted face and merge by offset ([`BrokerView::faces`]) — the report
//! reads what this node may read, and BROKER's `reads` line says so.
//! * WARNINGS — every condition found above that makes `connected` a lie or
//!   loses mail. A `stalled` bridge is one: its instance's mail queues and none
//!   arrives until the link is back. Among them: a node the bus's OWN roster calls `state=gone`
//!   while the instance whose node it is says `fabric=connected` — measured on
//!   this machine 2026-09-14, after a second bridge that had shared the state
//!   dir exited and fired its will, every peer reading the fleet's directory saw
//!   a live node as dead.
//!
//! ## Privacy
//!
//! A message body is never printed unless `tail --bodies` asks for it, and
//! `status` never prints one: its ctl reads are `--meta`, and of a bus record's
//! body it shows only the `t=` and the length. The report writes no file.
//!
//! Every string that came off the bus or out of another process — a subject
//! segment, a title, a principal — goes through [`crate::render::safe`] before it
//! reaches the terminal, so nothing a sender chose can move the cursor, reorder
//! a line or forge a column.
//!
//! ## Exit status
//!
//! `0` healthy; `1` at least one WARNING was printed; `2` the fabric is off, its
//! config cannot be read, or the command line is wrong.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use crate::body::Body;
use crate::bridge::{read_cap_file, Config};
use crate::ctl::Ctl;
use crate::render::{safe, trust_of, SCREEN};
use crate::transport::{self, Conn, Transport};

/// Usage, printed for `help` (exit 0) and after a usage error (exit 2).
pub const USAGE: &str = "\
usage: aterm fabric [status] [--json]
       aterm fabric tail [--bodies] [--from <offset>] [--filter <subject-filter>]
       aterm fabric on  [--dry-run] [--fleet <F>] [--service launchd|systemd|none]
                        [--tcp <host:port> --key-file <path> [--allow-remote]]
       aterm fabric off [--dry-run] [--service launchd|systemd|none]
       aterm fabric doctor [--retire-ghosts [--yes]]
       aterm fabric mint-for <node-id>|new [--out <cap>] [--fleet <F>]
       aterm fabric join --broker <host:port> --tcp --key-file <path> --cap-file <path>
                         [--node <id>] [--accept-from <p>,...] [--dry-run]
                         [--service launchd|systemd|none]
       aterm fabric help

The fabric's state on one screen, and the one command that turns it on. `status`
takes no arguments: the fleet, broker, cap file and state dir are read from the
`[fabric] command` in aterm.toml; $ATERM_FABRIC_COMMAND overrides the file exactly
as it does for aterm itself, and the rendezvous file `on` writes beside the
instance control sockets (fabric.toml) is read when the file has no command.

  status           the default. Seven sections: CONFIG (where the command came from,
                   its fleet, node id and cap-file grants), BROKER (a real connect,
                   attach and head query, plus the broker's pid and launchd job),
                   NODES (every node on the bus's presence roster: host=, state=,
                   fabric= and its live sessions, `this` marking this one and
                   `remote` a node only the bus knows — a joined second host),
                   BRIDGES (every aterm instance on this machine and its bridge),
                   SESSIONS (each session's phase, running program, hold and inbox
                   numbers, the broadcast topics it has opted into, and its node
                   once the fleet has two — a remote node's too, from its
                   presence), TRAFFIC (the last 10 bus records) and
                   WARNINGS (what makes `connected` a lie or loses mail). On a
                   GUARDED broker whose grants do not cover /f/<F>/> (the sealed
                   one always is) every read is made over the faces the cap files
                   grant, and BROKER says which
  --json           status only: the same data as one JSON object
  tail             follow the bus live, one line per record, until Ctrl-C
  --from <offset>  tail only: replay from that bus offset first (default: new
                   records only)
  --bodies         tail only: also print each record's text, after its trust label
  --filter <f>     tail only: follow ONE subject pattern instead of the whole fleet
                   — `*` is one segment and `>` the rest, so
                   `/f/<F>/pub/*/*/say/>` is every broadcast in the fleet. It must
                   name this fleet; what it may READ is still what the cap files
                   grant, and a broker that refuses it says so rather than quietly
                   following the granted faces instead
  on               turn the fabric on, idempotently, and PROVE it: the binary is
                   checked, the root ($ATERM_FABRIC_HOME, default
                   ~/.local/share/aterm-fabric) made 0700, a node id provisioned
                   once, a mint secret and the node's 8 grants minted (as one
                   transaction), the broker installed and started under launchd
                   (macOS) or systemd --user (Linux) and probed for real, `[fabric]
                   command` written to aterm.toml (atomically, previous file kept
                   as .bak), the rendezvous file written 0600, every running aterm
                   instance armed with `fabric attach <argv>`, then a note posted
                   from a session to itself and awaited back through the broker
                   within 5 s; `aterm fabric` status prints last. A second `on`
                   changes nothing and says so per step
  --dry-run        on/off/join only: print every step and touch nothing (join
                   still PROBES the remote broker, and the first attach of a
                   node's cap binds its producer id there: one hidden /a/bind
                   record in the first host's log, which the real join writes)
  --fleet <F>      on and mint-for only: the fleet name (on: default
                   $ATERM_FABRIC_FLEET, else local; mint-for: this host's fleet)
  --service <s>    on/off/join only: launchd | systemd | none — `none` means the
                   broker is kept alive by something else (it must already answer),
                   and for join that no supervisor is touched
  --tcp <host:port>, --key-file <path>
                   on: only in a `sealed` build (a default build refuses them
                   naming the feature). For join, --tcp is a bare flag and both
                   are required (see join). On: serve the broker on the SEALED TCP wire
                   at <host:port> as well as the Unix socket — astream's
                   XChaCha20-Poly1305 record layer under the pre-shared key in
                   <path> (64 hex, 0600; minted there when the file is absent,
                   never replaced when it is not) — and always GUARDED with this
                   root's mint secret. The port is fixed (not 0) and is for the
                   hosts that join: this host's own bridges keep dialing the
                   socket, where no peer can hold the port's 64 handshake slots.
                   The `wire` step proves the port (at the loopback twin of
                   0.0.0.0/[::]), the rendezvous file records it as serves_tcp,
                   and the label, the plist and every idempotence rule are `on`'s
                   own
  --allow-remote   on --tcp only: bind a non-loopback address. The key is ONE secret
                   every host holds — a transport boundary, not a per-host identity
                   — so a broker on the network is refused unless you say so
  off              stop and remove the broker job, remove the `[fabric]` table
                   (previous file kept as .bak) and the rendezvous file — and keep
                   the node id, secret and cap: identity is provisioned, never
                   discarded. The bus log stays; the line says where
  doctor           `status`'s WARNINGS, each with the fix for it, plus whether the
                   rendezvous file is there
                   --retire-ghosts lists every presence row this node advertises LIVE
                   that no local instance hosts and, after a y/N (or --yes), publishes
                   `exited` for each. DRY by default. With a local bridge UP the rows
                   are retired THROUGH it (`aterm ctl fabric retire`, then the bus is
                   read back); with none, this command publishes them itself — and a
                   bridge from 0.91.0 or earlier answers `ERR usage`, which is reported.
  mint-for <node>  on the FIRST host (a `sealed` build): mint that node's 8 grants
                   (the node ring) on this host's fleet under this host's mint
                   secret, which never leaves it and is never printed. <node> is
                   the joining host's id (its <root>/link-state/node) or `new` for
                   a fresh n-<16 hex>; a malformed id, THIS host's own id, and a
                   host that JOINED another host's fleet (its node cap was not
                   minted under its secret) are refused
  --out <cap>      mint-for only: write the cap there, 0600, never over a different
                   file and never through a symlink; an identical one already there
                   is made 0600 (default: the 8 lines on stdout, the guidance on
                   stderr). The last lines are the `join` to run on the other host
  join             on the SECOND host (a `sealed` build): check the key (64 hex,
                   0600) and the cap (0600, exactly one node's 8 grants — the node
                   and the fleet are read from it; a DIFFERENT node id already here
                   is refused), probe the remote broker with them BEFORE writing
                   anything (the handshake, every grant attached, a read — and a
                   node LIVE on the bus that this root never recorded, the same cap
                   joined from another root, is refused), then
                   record the node id, install both as <root>/fleet.key and
                   <root>/node.cap (0600), stop this root's own local broker job if
                   `on` installed one (not under --service none), write `[fabric]
                   command` for the remote broker and the rendezvous file, arm every
                   running instance and PROVE it: a note from a session to itself
                   out through the REMOTE broker and back within 5 s. A second
                   `join` changes nothing and says so
  --broker <h:p>   join only: the first host's broker, as this host reaches it
  --cap-file <c>   join only: this node's cap, from the first host's `mint-for`
  --node <id>      join only: assert this host's node id (must be the cap's)
  --accept-from <p>,...
                   join only: principals whose task arrives as a task, not a note —
                   the first host's node, for a manager there (this node is always
                   listed)

A message body never prints without `tail --bodies`; `status`, `tail` and `doctor`
write nothing to disk. Exit status: 0 healthy, 1 a warning was printed (or the
proof failed), 2 the fabric is off, its config cannot be read, or the command line
is wrong. `tail` exits 1 when the broker cannot be reached or closes the stream.
";

/// The env override for `[fabric] command` — the app's own name for it.
const FABRIC_COMMAND_ENV: &str = "ATERM_FABRIC_COMMAND";

/// How many bus records TRAFFIC shows.
pub const TRAFFIC_ROWS: usize = 10;

/// How far back TRAFFIC will scan for its rows before it settles for fewer.
/// The broker bounds one `Fetch` scan itself; this bounds how many of them.
const TRAFFIC_SCAN_MAX: u64 = 1 << 20;

/// An unhandled `task` or `ask` older than this is a WARNING: somebody was given
/// work and nobody has taken it.
pub const STALE_WORK_MS: u64 = 10 * 60 * 1000;

/// The per-read bound on every socket this report opens. An instance whose
/// control thread is wedged, or a broker that accepted and never answers, must
/// cost the operator a few seconds and a warning — never a hung terminal.
const IO_TIMEOUT: Duration = Duration::from_secs(3);

/// The kinds a session was GIVEN as work — the ones whose age is a warning.
const WORK_KINDS: [&str; 2] = ["task", "ask"];

// ---------------------------------------------------------------------------
// the command line
// ---------------------------------------------------------------------------

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
    /// `aterm fabric on …` ([`crate::enable::on`]).
    On(crate::enable::OnOpts),
    /// `aterm fabric off …` ([`crate::enable::off`]).
    Off(crate::enable::OffOpts),
    /// `aterm fabric doctor [--retire-ghosts [--yes]]`
    /// ([`crate::enable::doctor`]).
    Doctor {
        /// `--retire-ghosts`: publish `exited` for every presence row this node
        /// advertises LIVE that no local instance hosts. DRY unless `yes`.
        retire_ghosts: bool,
        /// `--yes`: skip the y/N confirmation. Only meaningful with
        /// `--retire-ghosts`.
        yes: bool,
    },
    /// `aterm fabric mint-for …` ([`crate::join::mint_for`]).
    MintFor(crate::join::MintForOpts),
    /// `aterm fabric join …` ([`crate::join::join`]).
    Join(crate::join::JoinOpts),
}

/// Parse argv (everything after `fabric`). Every refusal names the word.
///
/// # Errors
///
/// An unknown subcommand or flag, a flag on the wrong subcommand, or a
/// `--from` that is not an offset.
pub fn parse_args(args: &[String]) -> Result<Cmd, String> {
    let (verb, rest): (&str, &[String]) = match args.first().map(String::as_str) {
        None => ("status", &[]),
        Some(v @ ("status" | "tail" | "on" | "off" | "doctor" | "mint-for" | "join")) => {
            (v, &args[1..])
        }
        Some("help" | "-h" | "--help") => return Ok(Cmd::Help),
        Some(flag) if flag.starts_with('-') => ("status", args),
        Some(other) => {
            return Err(format!(
                "unknown subcommand `{}` (status, tail, on, off, doctor, mint-for, join or help)",
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
    let mut mint = crate::join::MintForOpts::default();
    let mut join = crate::join::JoinOpts::default();
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match (verb, flag.as_str()) {
            (_, "-h" | "--help") => return Ok(Cmd::Help),
            // `mint-for`: one positional, the node id (or `new`), and two flags.
            ("mint-for", "--out") => mint.out = Some(value()?),
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
                if !mint.node.is_empty() {
                    return Err(format!(
                        "mint-for takes ONE node id; `{}` is a second",
                        safe(word, 64)
                    ));
                }
                mint.node = word.to_string();
            }
            // `join`: every flag names the files copied from the first host.
            ("join", "--broker") => join.broker = Some(value()?),
            ("join", "--tcp") => join.tcp = true,
            ("join", "--key-file") => join.key_file = Some(value()?),
            ("join", "--cap-file") => join.cap_file = Some(value()?),
            ("join", "--node") => join.node = Some(value()?),
            ("join", "--accept-from") => join.accept_from.extend(
                value()?
                    .split(',')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
            ),
            ("join", "--dry-run") => join.dry_run = true,
            ("join", "--service") => join.service = Some(crate::enable::Service::parse(&value()?)?),
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
            ("on", "--tcp") => on.tcp = Some(value()?),
            ("on", "--key-file") => on.key_file = Some(value()?),
            ("on", "--allow-remote") => on.allow_remote = true,
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
                return Err(format!(
                    "{f} is an `on`/`off`/`join` flag (`aterm fabric on {f}`)"
                ));
            }
            (_, f @ ("--tcp" | "--key-file")) => {
                return Err(format!(
                    "{f} is an `on` or `join` flag (`aterm fabric on --tcp <bind> --key-file <k>`)"
                ));
            }
            (_, "--fleet") => {
                return Err(
                    "--fleet is an `on` or `mint-for` flag (`aterm fabric on --fleet <F>`)"
                        .to_string(),
                );
            }
            (_, "--allow-remote") => {
                return Err("--allow-remote is an `on` flag, with --tcp".to_string());
            }
            (_, f @ ("--broker" | "--cap-file" | "--node" | "--accept-from")) => {
                return Err(format!("{f} is a `join` flag (`aterm fabric join {f} …`)"));
            }
            (_, "--out") => {
                return Err(
                    "--out is a `mint-for` flag (`aterm fabric mint-for <node> --out <cap>`)"
                        .to_string(),
                );
            }
            (_, other) => return Err(format!("unknown flag {}", safe(other, 64))),
        }
    }
    if verb == "mint-for" && mint.node.is_empty() {
        return Err(format!(
            "mint-for needs a node id: the joining host's (its <root>/link-state/node), or `{}` \
             for a fresh one",
            crate::join::NEW_NODE
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
        "doctor" => {
            // `--yes` ANSWERS A QUESTION ONLY `--retire-ghosts` ASKS. Parsed
            // silently it read as "yes to whatever doctor does", which is
            // nothing, and an operator who meant to retire would see a clean
            // report and believe it had run.
            if yes && !retire_ghosts {
                return Err("--yes means nothing without --retire-ghosts".to_string());
            }
            Cmd::Doctor { retire_ghosts, yes }
        }
        "mint-for" => Cmd::MintFor(mint),
        "join" => Cmd::Join(join),
        _ => Cmd::Status { json },
    })
}

/// `aterm fabric …` — the entry both the `aterm` front door and `aterm-link
/// fabric` land in.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    match parse_args(args) {
        Ok(Cmd::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Cmd::Status { json }) => status_main(json),
        Ok(Cmd::Tail {
            bodies,
            from,
            filter,
        }) => tail_main(bodies, from, filter.as_deref()),
        Ok(Cmd::On(opts)) => crate::enable::on(&opts),
        Ok(Cmd::Off(opts)) => crate::enable::off(&opts),
        Ok(Cmd::Doctor { retire_ghosts, yes }) => crate::enable::doctor(retire_ghosts, yes),
        Ok(Cmd::MintFor(opts)) => crate::join::mint_for(&opts),
        Ok(Cmd::Join(opts)) => crate::join::join(&opts),
        Err(e) => {
            eprintln!("aterm fabric: {e}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// CONFIG: where the bridge command comes from
// ---------------------------------------------------------------------------

/// Where the bridge command was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `$ATERM_FABRIC_COMMAND`.
    Env,
    /// `[fabric] command` in this file.
    File(PathBuf),
    /// `command` in the rendezvous file at this path (`aterm fabric on` wrote
    /// it; aterm.toml has no command).
    Rendezvous(PathBuf),
    /// None of those: the command a running instance (this pid) was armed with.
    Instance(u32),
}

impl Source {
    fn describe(&self) -> String {
        match self {
            Source::Env => format!("${FABRIC_COMMAND_ENV} (it overrides aterm.toml)"),
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

/// The aterm config file: `$XDG_CONFIG_HOME/aterm/aterm.toml`, else
/// `$HOME/.config/aterm/aterm.toml` — the path `aterm-gui`'s
/// `app_config::config_path` resolves on every Unix.
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm").join("aterm.toml"));
    }
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".config/aterm/aterm.toml"))
}

/// `[fabric] command` out of an aterm.toml's TEXT, through the parser the app
/// loads the file with. `Ok(None)` is "no command" — no table, no key, or a
/// blank string, which the app also reads as off.
///
/// # Errors
///
/// A file that is not TOML, a `fabric` that is not a table, or a `command`
/// that is not a string: the app would not start a bridge from any of those,
/// and the report cannot say what it would start.
pub fn command_in_toml(text: &str) -> Result<Option<String>, String> {
    let table: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let Some(fabric) = table.get("fabric") else {
        return Ok(None);
    };
    let Some(fabric) = fabric.as_table() else {
        return Err("`fabric` is not a table".to_string());
    };
    match fabric.get("command") {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) if s.trim().is_empty() => Ok(None),
            Some(s) => Ok(Some(s.to_string())),
            None => Err("`[fabric] command` is not a string".to_string()),
        },
    }
}

/// `[fabric] presence` out of an aterm.toml's TEXT: `Ok(None)` when the table
/// or the key is absent (the bridge's default, `meta`, applies).
///
/// # Errors
///
/// A file that is not TOML, a `fabric` that is not a table, a `presence` that
/// is not a string, or a spelling that is neither `meta` nor `minimal`.
pub fn presence_in_toml(text: &str) -> Result<Option<crate::presence::Mode>, String> {
    let table: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let Some(fabric) = table.get("fabric") else {
        return Ok(None);
    };
    let Some(fabric) = fabric.as_table() else {
        return Err("`fabric` is not a table".to_string());
    };
    match fabric.get("presence") {
        None => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) => crate::presence::Mode::parse(s).map(Some).ok_or_else(|| {
                format!("`[fabric] presence = {s:?}` is neither \"meta\" nor \"minimal\"")
            }),
            None => Err("`[fabric] presence` is not a string".to_string()),
        },
    }
}

/// `[fabric] presence` from the aterm.toml [`config_path`] resolves, for a
/// `serve` whose command line did not say. `meta` when the file has no such
/// key, or no file exists; ALSO `meta` — said on stderr, not fatal — when the
/// file cannot be read or the value is wrong, because a bridge that refused to
/// start over a presence typo would hold every session of its instance under
/// `fabric-lost`, which is the worse outcome by far.
#[must_use]
pub fn presence_from_config() -> crate::presence::Mode {
    let Some(path) = config_path() else {
        return crate::presence::Mode::Meta;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return crate::presence::Mode::Meta,
        Err(e) => {
            eprintln!(
                "aterm-link: {} could not be read ({e}); presence defaults to meta",
                path.display()
            );
            return crate::presence::Mode::Meta;
        }
    };
    match presence_in_toml(&text) {
        Ok(mode) => mode.unwrap_or_default(),
        Err(e) => {
            eprintln!(
                "aterm-link: {}: {e}; presence defaults to meta",
                path.display()
            );
            crate::presence::Mode::Meta
        }
    }
}

/// `[fabric] receipts` out of an aterm.toml's TEXT: `Ok(None)` when the table
/// or the key is absent.
///
/// # Errors
///
/// A file that is not TOML, a `fabric` that is not a table, or a `receipts`
/// that is not a boolean.
pub fn receipts_in_toml(text: &str) -> Result<Option<bool>, String> {
    let table: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let Some(fabric) = table.get("fabric") else {
        return Ok(None);
    };
    let Some(fabric) = fabric.as_table() else {
        return Err("`fabric` is not a table".to_string());
    };
    match fabric.get("receipts") {
        None => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| "`[fabric] receipts` is not a boolean".to_string()),
    }
}

/// `[fabric] receipts` from the aterm.toml [`config_path`] resolves, for a
/// `serve` whose command line said neither `--receipts` nor `--no-receipts`.
///
/// ON when the file has no such key, or no file exists; ALSO on — said on
/// stderr, not fatal — when the file cannot be read or the value is wrong, for
/// the reason [`presence_from_config`] gives: a bridge that refused to start
/// over a config typo would hold every session of its instance.
///
/// THE DEFAULT IS ONE THING NOW, AND IT IS THIS ONE (round 21). It used to be
/// off here while `aterm fabric on` wrote `receipts = true` into the file, so
/// whether a node acked depended on which door its operator came through: a
/// `fabric on` node acked, and a hand-written `[fabric] command`, a bridge
/// started by hand, or a different `XDG_CONFIG_HOME` did not. The owner's own
/// machine was the second kind — its `[fabric]` table was written by
/// `tools/fabric-enable.sh` and carries no `receipts` key — so `aterm fabric
/// status` there read `receipts off`.
///
/// OFF WAS THE WRONG ONE TO SETTLE ON because it breaks `ask`: with no ack,
/// `post --wait-ack` has nothing that can ever release it and every `ask`
/// burns its whole deadline before answering. The catalog says as much in as
/// many words — "a recipient whose bridge runs without receipts never acks, so
/// bound it". The cost of the other direction is one ack record and one `ev`,
/// about 200 bytes, per ask or task a recipient actually DECIDES — not per
/// message, and nothing at all on a fleet that sends only notes.
///
/// `--no-receipts`, and `[fabric] receipts = false`, still turn it off; an
/// operator's explicit `false` is never overwritten by a later `aterm fabric
/// on`.
#[must_use]
pub fn receipts_from_config() -> bool {
    let Some(path) = config_path() else {
        return true;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return true,
        Err(e) => {
            eprintln!(
                "aterm-link: {} could not be read ({e}); receipts default to on",
                path.display()
            );
            return true;
        }
    };
    match receipts_in_toml(&text) {
        Ok(on) => on.unwrap_or(true),
        Err(e) => {
            eprintln!(
                "aterm-link: {}: {e}; receipts default to on",
                path.display()
            );
            true
        }
    }
}

/// The configured command, env first — the app's own precedence.
///
/// # Errors
///
/// The config file exists and cannot be read or parsed.
pub fn resolve_command(
    env: Option<&str>,
    path: Option<&Path>,
) -> Result<Option<(Source, String)>, String> {
    if let Some(v) = env.filter(|v| !v.trim().is_empty()) {
        return Ok(Some((Source::Env, v.to_string())));
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

/// The command the rendezvous file mirrors, when there is one and aterm.toml
/// names none — `Ok(None)` for no file.
///
/// # Errors
///
/// A rendezvous file that exists and cannot be read.
fn rendezvous_command() -> Result<Option<(Source, String)>, String> {
    Ok(crate::enable::Rendezvous::read()?
        .and_then(|r| crate::enable::rendezvous_path().map(|p| (Source::Rendezvous(p), r.command))))
}

/// The last path component of a program word.
fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Whether `words[i]` is the `aterm link <verb>` / `aterm-link <verb>` verb.
///
/// BOTH SPELLINGS, because the shipped binary answers to both — `aterm link` is
/// the front-door verb and `aterm-link` its argv0 alias — and they are both in
/// use: `tools/fabric-enable.sh` writes the first, the design and every test
/// harness the second. The program before `link` is not checked, so a renamed
/// or bundled binary (`…/aterm.app/Contents/MacOS/aterm link serve`) still reads.
fn is_link_verb(words: &[&str], i: usize, verb: &str) -> bool {
    i >= 1 && words[i] == verb && (words[i - 1] == "link" || basename(words[i - 1]) == "aterm-link")
}

/// The flags of an `aterm link serve …` / `aterm-link serve …` command, or
/// `None` when the command is not a bridge at all.
#[must_use]
pub fn serve_flags(command: &str) -> Option<Vec<String>> {
    let words: Vec<&str> = command.split_whitespace().collect();
    let at = (0..words.len()).find(|&i| is_link_verb(&words, i, "serve"))?;
    Some(words[at + 1..].iter().map(|w| (*w).to_string()).collect())
}

/// The bridge command's `Config`, through the bridge's own parser.
pub(crate) fn bridge_config(command: &str) -> Result<Config, String> {
    let flags = serve_flags(command).ok_or_else(|| {
        format!(
            "the command is not a bridge (`aterm link serve …` or `aterm-link serve …`): {}",
            safe(command, 512)
        )
    })?;
    crate::cli::parse(&flags).map(|p| p.cfg)
}

/// The node id a bridge with this state dir answers as, if one is provisioned.
fn read_node(state_dir: &str) -> Option<String> {
    let raw = std::fs::read_to_string(Path::new(state_dir).join("node")).ok()?;
    let node = raw.trim();
    (node.starts_with("n-") && crate::subject::is_principal(node)).then(|| node.to_string())
}

// ---------------------------------------------------------------------------
// the process table
// ---------------------------------------------------------------------------

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

/// Parse `ps -o pid= -o ppid= -o command=` output. Lines that do not start with
/// two numbers are skipped.
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

/// The machine's process table, or empty when `ps` cannot be run (a sandbox) —
/// every consumer then says "not found" rather than guessing.
pub(crate) fn process_table() -> Vec<Proc> {
    Command::new("ps")
        .args(["-A", "-ww", "-o", "pid=", "-o", "ppid=", "-o", "command="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// The process serving the broker at `sock`: `aterm link broker <sock> …` or
/// `aterm-link broker <sock> …`.
#[must_use]
pub fn broker_pid(procs: &[Proc], sock: &str) -> Option<u32> {
    procs.iter().find_map(|p| {
        let words: Vec<&str> = p.cmd.split_whitespace().collect();
        (0..words.len())
            .any(|i| {
                is_link_verb(&words, i, "broker")
                    && (words.get(i + 1) == Some(&sock) || serves_socket(&words, i, sock))
            })
            .then_some(p.pid)
    })
}

/// [`broker_pid`]'s other spelling: the socket a `--tcp` broker serves
/// beside its port, `--unix <sock>` (`on --tcp`).
fn serves_socket(words: &[&str], at: usize, sock: &str) -> bool {
    words[at + 1..]
        .windows(2)
        .any(|w| w[0] == "--unix" && w[1] == sock)
}

/// The process serving a TCP broker a client dials at `dial`: `link broker
/// … --tcp <bind> …` whose bind IS `dial`, or an unspecified address
/// (`0.0.0.0` / `[::]`) on the same port — the bind this host's clients reach
/// on loopback ([`transport::dial_for_bind`]).
#[must_use]
pub fn broker_pid_tcp(procs: &[Proc], dial: &str) -> Option<u32> {
    procs.iter().find_map(|p| {
        let words: Vec<&str> = p.cmd.split_whitespace().collect();
        let at = (0..words.len()).find(|&i| is_link_verb(&words, i, "broker"))?;
        let bind = words[at + 1..]
            .windows(2)
            .find(|w| w[0] == "--tcp")
            .map(|w| w[1])?;
        (transport::dial_for_bind(bind) == dial).then_some(p.pid)
    })
}

/// Every process running a bridge (`link serve`), with its command line.
#[must_use]
pub fn bridge_procs(procs: &[Proc]) -> Vec<&Proc> {
    procs
        .iter()
        .filter(|p| serve_flags(&p.cmd).is_some())
        .collect()
}

/// The launchd job label whose running pid is `pid`, from `launchctl list`'s
/// `PID  Status  Label` rows.
#[must_use]
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
    let out = Command::new("launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    parse_launchctl(&String::from_utf8_lossy(&out.stdout), pid)
}

#[cfg(not(target_os = "macos"))]
fn launchd_label(_pid: u32) -> Option<String> {
    None
}

// ---------------------------------------------------------------------------
// BROKER
// ---------------------------------------------------------------------------

/// What the broker probe found.
#[derive(Debug, Clone, Default)]
pub struct BrokerView {
    /// The endpoint the command names.
    pub endpoint: String,
    /// `unix`, `tcp` or `tcp+sealed`.
    pub transport: &'static str,
    /// Whether a broker ANSWERED — a connect and a `Hello` round trip.
    pub reachable: bool,
    /// Why it is not reachable, or why the cap file was refused.
    pub error: Option<String>,
    /// Whether every cap was attached, so the bus can be read.
    pub attached: bool,
    /// The pid serving it, from the process table.
    pub pid: Option<u32>,
    /// The launchd job running that pid (macOS).
    pub launchd: Option<String>,
    /// The bus head offset — the next offset the log will assign.
    pub head: Option<u64>,
    /// How long connect + hello + attach + head took.
    pub rtt_ms: Option<u64>,
    /// `None` when the report reads the whole fleet (`/f/<F>/>`); `Some(faces)`
    /// when the broker is GUARDED and the cap files do not grant that — the
    /// sealed cross-host broker (round 16) always is — so every read is made
    /// over the faces the caps DO grant, and TRAFFIC says so.
    pub faces: Option<Vec<String>>,
    /// The sealed TCP endpoint this host's broker serves BESIDE the socket
    /// probed above (`on --tcp`, from the rendezvous file's `serves_tcp`):
    /// the one port a joining host dials. Not probed here — `on`'s `wire`
    /// step proves it — so a report is never held up by that port's
    /// handshake slots.
    pub serves_tcp: Option<String>,
}

impl BrokerView {
    /// Whether the BUS WAS READ: the connect, the `Hello`, every attach AND
    /// the head query all answered.
    ///
    /// `attached` alone is not that. A broker answers the `Attach` of a cap
    /// whose grants do not cover `/f/<fleet>/>` (a `--fleet` typo in the
    /// command) and then REFUSES the `Fetch`; one that dies between the attach
    /// and the head query answers neither. Both leave `attached = true` and
    /// `head = None` — nothing of the bus can be read, so the roster, the
    /// traffic and the standing fleet halts are all missing, and calling that
    /// "answered" is the report lying about the one thing it exists to say.
    #[must_use]
    pub fn read_ok(&self) -> bool {
        self.reachable && self.attached && self.head.is_some()
    }
}

/// One connection with a read and write bound on the socket underneath.
///
/// The connect itself is bounded too on TCP (a black-holed address costs
/// IO_TIMEOUT, not the OS's SYN-retry schedule). The sealed wire keeps its
/// handshake's own deadline, and every read and write after it is bounded
/// through the closer — the one handle on the socket under the record layer — so
/// a broker that completes the handshake and then never answers costs the
/// operator IO_TIMEOUT, not a hung terminal.
fn connect_bounded(t: &Transport, endpoint: &str) -> io::Result<Conn> {
    let within = matches!(t, Transport::Tcp).then_some(IO_TIMEOUT);
    let (conn, closer) = transport::connect_within(t, endpoint, within)?;
    closer.set_read_timeout(Some(IO_TIMEOUT))?;
    closer.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(conn)
}

/// The faces a report may read when the broker is guarded and `/f/<F>/>` is
/// not granted: every grant's filter under `/f/<F>/` in the cap files — both
/// modes, because a guarded broker authorizes a read by ANY grant containing
/// it — except the consumer-group names under `/f/<F>/cur/`, which are never
/// published to. Sorted and deduplicated, so a report is reproducible.
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

/// Whether a broker error is the GUARD refusing a read (as opposed to a
/// broken connection or a dead broker) — the one error a narrower read can
/// answer.
fn refused(e: &io::Error) -> bool {
    e.to_string().contains("unauthorized")
}

/// Reach the broker FOR REAL: connect, `Hello`, attach every cap, head query.
pub(crate) fn probe_broker(cfg: &Config, procs: &[Proc]) -> (BrokerView, Option<Conn>) {
    let mut view = BrokerView {
        endpoint: cfg.broker.clone(),
        transport: cfg.transport.name(),
        ..BrokerView::default()
    };
    view.pid = if matches!(cfg.transport, Transport::Unix) {
        broker_pid(procs, &cfg.broker)
    } else {
        broker_pid_tcp(procs, &cfg.broker)
    };
    view.launchd = view.pid.and_then(launchd_label);
    let started = Instant::now();
    let mut conn = match connect_bounded(&cfg.transport, &cfg.broker) {
        Ok(c) => c,
        Err(e) => {
            view.error = Some(format!("connect: {e}"));
            return (view, None);
        }
    };
    if let Err(e) = conn.hello() {
        view.error = Some(format!("the socket accepted but no broker answered: {e}"));
        return (view, None);
    }
    view.reachable = true;
    for path in &cfg.cap_files {
        let caps = match read_cap_file(path) {
            Ok(c) => c,
            Err(e) => {
                view.error = Some(format!("cap file {path}: {e}"));
                return (view, None);
            }
        };
        for cap in caps {
            if let Err(e) = conn.attach(&cap.grant, &cap.tag) {
                view.error = Some(format!("the broker refused `{}`: {e}", cap.grant));
                return (view, None);
            }
        }
    }
    view.attached = true;
    // THE HEAD IS GLOBAL, whichever filter asks: the whole fleet first, and on
    // a GUARDED broker that refuses it — the sealed cross-host broker always
    // is, and a node's ring never grants `/f/<F>/>` — the first face the caps
    // do grant. A broker that refuses every one is still "the bus cannot be
    // read", with the whole-fleet refusal as its reason.
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
    (view, Some(conn))
}

// ---------------------------------------------------------------------------
// TRAFFIC
// ---------------------------------------------------------------------------

/// Every record of the fleet: `/f/<F>/>`. NOT `subject::fleet_filter`, which
/// is the fleet BROADCAST face (`/f/<F>/fleet/>`, halts and barriers) — one
/// subtree of this.
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
    /// Who sent it: the cap-forced `<src>` segment, or `<sid>@<node>` when a
    /// node attests which of its sessions spoke (the bridge's own rule).
    pub from: String,
    /// Who it is for: `<sid>@<node>`, `fleet`, `all`, or `-` for a node's own
    /// face.
    pub to: String,
    /// The record's kind: the `in` kind, or the face's leaf (`presence`, …).
    pub kind: String,
    /// The body's text length in bytes.
    pub len: usize,
    /// The RECEIVER's label, computed from the address alone
    /// ([`crate::render::trust_of`]).
    pub trust: &'static str,
    /// The text itself — only `tail --bodies` ever prints it.
    pub text: String,
}

/// The bridge's `render_from` rule: a NODE may attest which of its SESSIONS
/// spoke (`from=<sid>` in the body), because its `<src>` segment is cap-forced
/// and it could type as that session anyway; anything else is the segment.
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

/// Classify one record. TOTAL: a subject this build does not know still renders,
/// as its face with the most conservative label.
#[must_use]
pub fn traffic_of(fleet: &str, off: u64, subject: &str, raw: &[u8]) -> Traffic {
    let (body, _) = Body::decode(raw);
    let relayed = body.via.is_some();
    let segs: Vec<&str> = subject.split('/').collect();
    let seg = |i: usize| safe(segs.get(i).copied().unwrap_or("-"), 128);
    let face = if segs.len() >= 5 && segs[0].is_empty() && segs[1] == "f" && segs[2] == fleet {
        segs[3]
    } else {
        ""
    };
    let (from, to, kind, trust) = match (face, segs.len()) {
        // `/f/<F>/in/<node>/<sid>/<src>/<kind>` — a message to one session.
        ("in", 8) => (
            safe(&render_from(segs[6], body.from.as_deref()), 128),
            format!("{}@{}", seg(5), seg(4)),
            seg(7),
            trust_of(segs[6], relayed),
        ),
        // `/f/<F>/fleet/<h>/<kind>` — the broadcast face (a halt, a notice).
        ("fleet", 6) => (
            seg(4),
            "fleet".to_string(),
            seg(5),
            trust_of(segs[4], relayed),
        ),
        // `/f/<F>/term/<node>/<sid>/in/<src>` — a keystroke record for a session.
        ("term", 8) if segs[6] == "in" => (
            seg(7),
            format!("{}@{}", seg(5), seg(4)),
            "term/in".to_string(),
            SCREEN,
        ),
        // `/f/<F>/term/<node>/<sid>/<leaf…>` — screen content, by definition.
        ("term", n) if n >= 7 => (
            format!("{}@{}", seg(5), seg(4)),
            "-".to_string(),
            format!("term/{}", safe(&segs[6..].join("/"), 64)),
            SCREEN,
        ),
        // `/f/<F>/pub/<node>/<sid>/say/<topic>` — a broadcast. `to=` names the
        // face a reader subscribes to; the kind is in the body.
        ("pub", 8) if segs[6] == "say" => (
            format!("{}@{}", seg(5), seg(4)),
            format!("say:{}", seg(7)),
            safe(body.kind.as_deref().unwrap_or("note"), 32),
            trust_of(segs[4], relayed),
        ),
        // `/f/<F>/pub/<node>/{node,<sid>}/<leaf…>` — presence, ev, ack, control.
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

/// The last `n` records under the fleet filter, oldest first.
///
/// `Fetch` only reads FORWARD, so this reads a window below the head and widens
/// it until it holds `n` fleet records, reaches offset 0, or has scanned
/// [`TRAFFIC_SCAN_MAX`] offsets back — whichever is first. Offsets are dense
/// over the whole log, and records outside the fleet (and the broker's own
/// commit records) take offsets too, which is why a window can come back short.
///
/// On a guarded broker ([`BrokerView::faces`]) the window is read per granted
/// face and the pages merged by offset — a record two faces both match is
/// one record.
fn last_records(
    conn: &mut Conn,
    fleet: &str,
    faces: Option<&[String]>,
    head: u64,
    n: usize,
) -> io::Result<Vec<Traffic>> {
    let filters: Vec<String> = faces.map_or_else(|| vec![fleet_root(fleet)], <[String]>::to_vec);
    // ONE WINDOW FOR EVERY FACE, widened together: the merged answer is what
    // has to hold `n` records, so a face that is quiet (a node's screen face,
    // say) does not widen its own scan to the ceiling while the busy faces
    // already filled the table — the cost stays one window's worth per face.
    let mut window: u64 = 256;
    loop {
        let start = head.saturating_sub(window);
        let mut merged: BTreeMap<u64, Traffic> = BTreeMap::new();
        for filter in &filters {
            let mut next = start;
            loop {
                let (page, (after, now_head)) = conn.fetch(next, filter, 256)?;
                for (off, subject, raw) in &page {
                    merged.insert(*off, traffic_of(fleet, *off, subject, raw));
                    if merged.len() > n {
                        merged.pop_first();
                    }
                }
                // `after` is the offset past the last record the broker SCANNED,
                // so it advances across a page that matched nothing; no progress
                // at all is the broker's end of the log.
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

/// The publisher's `t=` of the record at exactly `off` under `filter`, if the
/// log still holds it.
fn record_t(conn: &mut Conn, filter: &str, off: u64) -> Option<u64> {
    let (page, _) = conn.fetch(off, filter, 1).ok()?;
    let (at, _, raw) = page.first()?;
    (*at == off).then(|| Body::decode(raw).0.t)
}

/// An `ask` or `task` whose `dl=` has passed with no `answer`, `report` or
/// `ack` carrying its offset as `re=` anywhere on the fleet's `in` lanes (R8).
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
    /// Whether the asker's own bridge has already put `expired re=<off>` on
    /// the asker's lane — the verdict R8 makes that bridge responsible for.
    pub expired: bool,
}

/// How many offsets below the head the deadline scan reads. Every `in` record
/// in that window is decoded once; a fleet busier than this over one report is
/// a fleet whose oldest asks are past any deadline worth listing.
const OVERDUE_SCAN_SPAN: u64 = 1 << 16;

/// Every overdue ask on the bus, oldest first, over the last
/// [`OVERDUE_SCAN_SPAN`] offsets.
///
/// FOLDED PAGE BY PAGE. The scan used to collect every raw record in the window
/// — up to 65,536 of them, BODIES included — before reading any; a report is a
/// one-shot command, but a window of large bodies made it a memory spike of
/// the whole window's text. Each page is reduced to the few fields the rule
/// needs ([`OverdueScan`]) and dropped, so the scan holds one page of bodies at
/// a time and the rest as offsets and addresses.
///
/// On a guarded broker the `in` faces the caps grant are scanned instead of
/// `/f/<F>/in/>` — a node's own inbox lanes and its own posts, which are the
/// asks a node can see — and a record two faces match is fed once.
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
            let (page, (after, now_head)) = conn.fetch(next, filter, 256)?;
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

/// The filters the `in`-face reads use: `/f/<F>/in/>`, or on a guarded broker
/// the granted faces under it.
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

/// The overdue asks among `records` (each `(off, subject, raw)`), as of
/// `now_ms`. PURE, so the rule is pinned by a test that needs no broker: a
/// `WORK_KINDS` record with `dl=` whose `t + dl` is past, and no
/// `answer|report|ack` record with `re=` naming it.
#[must_use]
pub fn overdue_of(fleet: &str, records: &[(u64, String, Vec<u8>)], now_ms: u64) -> Vec<Overdue> {
    let mut scan = OverdueScan::default();
    for (off, subject, raw) in records {
        scan.feed(fleet, *off, subject, raw);
    }
    scan.finish(now_ms)
}

/// [`overdue_of`]'s rule as a fold: what one record contributes is decided
/// when it is read, and nothing of its body is kept.
#[derive(Default)]
struct OverdueScan {
    settled: BTreeSet<u64>,
    expired: BTreeSet<u64>,
    asks: Vec<(Traffic, u64, u64)>,
}

impl OverdueScan {
    fn feed(&mut self, fleet: &str, off: u64, subject: &str, raw: &[u8]) {
        let t = traffic_of(fleet, off, subject, raw);
        let (body, _) = Body::decode(raw);
        match t.kind.as_str() {
            k if WORK_KINDS.contains(&k) => {
                if let Some(dl) = body.dl {
                    // The addresses, never the text: an overdue row names who
                    // asked whom, and only `tail --bodies` prints a body.
                    let t = Traffic {
                        text: String::new(),
                        ..t
                    };
                    self.asks.push((t, body.t, dl));
                }
            }
            "answer" | "report" | "ack" => {
                if let Some(re) = body.re {
                    self.settled.insert(re);
                }
            }
            "expired" => {
                if let Some(re) = body.re {
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
            .filter(|(t, _, _)| !settled.contains(&t.off))
            .filter_map(|(t, at, dl)| {
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

// ---------------------------------------------------------------------------
// BRIDGES and local SESSIONS, over the control socket
// ---------------------------------------------------------------------------

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
    /// The last recorded hold transition's `(reason, origin)`, when held.
    pub hold_why: Option<(String, String)>,
    /// The BROADCAST TOPICS this session has opted into (`topic ls`), in topic
    /// order. Empty is the default and means the session receives no broadcast
    /// at all — which is a fact an operator reading this report wants, because
    /// "the shout went out and nobody heard it" and "the shout never went out"
    /// look identical from the sender's side.
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
    /// Why it could not be asked, when it could not.
    pub error: Option<String>,
    /// `fabric status`'s `state=`.
    pub fabric: Option<String>,
    /// `fabric status`'s `reason=` — why the bridge's broker link is down, when
    /// `state=stalled` (`-` otherwise). Absent from an instance that predates
    /// the link report.
    pub reason: Option<String>,
    /// `fabric status`'s `rtt_ms=`: the bridge's last acknowledged round trip
    /// to the broker.
    pub rtt_ms: Option<u64>,
    /// `fabric status`'s `link_age_ms=`: how long ago that ack was.
    pub link_age_ms: Option<u64>,
    /// `fabric status`'s `supervised=`.
    pub supervised: Option<bool>,
    /// The command it runs (or was launched with), decoded.
    pub command: Option<String>,
    /// Its bridge child pid(s), from the process table.
    pub bridge_pids: Vec<u32>,
    /// The node its bridge answers as, from that command's state dir.
    pub node: Option<String>,
    /// The state dir its bridge command names.
    pub state_dir: Option<String>,
    /// Its sessions.
    pub sessions: Vec<LocalSession>,
}

/// The BRIDGES table's FABRIC cell: the state, and what the bridge said about
/// its broker link — the `stalled` reason, or the last acked round trip and its
/// age — so a reader sees at a glance both that mail cannot move and why.
#[must_use]
pub fn fabric_cell(i: &InstanceView) -> String {
    let state = safe(i.fabric.as_deref().unwrap_or("-"), 32);
    match (i.fabric.as_deref(), &i.reason, i.rtt_ms, i.link_age_ms) {
        (Some("stalled"), Some(reason), _, _) => format!("{state} ({})", safe(reason, 32)),
        // `stale` CARRIES THE AGE FOR THE SAME REASON `connected` DOES — it is
        // the same number, and it is the whole evidence for the word. The
        // endpoint only says `stale` while a post it handed over is still
        // unanswered, so unlike a large age on `connected` this one is not a
        // quiet link.
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

/// Fill a session's inbox numbers from the two peeked replies: `inbox 0 --peek
/// --meta` (whose header counts EVERY unlisted row as `pending=`, and which
/// lists every un-landed post) and `inbox --peek --meta` (every message row).
///
/// UNREAD IS DERIVED, and the derivation is exact: `inbox seen <id>` lists every
/// row at or below its argument, so no unlisted row sits at or below `seen=` —
/// the rows above it are exactly the listed-unhandled ones plus `pending`.
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

/// The `(reason, origin)` of the LAST `kind=hold` transition in a `timeline`
/// reply, when it put the hold on.
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
        kv(row, "origin").unwrap_or("?").to_string(),
    ))
}

/// One ctl request, as `Ok(reply)` only when it answered `OK`.
fn ask(ctl: &mut Ctl, line: &str) -> Result<crate::ctl::Reply, String> {
    let reply = ctl.request(line).map_err(|e| format!("`{line}`: {e}"))?;
    if reply.ok() {
        Ok(reply)
    } else {
        Err(format!("`{line}` answered {}", safe(reply.header(), 160)))
    }
}

/// Ask one session for its title, role, inbox numbers and — when held — why.
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
        // AN OLDER INSTANCE DOES NOT KNOW THE VERB, and that is not a read
        // failure: this report is run against whatever is on the machine, and
        // an `ERR unknown verb` here would mark the whole session `error=` and
        // hide its inbox numbers. An empty list is what an instance without
        // broadcast has, which is also what it reports.
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

/// Ask one instance for its bridge and — with `sessions` — every session's
/// numbers. `None` for a STALE socket — nothing accepts on it and the pid it
/// names is gone — which is a leftover file, not an instance.
fn read_instance(pid: u32, sock: &str, procs: &[Proc], sessions: bool) -> Option<InstanceView> {
    let mut v = InstanceView {
        pid,
        sock: sock.to_string(),
        ..InstanceView::default()
    };
    v.bridge_pids = procs
        .iter()
        .filter(|p| p.ppid == pid && pid != 0 && serve_flags(&p.cmd).is_some())
        .map(|p| p.pid)
        .collect();
    let token = match aterm_ctl::instance_token(sock) {
        Ok(t) => t,
        Err(e) => {
            if !aterm_uds::process::pid_alive(pid) {
                return None;
            }
            v.error = Some(format!("no Owner token: {e}"));
            return Some(v);
        }
    };
    let mut ctl = match Ctl::connect(sock, &token) {
        Ok(c) => c,
        Err(e) => {
            if e.kind() == io::ErrorKind::ConnectionRefused && !aterm_uds::process::pid_alive(pid) {
                return None;
            }
            v.error = Some(format!("connect: {e}"));
            return Some(v);
        }
    };
    let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
    let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
    match ask(&mut ctl, "fabric status") {
        Ok(r) => {
            let h = r.header();
            v.fabric = kv(h, "state").map(str::to_string);
            v.reason = kv(h, "reason").filter(|r| *r != "-").map(str::to_string);
            v.rtt_ms = kv(h, "rtt_ms").and_then(|n| n.parse().ok());
            v.link_age_ms = kv(h, "link_age_ms").and_then(|n| n.parse().ok());
            v.supervised = kv(h, "supervised").map(|s| s == "1");
            v.command = kv(h, "command")
                .filter(|c| *c != "-")
                .map(crate::pct::decode);
        }
        Err(e) => {
            v.error = Some(e);
            return Some(v);
        }
    }
    if let Some(cfg) = v.command.as_deref().and_then(|c| bridge_config(c).ok()) {
        v.node = read_node(&cfg.state_dir);
        v.state_dir = Some(cfg.state_dir);
    }
    if !sessions {
        return Some(v);
    }
    let rows = match ask(&mut ctl, "sessions") {
        Ok(r) => r.rows().to_vec(),
        Err(e) => {
            v.error = Some(e);
            return Some(v);
        }
    };
    for row in &rows {
        let Some(sid) = row
            .split(' ')
            .nth(1)
            .filter(|s| crate::subject::is_principal(s))
        else {
            continue;
        };
        v.sessions.push(read_session(&mut ctl, sid));
    }
    Some(v)
}

// ---------------------------------------------------------------------------
// the report
// ---------------------------------------------------------------------------

/// One SESSIONS row: a local session, a live remote one, or both joined.
#[derive(Debug, Clone, Default)]
pub struct SessionRow {
    /// The session id.
    pub sid: String,
    /// The local instance hosting it, if one here does.
    pub pid: Option<u32>,
    /// The node its presence row (or its instance's bridge) names.
    pub node: String,
    /// The bus presence `state=`, or `-` when the bus has no row for it.
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

/// A presence field as the report shows it: `None` for an absent one or the
/// dash an older bridge prints, else pct-decoded.
fn bus_field(row: &crate::glance::Row, key: &str) -> Option<String> {
    let v = row.field(key);
    (v != crate::glance::ABSENT && !v.is_empty()).then(|| crate::pct::decode(v))
}

/// A presence row of a NODE (not a session).
#[derive(Debug, Clone, Default)]
pub struct NodeRow {
    /// The node id.
    pub node: String,
    /// Its `host=`.
    pub host: String,
    /// Its `state=` (`live`, or `gone` once its will fired).
    pub state: String,
    /// Its `fabric=`.
    pub fabric: String,
}

/// Everything `status` found.
#[derive(Debug, Clone)]
pub struct Report {
    /// Where the command came from.
    pub source: Source,
    /// The command itself.
    pub command: String,
    /// The config file looked in, if any.
    pub config_path: Option<PathBuf>,
    /// The parsed bridge config.
    pub cfg: Config,
    /// What the bridge's presence rows carry: `--presence` on the command, else
    /// `[fabric] presence` in the config file, else `meta`.
    pub presence: crate::presence::Mode,
    /// Whether the bridge publishes receipts (R8): `--receipts`/`--no-receipts`
    /// on the command, else `[fabric] receipts` in the config file, else off.
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
    /// Bus session rows that are not live and not local (not shown in text).
    pub exited: usize,
    /// TRAFFIC, oldest first.
    pub traffic: Vec<Traffic>,
    /// Every `ask`/`task` on the bus past its `dl=` with no reply (R8). Each
    /// is a WARNING; kept as data for `--json`.
    pub overdue: Vec<Overdue>,
    /// WARNINGS.
    pub warnings: Vec<String>,
    /// When the report was taken, ms since the epoch.
    pub now_ms: u64,
}

/// The fabric is OFF or UNREADABLE — `status` exits 2 with this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Off {
    /// No command in the env, the config, or any running instance.
    NoCommand(Option<PathBuf>),
    /// The config cannot be read, or its command is not a bridge.
    Unreadable(String),
}

impl Off {
    /// The one line the operator reads.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Off::NoCommand(Some(p)) => format!(
                "fabric is off: no [fabric] command in {} (and no ${FABRIC_COMMAND_ENV}, no \
                 rendezvous file); `aterm fabric on` turns it on",
                p.display()
            ),
            Off::NoCommand(None) => format!(
                "fabric is off: no config file (neither $XDG_CONFIG_HOME nor $HOME is set) \
                 and no ${FABRIC_COMMAND_ENV}; `aterm fabric on` turns it on"
            ),
            Off::Unreadable(why) => format!("fabric config is unreadable: {why}"),
        }
    }
}

/// Take the whole report. Pure of printing: every section is data first.
///
/// # Errors
///
/// [`Off`] when there is nothing to report on.
pub fn gather() -> Result<Report, Off> {
    let now_ms = crate::now_ms();
    let config_path = config_path();
    let env = std::env::var(FABRIC_COMMAND_ENV).ok();
    let resolved =
        match resolve_command(env.as_deref(), config_path.as_deref()).map_err(Off::Unreadable)? {
            Some(found) => Some(found),
            None => rendezvous_command().map_err(Off::Unreadable)?,
        };
    let procs = process_table();

    // The local instances first: when neither the env nor the file names a
    // command, a running bridge may still be carrying mail.
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
    let node = read_node(&cfg.state_dir);
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

    let (mut broker, conn) = probe_broker(&cfg, &procs);
    broker.serves_tcp = crate::enable::Rendezvous::read()
        .ok()
        .flatten()
        .filter(|r| r.broker == cfg.broker)
        .and_then(|r| r.serves_tcp);
    let mut report = Report {
        source,
        command,
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
            Ok(g) => roster = Some(g),
            Err(e) => bus_errors.push(format!("the presence roster could not be read: {e}")),
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

/// Every standing fleet halt: `(human, reason)` for each `Last{/f/<F>/fleet/*/halt}`
/// row that says `state=on`. A read failure is an empty list — the presence
/// rows' `hold=` still shows a halt the nodes applied.
fn fleet_halts(conn: &mut Conn, fleet: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let filter = format!("/f/{fleet}/fleet/*/halt");
    let _ = transport::walk_last(conn, &filter, |(_, subject, raw)| {
        let line = String::from_utf8_lossy(raw);
        if kv(&line, "state") == Some("on") {
            let who = subject.split('/').nth(4).unwrap_or("-");
            out.push((
                safe(who, 64),
                crate::pct::decode(kv(&line, "reason").unwrap_or("-")),
            ));
        }
        Ok(())
    });
    out
}

/// Join the bus roster with the local instances, by sid.
/// How long a retired session row is still shown as `gone` before it becomes
/// one of the `exited` count.
///
/// Presence is retained per SUBJECT and a session's subject is minted once, so
/// without a bound this table would list every session ever run on this
/// machine. An hour is long enough for the operator who just watched an
/// instance die to see what became of its sessions.
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
                });
            } else {
                bus.entry(row.owner.clone()).or_default().push(row);
            }
        }
    }
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
                bus: row.map_or_else(|| "-".to_string(), |r| r.field("state").to_string()),
                local: Some(s.clone()),
                bus_hold: row.map(|r| r.field("hold").to_string()),
                bus_role: row.and_then(|r| bus_field(r, "role")),
                bus_title: row.and_then(|r| bus_field(r, "title")),
                detail: row.and_then(|r| bus_field(r, "detail")),
                phase: row.and_then(|r| bus_field(r, "phase")),
                context: row.and_then(|r| bus_field(r, "context")),
            });
        }
    }
    // WHOSE NODES THESE ARE. A retired row under THIS machine's node is the
    // answer to a warning this report used to repeat forever, so it is shown;
    // one under a remote node is somebody else's history and stays a number.
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
            if row.field("state") != "live" {
                // A GHOST THAT WAS RETIRED IS SHOWN AS `gone`, ONCE IT IS
                // OVER AND WHILE IT IS STILL NEWS.
                //
                // The bridge's own token on a session face is `exited`
                // (`bridge.rs`'s `publish_session_presence`); `gone` is what
                // this table prints, because it is the word the fleet already
                // uses for "this is over" on the node face and the reader's
                // question is reachability, not vocabulary. Bounded by the
                // row's own `t=`: presence is retained per subject forever, so
                // one row per session ever run on this machine would be the
                // whole history, not a report.
                let recent = row
                    .field("t")
                    .parse::<u64>()
                    .ok()
                    .is_some_and(|t| now.saturating_sub(t) <= GONE_SHOW_MS);
                if !(recent && local_nodes.contains(&row.node)) {
                    report.exited += 1;
                    continue;
                }
                report.sessions.push(SessionRow {
                    sid: sid.clone(),
                    pid: None,
                    node: row.node.clone(),
                    bus: "gone".to_string(),
                    local: None,
                    bus_hold: Some(row.field("hold").to_string()),
                    bus_role: bus_field(row, "role"),
                    bus_title: bus_field(row, "title"),
                    detail: bus_field(row, "detail"),
                    phase: bus_field(row, "phase"),
                    context: bus_field(row, "context"),
                });
                continue;
            }
            report.sessions.push(SessionRow {
                sid: sid.clone(),
                pid: None,
                node: row.node.clone(),
                bus: "live".to_string(),
                local: None,
                bus_hold: Some(row.field("hold").to_string()),
                bus_role: bus_field(row, "role"),
                bus_title: bus_field(row, "title"),
                detail: bus_field(row, "detail"),
                phase: bus_field(row, "phase"),
                context: bus_field(row, "context"),
            });
        }
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

/// Every condition that makes `connected` a lie or loses mail.
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
             instead (armed by hand with `aterm ctl fabric attach`, or launched with \
             ${FABRIC_COMMAND_ENV} set), and the next aterm launch starts none — \
             `aterm fabric on` writes it"
        ));
    }
    if r.node.is_none() {
        w.push(format!(
            "no node id in {}/node: the bridge has no identity to answer mail as — \
             `aterm fabric on` provisions one",
            safe(&r.cfg.state_dir, 256)
        ));
    }
    for (path, caps) in &r.caps {
        if let Err(e) = caps {
            w.push(format!("cap file {}: {}", safe(path, 256), safe(e, 256)));
        }
    }

    if !r.broker.reachable {
        w.push(format!(
            "the broker at {broker} does not answer ({}): no record lands and none is \
             delivered",
            safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
        ));
    } else if !r.broker.read_ok() {
        w.push(format!(
            "the broker at {broker} answers but the bus could not be read: {}",
            safe(r.broker.error.as_deref().unwrap_or("unknown"), 256)
        ));
    }

    if let Some(e) = &r.discovery_error {
        w.push(format!(
            "cannot list this machine's aterm instances: {}",
            safe(e, 512)
        ));
    }
    // Bridges sharing one state dir share one node id, so they are ONE node on
    // the bus: each reads that node's whole inbox lane (a group subscription
    // resumes every member from the one committed cursor), and a bridge files a
    // record for a session its own instance does not host as `undeliverable
    // reason=not-hosted` (`Bridge::on_inbox_record`).
    let mut by_state: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for inst in &r.instances {
        if let (Some(dir), Some(true)) = (&inst.state_dir, inst.supervised) {
            by_state.entry(dir.clone()).or_default().push(inst.pid);
        }
    }
    for (dir, pids) in by_state.iter().filter(|(_, p)| p.len() > 1) {
        let list: Vec<String> = pids.iter().map(u32::to_string).collect();
        w.push(format!(
            "instances {} run bridges on ONE state dir ({}), so as ONE node: both read \
             that node's whole inbox lane, and each files the mail for the other's \
             sessions as undeliverable (not-hosted)",
            list.join(", "),
            safe(dir, 256)
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
        // What THIS instance's bridge was started with. The broker probed above
        // is the one the report's command names; a bridge armed with another
        // command talks to its own, and nothing above says anything about that.
        let own = inst.command.as_deref().and_then(|c| bridge_config(c).ok());
        let same_broker = own.as_ref().is_none_or(|c| c.broker == r.cfg.broker);
        match (inst.fabric.as_deref(), inst.supervised) {
            (_, Some(false)) => w.push(format!(
                "instance {pid} has no bridge (fabric={}, not supervised): its sessions \
                 receive no mail and their posts only queue — `aterm ctl --pid {pid} \
                 fabric attach <command>` arms one",
                safe(inst.fabric.as_deref().unwrap_or("-"), 32)
            )),
            (Some("disconnected"), _) => w.push(format!(
                "instance {pid}'s bridge is down (fabric=disconnected): the sessions it \
                 governed are held until a relaunched bridge attaches and lifts the hold"
            )),
            // THE BRIDGE IS UP AND ITS LINK IS NOT (round 13). The bridge dials
            // again with back-off (100 ms to 5 s) and says nothing in between,
            // so this is the state a killed broker, a wrong socket path or a
            // wedged broker leaves an instance in — and every post from it
            // queues while no mail arrives.
            // ATTACHED AND SILENT ABOUT ITS LINK: `reason=starting` is the
            // token between the attach and the bridge's first report. A
            // round-13 bridge reports within its dial; a bridge from before
            // the link report (0.85 and earlier) never does, and its
            // instance reads `connected` at the first record it delivers —
            // its mail moves (measured 2026-09-14: the note landed at off=3
            // under `stalled (starting)`), so this is not "no mail arrives".
            (Some("stalled"), _) if inst.reason.as_deref() == Some("starting") => {
                w.push(format!(
                    "instance {pid}'s bridge is attached and has not reported its broker link \
                     (fabric=stalled reason=starting): a bridge older than the link report \
                     never does and shows connected at its first delivery, so `post --wait` \
                     from its sessions waits rather than failing fast; a newer bridge reports \
                     within its dial — if this stays, check the broker at {broker}"
                ));
            }
            (Some("stalled"), _) => w.push(format!(
                "instance {pid}'s bridge is attached but its broker link is down \
                 (fabric=stalled reason={}, last ack {}): its posts queue and no mail \
                 arrives until the link is back — the bridge redials with back-off, so \
                 check the broker at {broker}",
                safe(inst.reason.as_deref().unwrap_or("-"), 32),
                inst.link_age_ms
                    .map_or_else(|| "never".to_string(), |a| format!("{} ago", age(a)))
            )),
            // OWED AND SILENT. The endpoint handed this bridge a post and has
            // had nothing back since — not an ack, not a landing, not a link
            // report. A bridge whose BROKER died says `link down reason=no-ack`
            // within its own 5 s deadline and reads `stalled`; one that closed
            // its fds reads `disconnected`. This is the third case and the one
            // neither of those can see: the helper process itself is wedged,
            // holding both lanes open and answering nothing.
            (Some("stale"), _) => w.push(format!(
                "instance {pid}'s bridge has not answered for the work it was given \
                 (fabric=stale, last ack {}): its posts queue and `post --wait` from its \
                 sessions fails fast rather than waiting — the bridge process is attached \
                 but not moving mail, so kill the bridge pid and let the instance relaunch it",
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
        if inst.supervised == Some(true)
            && inst.command.as_deref().and_then(serve_flags) != config_flags
            && r.source != Source::Instance(pid)
        {
            let differs = own.as_ref().map_or_else(
                || "it cannot be parsed as a bridge".to_string(),
                |c| {
                    let mut d = Vec::new();
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
        // What the FLEET'S OWN DIRECTORY says about this instance's node. A
        // bridge that says `fabric=connected` while the bus retains
        // `state=gone fabric=disconnected` for its node is invisible to every
        // peer: they read the roster, not this machine. Measured on this
        // machine 2026-09-14 — a second bridge sharing the state dir exited and
        // its will fired `state=gone` for the node id both were using, and the
        // surviving bridge never republishes presence unprompted.
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
                         fleet's roster sees this node dead (`aterm link ls` says so), and \
                         a bridge that shared this state dir has exited and fired its will",
                        safe(row.node.as_str(), 64),
                        safe(&row.state, 16),
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
    // AN ASK PAST ITS DEADLINE WITH NO REPLY (R8). The bus is the authority:
    // a reply the asker's endpoint dropped is still a reply, and an ask whose
    // asker's bridge has already recorded `expired` is still unanswered — the
    // line says which of the two states it is in.
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
                "not yet recorded expired by the asker's bridge (its tick does that; a bridge \
                 older than round 15 never will)"
            }
        ));
    }
    // A LIVE presence row for this machine's node that no instance here hosts
    // routes mail to nowhere: every post to it comes back undeliverable.
    let answered = every_instance_answered(r);
    let local_nodes = local_nodes(&r.instances, r.node.as_deref());
    if answered {
        // ON THE `bus` COLUMN, NOT ON `pid` ALONE. A row with no local host used
        // to mean exactly one thing — the bus advertises it LIVE and nobody
        // serves it — because a row whose `state=` was anything else was counted
        // into `exited` and never listed. Round 21 started LISTING a retired one
        // as `gone` (see [`join_sessions`]), so `pid.is_none()` now also matches
        // every session that ended normally in the last hour, and this warning
        // fired for all of them: the sentence says "advertises @<sid> live",
        // which for a `gone` row is simply false.
        for s in ghost_rows(&r.sessions, &local_nodes) {
            w.push(format!(
                "the bus advertises @{} live on {} — this machine's node — but no \
                 aterm instance here hosts it: mail to it is undeliverable \
                 (`aterm fabric doctor --retire-ghosts` publishes `exited` for it)",
                safe(&s.sid, 64),
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
    w
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

/// The local clock's offset from UTC in seconds, from `date +%z`; `None` when it
/// cannot be run, and then every time prints in UTC and says so.
fn local_offset() -> Option<i64> {
    static OFFSET: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = Command::new("date")
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

/// A record's time at the clock's offset: `HH:MM:SS` within a day of `now`,
/// `MM-DDTHH:MM` beyond it, `-` when the record carried none. Always ONE token,
/// so a `tail` line splits on whitespace into the same columns every time.
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
fn zone_label(offset: Option<i64>) -> String {
    match offset {
        None => "times UTC".to_string(),
        Some(o) => {
            let sign = if o < 0 { '-' } else { '+' };
            let o = o.abs();
            format!("times UTC{sign}{:02}:{:02}", o / 3600, (o % 3600) / 60)
        }
    }
}

/// An aligned table: two-space gutters, the LAST column unpadded, and the
/// columns flagged in `right` right-aligned.
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

/// The TRAFFIC / `tail` columns of one record, escaped.
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

/// The TRAFFIC / `tail` header.
const TRAFFIC_HEADER: [&str; 7] = ["OFFSET", "TIME", "KIND", "LEN", "TRUST", "FROM", "TO"];

/// A session's role: the local instance's `meta role=` when it hosts the
/// session and one is set, else the bus row's `role=`, else `-`.
fn session_role(s: &SessionRow) -> &str {
    s.local
        .as_ref()
        .map(|l| l.role.as_str())
        .filter(|r| *r != "-")
        .or(s.bus_role.as_deref())
        .unwrap_or("-")
}

/// A session's title, the same way: local `meta title=` first.
fn session_title(s: &SessionRow) -> &str {
    s.local
        .as_ref()
        .map(|l| l.title.as_str())
        .filter(|t| *t != "-" && !t.is_empty())
        .or(s.bus_title.as_deref())
        .unwrap_or("-")
}

/// Which node a NODES row is, from where this report stands: `this` — the
/// node the command's state dir names; `local` — another instance on this
/// machine answers as it (its own state dir); `remote` — only the bus knows it.
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

/// Publish `state=exited` for each `(sid, node)`, by REWRITING the row that is
/// already there.
///
/// Only `state=` and `t=` change. `inc=`, `epoch=` and `gen=` are carried
/// through from the live row, because those are the bridge's facts and a
/// retirement is not the place to invent them — a row whose `inc=` this command
/// guessed would be a row the next bridge cannot reason about
/// ([`crate::bridge::adoptable`]).
///
/// The sequence comes from the state dir, in the CURRENT incarnation's range,
/// and is reserved durably before the publish. That is safe only because the
/// caller refused when a bridge was running; see `enable::retire_ghosts_action`.
///
/// # Errors
///
/// The broker, the caps, the state dir, or a row that is no longer on the bus.
pub fn publish_exited(report: &Report, ghosts: &[(String, String)]) -> io::Result<usize> {
    let cfg = &report.cfg;
    let node = report
        .node
        .as_deref()
        .ok_or_else(|| io::Error::other("this state dir has no node id"))?;
    let state = crate::state::StateDir::open(&cfg.state_dir)?;
    let (mut conn, _closer) = transport::connect(&cfg.transport, &cfg.broker)?;
    for path in &cfg.cap_files {
        for cap in read_cap_file(path)? {
            conn.attach(&cap.grant, &cap.tag)?;
        }
    }
    // The rows as they stand, so the retirement carries their own fields.
    let mut live: BTreeMap<String, String> = BTreeMap::new();
    let filter = format!("/f/{}/pub/{}/*/presence", cfg.fleet, node);
    transport::walk_last(&mut conn, &filter, |(_, subject, raw)| {
        if let Some(sid) = subject.split('/').nth(5) {
            live.insert(sid.to_string(), String::from_utf8_lossy(raw).into_owned());
        }
        Ok(())
    })?;

    let producer_id = astream_cap::producer_id_of(node);
    let base = state.incarnation() << 32;
    let mut seq = state.sequence().max(base);
    let mut done = 0;
    for (sid, _) in ghosts {
        let Some(body) = live.get(sid) else { continue };
        let rewritten = retired_body(body);
        let subject = crate::subject::session_face(&cfg.fleet, node, sid, "presence");
        seq += 1;
        state.reserve_sequence(seq)?;
        let (off, deduped) = conn.publish(producer_id, seq, &subject, rewritten.as_bytes())?;
        // A DEDUPED PUBLISH IS A FAILURE HERE. The broker answers a re-sent
        // `(producer_id, seq)` with the original offset and appends nothing, so
        // the row would still read `live` while this reported success — the
        // exact silent no-op the running-bridge refusal exists to prevent, and
        // it must not pass quietly if some other writer got there anyway.
        if deduped {
            return Err(io::Error::other(format!(
                "the broker deduped the retirement of @{sid} at @{off}: this node's \
                 sequence {seq} was already used — is a bridge running?"
            )));
        }
        done += 1;
    }
    Ok(done)
}

/// The `state=` of each named session's presence row under this node, read
/// off the bus with the cap files' grants — the doctor's read-back after it
/// asked a bridge to retire. A sid with no row is absent from the map.
///
/// # Errors
///
/// The broker or the caps.
pub fn presence_states(report: &Report, sids: &[&str]) -> io::Result<BTreeMap<String, String>> {
    let cfg = &report.cfg;
    let node = report
        .node
        .as_deref()
        .ok_or_else(|| io::Error::other("this state dir has no node id"))?;
    let (mut conn, _closer) = transport::connect(&cfg.transport, &cfg.broker)?;
    for path in &cfg.cap_files {
        for cap in read_cap_file(path)? {
            conn.attach(&cap.grant, &cap.tag)?;
        }
    }
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
    Ok(out)
}

/// One presence body with `state=` set to `exited` and `t=` refreshed; every
/// other token kept in place. Shared with the bridge's operator-requested
/// retire, so the two paths write the same row.
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

/// Whether EVERY local instance was reached and answered.
///
/// One definition, because two things must agree about it: the `status`
/// warning that names a ghost row, and `doctor --retire-ghosts` that retires
/// one. An instance that did not answer (no Owner token, a connect error, a
/// `fabric status` that failed) is kept in the report with `error` set and NO
/// sessions — so every live row on this node reads unhosted, and a retire that
/// ignored this would publish `exited` for rows that instance is hosting
/// perfectly well.
pub(crate) fn every_instance_answered(r: &Report) -> bool {
    r.discovery_error.is_none() && r.instances.iter().all(|i| i.error.is_none())
}

/// Which instances did NOT answer, as `pid: why`, for a refusal that names them.
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

/// THE GHOSTS: presence rows this machine's node advertises LIVE that no local
/// instance hosts.
///
/// ONE DEFINITION, because two things act on it — the `status` warning that
/// names them and `aterm fabric doctor --retire-ghosts` that retires them — and
/// a doctor that retired a row the warning did not name (or the reverse) would
/// be the worst kind of repair tool. `bus == "live"` is load-bearing: round 21
/// started LISTING a retired row as `gone`, and a predicate on `pid.is_none()`
/// alone would sweep up every session that ended normally in the last hour.
pub(crate) fn ghost_rows<'r>(
    sessions: &'r [SessionRow],
    local: &BTreeSet<&str>,
) -> Vec<&'r SessionRow> {
    sessions
        .iter()
        .filter(|s| s.pid.is_none() && s.bus == "live" && local.contains(s.node.as_str()))
        .collect()
}

/// EVERY SESSION ANY LOCAL INSTANCE HOSTS, by pid — the node's hosted set, read
/// the way the report reads it (each instance's own `sessions`). `Err` when an
/// instance could not be asked: a retire gated on this set must then refuse,
/// because the session it would retire may be that instance's.
pub(crate) fn node_hosted() -> Result<BTreeMap<String, u32>, String> {
    let listed = aterm_ctl::local_instances().map_err(|e| e.reason)?;
    let mut hosted = BTreeMap::new();
    for (pid, sock) in listed {
        let token = match aterm_ctl::instance_token(&sock) {
            Ok(t) => t,
            Err(_) if !aterm_uds::process::pid_alive(pid) => continue,
            Err(e) => return Err(format!("instance {pid}: no Owner token: {e}")),
        };
        let mut ctl = match Ctl::connect(&sock, &token) {
            Ok(c) => c,
            Err(e)
                if e.kind() == io::ErrorKind::ConnectionRefused
                    && !aterm_uds::process::pid_alive(pid) =>
            {
                continue;
            }
            Err(e) => return Err(format!("instance {pid}: connect: {e}")),
        };
        let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
        let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
        let reply = ask(&mut ctl, "sessions").map_err(|e| format!("instance {pid}: {e}"))?;
        for row in reply.rows() {
            if let Some(sid) = row
                .split(' ')
                .nth(1)
                .filter(|s| crate::subject::is_principal(s))
            {
                hosted.insert(sid.to_string(), pid);
            }
        }
    }
    Ok(hosted)
}

/// Why a presence row may not be retired on an operator's request: the door
/// the request came through is one instance's, the ghost list is the node's.
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

/// The nodes this machine speaks for: every local instance's, plus this state
/// dir's own.
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

/// How many LIVE sessions the roster advertises on `node`.
#[must_use]
pub fn live_sessions_on(r: &Report, node: &str) -> usize {
    r.sessions
        .iter()
        .filter(|s| s.node == node && s.bus == "live")
        .count()
}

/// The NODES section (round 16): every node on the bus's presence roster —
/// its `host=`, `state=` and `fabric=` as the node itself published them, and
/// how many live sessions it advertises — `this` node first. A second host
/// joined over the sealed wire is a `remote` row here, and its sessions are in
/// SESSIONS under its node. This node with no presence row on the bus is a row
/// too, saying so: absence is the finding.
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
                    "host={} state={} fabric={}",
                    safe(&n.host, 64),
                    safe(&n.state, 16),
                    safe(&n.fabric, 16)
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
    out
}

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
            "on (the default; inbox seen handled|refused|deferred on an ask/task acks the \
             sender — `[fabric] receipts = false` or `--no-receipts` turns it off)"
                .to_string()
        } else {
            "off — TURNED OFF HERE (no ack reaches a sender, so their `post --wait-ack` and \
             `ask` wait out the whole deadline; remove `[fabric] receipts = false` or \
             `--no-receipts` to restore the default)"
                .to_string()
        },
    ));
    push_kvs(&mut out, &kvs);

    out.push_str("\nBROKER\n");
    let b = &r.broker;
    let mut kvs: Vec<(&str, String)> = vec![(
        if b.transport == "unix" {
            "socket"
        } else {
            "endpoint"
        },
        format!(
            "{}{}",
            safe(&b.endpoint, 256),
            if b.transport == "unix" {
                String::new()
            } else {
                format!(" ({})", b.transport)
            }
        ),
    )];
    if let Some(tcp) = &b.serves_tcp {
        kvs.push((
            "serves",
            format!(
                "{} (tcp+sealed) too — the one port a joining host dials; this host's own \
                 bridges use the socket above",
                safe(tcp, 128)
            ),
        ));
    }
    kvs.push((
        "reachable",
        if b.read_ok() {
            format!(
                "yes — connect, hello, attach and head query answered in {} ms",
                b.rtt_ms.unwrap_or(0)
            )
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
        match (b.pid, &b.launchd) {
            (Some(p), Some(l)) => format!("{p} (launchd {})", safe(l, 128)),
            (Some(p), None) => format!("{p}"),
            (None, _) => "- (no `link broker` process serves this socket)".to_string(),
        },
    ));
    kvs.push((
        "bus head",
        b.head.map_or_else(|| "-".to_string(), |h| format!("@{h}")),
    ));
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
                    safe(&n.state, 16),
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
        ]);
    }
    // Another node's row is in NODES, above: this table is this machine's
    // instances, one row each, as its header says.
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
            ],
            &rows,
            &[true, false, false, true, false, false],
        );
    }

    out.push_str(
        "\nSESSIONS  unread = listed, not handled · pending = delivered, never listed · \
         posts = queued outbound\n",
    );
    // THE NODE COLUMN whenever the fleet has more than one node — on the bus
    // or among these rows — so every node's sessions say whose they are.
    let many_nodes = r.nodes.len() > 1
        || r.sessions
            .iter()
            .map(|s| &s.node)
            .collect::<BTreeSet<_>>()
            .len()
            > 1;
    // A row no instance here hosts is REMOTE when another node advertises it,
    // and `-` when this machine's own node does (a WARNING says why).
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
            // THE BROADCAST OPT-INS. `-` is "this session hears no broadcast",
            // which is the default and the answer an operator needs when a
            // shout reached nobody: the topic set is the only thing that
            // decides, and it is invisible everywhere else in this report.
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
        out.push_str("\nWARNINGS  none — the broker answers, every bridge is supervised and connected, and no mail is dropped, held or waiting\n");
    } else {
        out.push_str(&format!("\nWARNINGS  {}\n", r.warnings.len()));
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

// --- JSON -------------------------------------------------------------------

/// A JSON value, rendered by hand: this crate carries no JSON writer and the
/// shape is small. Strings go through `glance`'s escaper, so a value that came
/// off the bus can never close its string or carry a control byte.
enum J {
    Null,
    Bool(bool),
    Num(u64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    fn s(v: &str) -> J {
        J::Str(v.to_string())
    }
    fn opt_s(v: Option<&str>) -> J {
        v.map_or(J::Null, J::s)
    }
    fn opt_n(v: Option<u64>) -> J {
        v.map_or(J::Null, J::Num)
    }
    fn render(&self, out: &mut String) {
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
                ("command", J::opt_s(i.command.as_deref())),
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
                // `null` for a row no instance here hosts, like its siblings:
                // `[]` would claim the session opted into nothing.
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
                ("fabric", J::s(&n.fabric)),
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
                ("command", J::s(&r.command)),
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
                ("transport", J::s(b.transport)),
                ("reachable", J::Bool(b.reachable)),
                ("attached", J::Bool(b.attached)),
                ("error", J::opt_s(b.error.as_deref())),
                ("pid", J::opt_n(b.pid.map(u64::from))),
                ("launchd", J::opt_s(b.launchd.as_deref())),
                ("head", J::opt_n(b.head)),
                ("rtt_ms", J::opt_n(b.rtt_ms)),
                ("serves_tcp", J::opt_s(b.serves_tcp.as_deref())),
                (
                    "faces",
                    b.faces
                        .as_ref()
                        .map_or(J::Null, |f| J::Arr(f.iter().map(|x| J::s(x)).collect())),
                ),
            ]),
        ),
        ("bridges", J::Arr(bridges)),
        ("discovery_error", J::opt_s(r.discovery_error.as_deref())),
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

/// Write to stdout, tolerating a reader that went away (`| head`).
fn emit(text: &str) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn status_main(json: bool) -> ExitCode {
    match gather() {
        Ok(r) => {
            emit(&if json {
                render_json(&r)
            } else {
                render_text(&r)
            });
            ExitCode::from(exit_of(&r))
        }
        Err(off) => {
            if json {
                emit(&render_off_json(&off));
            } else {
                emit(&format!("{}\n", off.message()));
            }
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// tail
// ---------------------------------------------------------------------------

/// One `tail` line. The widths are fixed, because a live tail cannot know the
/// widest row to come; the principals are last so a long one only pushes
/// what follows it. With `--bodies` the text comes LAST, after the trust label
/// — what a record IS must reach the reader before what it says. (`tui.rs`
/// ordered its transcript the same way and for the same reason, until round 21
/// deleted the module; `aterm-gui`'s presence band still does.)
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
    let resolved = match resolve_command(
        std::env::var(FABRIC_COMMAND_ENV).ok().as_deref(),
        config_path().as_deref(),
    ) {
        Ok(Some((_, c))) => c,
        Ok(None) => {
            // The same fallbacks `status` takes: the rendezvous file, then a
            // hand-armed running bridge.
            let from_rendezvous = match rendezvous_command() {
                Ok(r) => r.map(|(_, c)| c),
                Err(e) => {
                    eprintln!("{}", Off::Unreadable(e).message());
                    return ExitCode::from(2);
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
                    return ExitCode::from(2);
                }
            }
        }
        Err(e) => {
            eprintln!("{}", Off::Unreadable(e).message());
            return ExitCode::from(2);
        }
    };
    let cfg = match bridge_config(&resolved) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", Off::Unreadable(e).message());
            return ExitCode::from(2);
        }
    };
    match tail(&cfg, bodies, from, filter) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("aterm fabric tail: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Whether `f` is a SUBJECT FILTER this command will subscribe to.
///
/// Shape only, and deliberately: the fleet segment is checked in [`tail`],
/// where the config has actually been read, and what the caps grant is the
/// broker's answer to give. What is checked here is that the string is a
/// subject pattern at all — anchored at `/f/`, no empty segment, and nothing
/// outside the characters a subject segment and the two wildcards are made of.
/// A filter is written into the operator's own terminal beside every record it
/// matches, so an unbounded one is a line that can carry a control sequence.
fn is_subject_filter(f: &str) -> bool {
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

/// Follow the bus from `from` (or the head) until the broker closes the stream
/// or stdout goes away.
fn tail(cfg: &Config, bodies: bool, from: Option<u64>, want: Option<&str>) -> io::Result<()> {
    let broker = safe(&cfg.broker, 256);
    let (mut conn, _closer) = transport::connect(&cfg.transport, &cfg.broker)
        .map_err(|e| io::Error::new(e.kind(), format!("the broker at {broker}: {e}")))?;
    for path in &cfg.cap_files {
        for cap in read_cap_file(path)? {
            conn.attach(&cap.grant, &cap.tag)?;
        }
    }
    let whole = match want {
        // A FILTER NARROWS THE SUBSCRIPTION, it does not widen the CAPS.
        //
        // What it must not do is leave the fleet: this command reads one
        // fleet's bus, its config names which, and a filter pointing at another
        // one would print records under a heading that says otherwise. What it
        // may freely do is name a subtree the caps do not cover — the broker
        // refuses that, which is the broker's job and its answer is the honest
        // one. The guarded-broker fallback below is NOT taken for a narrowed
        // tail: a refusal there means the filter is outside the grant, and
        // silently following the granted faces instead would answer a different
        // question than the one asked.
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
    // A GUARDED broker whose grants do not cover the whole fleet (the sealed
    // cross-host one, under a node's ring) refuses `/f/<F>/>`: the tail then
    // follows every face the caps DO grant, one subscription each, in arrival
    // order (see `tail_faces`).
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
    while let Some((off, subject, raw)) = sub.recv()? {
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

/// `tail` over SEVERAL granted faces — one connection and one subscription
/// each, every record forwarded to this thread and printed as it arrives. A
/// record two faces both match (a node's post to one of its own sessions is
/// on its write lane and its read lane) is printed once: the offsets printed
/// are remembered, and forgotten below the lowest one still in flight so the
/// memory is bounded by how far the faces are apart, not by the log.
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
    let (tx, rx) = mpsc::channel::<io::Result<(u64, String, Vec<u8>)>>();
    for face in faces {
        let (mut conn, _closer) = transport::connect(&cfg.transport, &cfg.broker)?;
        for path in &cfg.cap_files {
            for cap in read_cap_file(path)? {
                conn.attach(&cap.grant, &cap.tag)?;
            }
        }
        let mut sub = conn.subscribe(start, face)?;
        let tx = tx.clone();
        std::thread::spawn(move || loop {
            match sub.recv() {
                Ok(Some(rec)) => {
                    if tx.send(Ok(rec)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = tx.send(Err(io::Error::other("closed")));
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
        let Ok((off, subject, raw)) = next else {
            break;
        };
        if !printed.insert(off) {
            continue;
        }
        // BOUNDED: keep the newest 4096 offsets, which is far more than two
        // faces of one log can be apart.
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

    /// **THE GHOSTS ARE EXACTLY THE LIVE-BUT-UNHOSTED ROWS ON OUR OWN NODE.**
    /// A `gone` row is not one (round 21 started listing those, and a predicate
    /// on `pid.is_none()` alone would sweep up every session that ended in the
    /// last hour); a row on a REMOTE node is not one (we cannot speak for it);
    /// and a row a local instance hosts is not one.
    /// The door is per-instance and the ghost list per-node: a sid a SIBLING
    /// instance hosts is refused with that instance's pid, and a node that
    /// could not be asked refuses everything.
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

    /// A retirement rewrites `state=` and `t=` and NOTHING else: `inc=`,
    /// `epoch=` and `gen=` are the bridge's facts, and a row whose `inc=` this
    /// command invented is a row the next bridge cannot reason about.
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
        // A row with neither token still comes out retired and stamped.
        let bare = retired_body("v=1 inc=2");
        assert!(
            bare.contains("state=exited") && bare.contains("t="),
            "{bare}"
        );
    }
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    /// THE GRAMMAR: no arguments is `status`, the flags belong to their own
    /// subcommand, and anything else is refused BY NAME — including the front
    /// door's routing probe, which must get this tool's usage error and nothing
    /// else.
    #[test]
    fn the_grammar_is_status_by_default_and_refuses_what_it_does_not_know() {
        assert_eq!(parse_args(&[]), Ok(Cmd::Status { json: false }));
        assert_eq!(parse_args(&argv("status")), Ok(Cmd::Status { json: false }));
        assert_eq!(parse_args(&argv("--json")), Ok(Cmd::Status { json: true }));
        assert_eq!(
            parse_args(&argv("status --json")),
            Ok(Cmd::Status { json: true })
        );
        assert_eq!(
            parse_args(&argv("tail")),
            Ok(Cmd::Tail {
                bodies: false,
                from: None,
                filter: None
            })
        );
        assert_eq!(
            parse_args(&argv("tail --bodies --from @12")),
            Ok(Cmd::Tail {
                bodies: true,
                from: Some(12),
                filter: None
            })
        );
        assert_eq!(
            parse_args(&argv("tail --from 7")),
            Ok(Cmd::Tail {
                bodies: false,
                from: Some(7),
                filter: None
            })
        );
        assert_eq!(
            parse_args(&argv("tail --filter /f/lab/pub/*/*/say/>")),
            Ok(Cmd::Tail {
                bodies: false,
                from: None,
                filter: Some("/f/lab/pub/*/*/say/>".to_string())
            })
        );
        // NOT A SUBJECT FILTER — each refused at the parse, before a broker is
        // dialled. The last two are the ones that matter: a filter is echoed
        // into the operator's terminal beside every record it matches, so a
        // string that can carry an escape is a string that can paint one.
        for bad in [
            "pub/>",
            "/f//pub/>",
            "/f/lab/pub/ /say",
            "/f/lab/\u{1b}[2J",
            "/f/lab/a;rm -rf /",
        ] {
            let got = parse_args(&[
                "tail".to_string(),
                "--filter".to_string(),
                (*bad).to_string(),
            ]);
            assert!(
                got.as_ref().is_err_and(|e| e.contains("--filter")),
                "{bad}: {got:?}"
            );
        }
        // …and it belongs to `tail` alone.
        assert!(
            parse_args(&argv("status --filter /f/lab/>")).is_err_and(|e| e.contains("`tail` flag"))
        );
        assert!(
            parse_args(&argv("doctor --filter /f/lab/>")).is_err_and(|e| e.contains("`tail` flag"))
        );

        for help in [
            "help",
            "-h",
            "--help",
            "status --help",
            "tail -h",
            "on --help",
        ] {
            assert_eq!(parse_args(&argv(help)), Ok(Cmd::Help), "{help}");
        }
        // `on`, `off` and `doctor`, with their flags on their own verbs only.
        use crate::enable::{OffOpts, OnOpts, Service};
        assert_eq!(parse_args(&argv("on")), Ok(Cmd::On(OnOpts::default())));
        assert_eq!(
            parse_args(&argv("on --dry-run --service none --fleet lab")),
            Ok(Cmd::On(OnOpts {
                dry_run: true,
                service: Some(Service::None),
                fleet: Some("lab".to_string()),
                tcp: None,
                key_file: None,
                allow_remote: false,
            }))
        );
        assert_eq!(
            parse_args(&argv("on --tcp 0.0.0.0:7000 --key-file /k --allow-remote")),
            Ok(Cmd::On(OnOpts {
                tcp: Some("0.0.0.0:7000".to_string()),
                key_file: Some("/k".to_string()),
                allow_remote: true,
                ..OnOpts::default()
            })),
            "parsed here; `on` itself refuses them in a default build, naming the feature"
        );
        // ROUND 16: `mint-for` and `join`, each with its own flags only.
        use crate::join::{JoinOpts, MintForOpts};
        assert_eq!(
            parse_args(&argv("mint-for n-m7 --out /tmp/m7.cap")),
            Ok(Cmd::MintFor(MintForOpts {
                node: "n-m7".to_string(),
                out: Some("/tmp/m7.cap".to_string()),
                fleet: None,
            }))
        );
        assert_eq!(
            parse_args(&argv("mint-for new")),
            Ok(Cmd::MintFor(MintForOpts {
                node: "new".to_string(),
                ..MintForOpts::default()
            }))
        );
        assert_eq!(
            parse_args(&argv(
                "join --broker h1:7000 --tcp --key-file /k --cap-file /c --node n-m7 \
                 --accept-from n-h1,h-andrew --dry-run --service none"
            )),
            Ok(Cmd::Join(JoinOpts {
                broker: Some("h1:7000".to_string()),
                tcp: true,
                key_file: Some("/k".to_string()),
                cap_file: Some("/c".to_string()),
                node: Some("n-m7".to_string()),
                accept_from: vec!["n-h1".to_string(), "h-andrew".to_string()],
                dry_run: true,
                service: Some(Service::None),
            }))
        );
        assert_eq!(
            parse_args(&argv("off --dry-run --service launchd")),
            Ok(Cmd::Off(OffOpts {
                dry_run: true,
                service: Some(Service::Launchd),
            }))
        );
        assert_eq!(
            parse_args(&argv("doctor")),
            Ok(Cmd::Doctor {
                retire_ghosts: false,
                yes: false
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
        assert_eq!(
            parse_args(&argv("doctor --retire-ghosts --yes")),
            Ok(Cmd::Doctor {
                retire_ghosts: true,
                yes: true
            })
        );
        // The flags are the DOCTOR's, not every verb's.
        assert!(parse_args(&argv("status --retire-ghosts")).is_err());
        assert!(parse_args(&argv("tail --yes")).is_err());
        // AND `--yes` ANSWERS A QUESTION ONLY `--retire-ghosts` ASKS. Parsed
        // silently it read as "yes to whatever doctor does", which is nothing:
        // an operator who meant to retire would read a clean report and
        // believe it had run.
        assert!(
            parse_args(&argv("doctor --yes")).is_err(),
            "--yes alone must be a usage error, not a silent no-op"
        );
        for bad in [
            "--aterm-front-door-routing-probe",
            "status --bodies",
            "status --from 3",
            "tail --json",
            "tail --from",
            "tail --from x",
            "stats",
            "status --dry-run",
            "tail --service none",
            "on --service cron",
            "on --fleet LAB",
            "on --json",
            "off --fleet lab",
            "doctor --dry-run",
            "mint-for",
            "mint-for n-a n-b",
            "mint-for n-a --broker x:1",
            "join --out /c",
            "on --broker x:1",
            "on --cap-file /c",
            "off --allow-remote",
            "status --accept-from n-a",
        ] {
            assert!(parse_args(&argv(bad)).is_err(), "{bad} must be refused");
        }
    }

    /// CONFIG PARSING: `[fabric] command` in every TOML spelling the app's own
    /// parser accepts, blank as off, and a wrong type as unreadable.
    #[test]
    fn the_command_is_read_from_every_toml_spelling_and_blank_is_off() {
        let cmd = "aterm link serve --fleet f --broker /tmp/b.sock";
        for text in [
            format!("[fabric]\ncommand = \"{cmd}\"\n"),
            format!("fabric.command = \"{cmd}\"\n"),
            format!("fabric = {{ command = \"{cmd}\" }}\n"),
            format!("font_px = 12.0\n\n[fabric]\n# a comment\ncommand = '{cmd}'\n\n[keys]\n"),
            format!("[fabric]\ncommand = \"\"\"{cmd}\"\"\"\n"),
        ] {
            assert_eq!(command_in_toml(&text), Ok(Some(cmd.to_string())), "{text}");
        }
        for off in [
            "",
            "font_px = 1.0\n",
            "[fabric]\n",
            "[fabric]\ncommand = \"   \"\n",
        ] {
            assert_eq!(command_in_toml(off), Ok(None), "{off:?}");
        }
        for bad in ["[fabric]\ncommand = 3\n", "fabric = 1\n", "[fabric\n"] {
            assert!(command_in_toml(bad).is_err(), "{bad:?}");
        }
    }

    /// `[fabric] presence` has two spellings, an absent key is the default,
    /// and a wrong one is an error the bridge turns into `meta` with a word on
    /// stderr rather than a refusal to start.
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

    /// **THE TOPICS COLUMN IS WHY A BROADCAST REACHED NOBODY.**
    ///
    /// A `post to=say:<t>` that lands on the bus and is delivered to nobody
    /// looks, from the sender's side, exactly like one that was never sent: the
    /// `post` answers `OK`, the record is on the log, and no inbox grew. The
    /// receiver-side opt-in is the only thing that decides, and this column is
    /// the only place in the report that shows it. `-` is a session that hears
    /// no broadcast at all, which is the default and is the answer an operator
    /// is looking for most of the time.
    #[test]
    fn sessions_show_the_broadcast_topics_and_a_dash_for_none() {
        let mut r = healthy();
        r.sessions.clear();
        let mut listening = r.instances[0].sessions[0].clone();
        listening.sid = "s-ears".to_string();
        listening.topics = vec!["build.failed".to_string(), "sat-comp".to_string()];
        r.instances[0].sessions.push(listening);
        join_sessions(&mut r, None);
        let text = render_text(&r);
        assert!(
            text.contains("TOPICS"),
            "the SESSIONS table names the column: {text}"
        );
        let ears = text
            .lines()
            .find(|l| l.contains("s-ears"))
            .expect("the subscribed row");
        assert!(
            ears.contains("build.failed,sat-comp"),
            "the opt-ins are shown: {ears}"
        );
        let deaf = text
            .lines()
            .find(|l| l.contains("s-one"))
            .expect("the unsubscribed row");
        assert!(
            !deaf.contains("build.failed"),
            "a session that asked for nothing shows nothing: {deaf}"
        );
        let json = render_json(&r);
        assert!(
            json.contains("\"topics\":[\"build.failed\",\"sat-comp\"]"),
            "{json}"
        );
        assert!(json.contains("\"topics\":[]"), "{json}");
        // A ROW NO INSTANCE HERE HOSTS is `null`, like its sibling fields:
        // `[]` would claim the session opted into nothing.
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

    /// **SESSIONS SHOWS WHAT THE ROW MEANS**: a bus row's `role= detail=
    /// phase= context= title=` land in the columns and the JSON — the local
    /// `meta` winning for role and title when the instance hosts the session,
    /// the bus row filling them in for a remote one — and a row from an older
    /// bridge reads `-`.
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
        let text = render_text(&r);
        // The local session: its own `meta` title (`work`) over the bus's,
        // the bus's detail, phase and context.
        let local = text
            .lines()
            .find(|l| l.contains("s-one"))
            .expect("the local row");
        for col in ["bus-role", "claude", "busy", "12%", "work"] {
            assert!(local.contains(col), "{col} missing from: {local}");
        }
        assert!(!local.contains("bus title"), "{local}");
        // The remote one: everything off the bus row.
        let remote = text
            .lines()
            .find(|l| l.contains("s-far"))
            .expect("the remote row");
        for col in ["remote", "bus-role", "claude", "busy", "12%", "bus title"] {
            assert!(remote.contains(col), "{col} missing from: {remote}");
        }
        // The older bridge's row: dashes, never a panic.
        let old = text
            .lines()
            .find(|l| l.contains("s-old"))
            .expect("the old row");
        assert!(old.contains("  -  "), "{old}");
        assert!(text.contains("presence   meta ("), "{text}");
        let json = render_json(&r);
        assert!(json.contains("\"phase\":\"busy\""), "{json}");
        assert!(json.contains("\"context\":12"), "{json}");
        assert!(json.contains("\"detail\":\"claude\""), "{json}");
        assert!(json.contains("\"presence\":\"meta\""), "{json}");
        assert!(json.contains("\"title\":\"bus title\""), "{json}");
    }

    /// `ATERM_FABRIC_COMMAND` WINS over the file, as it does for the app; a
    /// blank one is absent; a missing file is "off", not an error.
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
        assert!(c.contains("--fleet file"));
        let (src, c) = resolve_command(Some("aterm-link serve --fleet env"), Some(&path))
            .expect("ok")
            .expect("some");
        assert_eq!(src, Source::Env);
        assert!(c.contains("--fleet env"));
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

    /// BOTH SPELLINGS OF THE BRIDGE: `aterm link serve` (what
    /// tools/fabric-enable.sh writes) and `aterm-link serve` (the argv0 alias
    /// every harness writes) — with any program path in front.
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
            let cfg = bridge_config(&cmd).expect(&cmd);
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
            assert!(serve_flags(not_a_bridge).is_none(), "{not_a_bridge}");
            assert!(bridge_config(not_a_bridge).is_err());
        }
        // The bridge's own parser refuses what the bridge would refuse.
        assert!(bridge_config("aterm link serve --fleet lab").is_err());
    }

    /// THE PROCESS TABLE finds the broker by the socket it serves, and a bridge
    /// by `link serve`, in either spelling.
    #[test]
    fn the_process_table_names_the_broker_and_the_bridges() {
        let ps = "\
    1     0 /sbin/launchd
48078     1 /Users//u/.local/bin/aterm link broker /Users//u/f/bus.sock /Users//u/f/bus.log
15678     1 /x/target/debug/aterm-link broker /tmp/other.sock /tmp/other.log
48224 66439 /Users//u/.local/bin/aterm link serve --fleet local --broker /Users//u/f/bus.sock
15792 15335 /x/aterm-link serve --fleet t --broker /tmp/other.sock
  900   800 vim notes-about-aterm-link-serve.txt
";
        let procs = parse_ps(ps);
        assert_eq!(procs.len(), 6);
        assert_eq!(broker_pid(&procs, "/Users//u/f/bus.sock"), Some(48078));
        assert_eq!(broker_pid(&procs, "/tmp/other.sock"), Some(15678));
        assert_eq!(broker_pid(&procs, "/tmp/none.sock"), None);
        let bridges: Vec<(u32, u32)> = bridge_procs(&procs)
            .iter()
            .map(|p| (p.pid, p.ppid))
            .collect();
        assert_eq!(bridges, vec![(48224, 66439), (15792, 15335)]);
        let launchctl =
            "PID\tStatus\tLabel\n-\t0\tcom.apple.x\n48078\t0\tsystems.alab.astream-broker\n";
        assert_eq!(
            parse_launchctl(launchctl, 48078).as_deref(),
            Some("systems.alab.astream-broker")
        );
        assert_eq!(parse_launchctl(launchctl, 1), None);
    }

    /// THE INBOX NUMBERS: pending and dropped come off the header of the
    /// zero-row peek, posts from its post rows, and unread is exact.
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

    /// THE HOLD'S REASON comes from the session's own timeline, and only a
    /// transition that put the hold ON names one.
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
    }

    /// TRAFFIC IS METADATA: every face classifies, trust comes from the address,
    /// and the body's text is carried for `--bodies` but is never a column.
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

        // A human's word is `human`; a relayed one is `relayed`, whoever it was.
        let h = traffic_of("lab", 5, "/f/lab/in/n-a/s-w/h-andrew/ask", b"v=1 t=1");
        assert_eq!((h.from.as_str(), h.trust), ("h-andrew", "human"));
        let r = traffic_of(
            "lab",
            6,
            "/f/lab/in/n-a/s-w/h-andrew/ask",
            b"v=1 t=1 via=n-b",
        );
        assert_eq!(r.trust, "relayed");
        // Only a NODE may attest a session, and only a session.
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
        // A BROADCAST: eight segments, `to=` is the face a reader subscribes
        // to, and the kind comes out of the body — so a `task` shout is not a
        // note. (The old arm expected seven segments and never matched.)
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
        // The seven-segment `/pub/<owner>/say/<kind>` this used to pin was a
        // shape nothing ever published; it renders through the generic `pub`
        // arm like any other unknown leaf.
        let odd = traffic_of("lab", 9, "/f/lab/pub/h-andrew/say/note", b"v=1 t=2");
        assert_eq!((odd.to.as_str(), odd.kind.as_str()), ("-", "note"));
        let key = traffic_of("lab", 10, "/f/lab/term/n-a/s-w/in/h-andrew", b"v=1");
        assert_eq!(
            (key.from.as_str(), key.kind.as_str(), key.trust),
            ("h-andrew", "term/in", SCREEN)
        );
        // A face this build does not know renders as its name, labelled the most
        // conservative way; a subject outside the fleet renders as `unknown`.
        let odd = traffic_of("lab", 11, "/f/lab/zzz/q", b"");
        assert_eq!((odd.kind.as_str(), odd.trust), ("zzz", SCREEN));
        let alien = traffic_of("lab", 11, "/f/other/in/n-a/s-w/n-a/note", b"");
        assert_eq!((alien.kind.as_str(), alien.trust), ("unknown", SCREEN));

        // NO SEGMENT MOVES THE CURSOR: a hostile subject renders escaped.
        let hostile = traffic_of("lab", 12, "/f/lab/in/n-a/s-w/n-\u{1b}[2J/note", b"v=1");
        let line = tail_line(&hostile, 0, 0, true);
        assert!(!line.contains('\u{1b}'), "{line:?}");
    }

    /// THE CLOCK: local wall time within a day, a date beyond it, `-` for none.
    #[test]
    fn times_print_as_a_human_reads_them() {
        assert_eq!(parse_utc_offset("-0700"), Some(-7 * 3600));
        assert_eq!(parse_utc_offset("+0530"), Some(5 * 3600 + 30 * 60));
        assert_eq!(parse_utc_offset("UTC"), None);
        // 2026-09-14T16:44:03Z
        let t = 1_789_404_243_000;
        assert_eq!(clock(t, t, 0), "16:44:03");
        assert_eq!(clock(t, t, -7 * 3600), "09:44:03");
        assert_eq!(clock(t, t + 3 * 86_400_000, 0), "09-14T16:44");
        assert_eq!(clock(0, t, 0), "-");
        assert_eq!(zone_label(Some(-7 * 3600)), "times UTC-07:00");
        assert_eq!(zone_label(None), "times UTC");
        assert_eq!(age(42_000), "42s");
        assert_eq!(age(11 * 60_000), "11m");
        assert_eq!(age(3 * 3_600_000), "3h");
        assert_eq!(age(2 * 86_400_000), "2d");
    }

    const CMD: &str = "aterm link serve --fleet lab --broker /tmp/b.sock --state /s";

    /// A HEALTHY world: one instance, supervised and connected, one session with
    /// nothing wrong, and a broker that answers.
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
            command: CMD.to_string(),
            config_path: Some(PathBuf::from("/c/aterm.toml")),
            cfg: bridge_config(CMD).expect("a bridge command"),
            presence: crate::presence::Mode::Meta,
            receipts: false,
            node: Some("n-a".to_string()),
            caps: vec![("/c/node.cap".to_string(), Ok(8))],
            broker: BrokerView {
                endpoint: "/tmp/b.sock".to_string(),
                transport: "unix",
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
                command: Some(CMD.to_string()),
                bridge_pids: vec![101],
                node: Some("n-a".to_string()),
                state_dir: Some("/s".to_string()),
                sessions: vec![session],
                ..InstanceView::default()
            }],
            discovery_error: None,
            sessions: Vec::new(),
            nodes: Vec::new(),
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

    /// **ROUND 16: NODES LISTS EVERY NODE AND SAYS WHICH ONE THIS IS.** A
    /// second host joined over the sealed wire is a `remote` row with its own
    /// `host=`, `state=` and `fabric=` and its live-session count; this node
    /// is `this` and first; another instance of this machine is `local`; and
    /// this node with NO row on the bus is a row saying so. BRIDGES keeps to
    /// this machine's instances — the remote node is not a row there any more
    /// — and SESSIONS names every row's node.
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
        let text = render_text(&r);
        let nodes = text
            .split("\nNODES")
            .nth(1)
            .and_then(|t| t.split("\nBRIDGES").next())
            .expect("a NODES section between BROKER and BRIDGES");
        let rows: Vec<&str> = nodes.lines().skip(2).collect();
        assert!(
            rows[0].trim_start().starts_with("n-a")
                && rows[0].contains("  this  ")
                && rows[0].contains("host=m100 state=live fabric=connected"),
            "this node first:\n{nodes}"
        );
        assert!(
            rows[1].trim_start().starts_with("n-z")
                && rows[1].contains("  remote  ")
                && rows[1].contains("host=m7")
                && rows[1].trim_end().ends_with('1'),
            "the remote node, its host and its one live session:\n{nodes}"
        );
        let bridges = text
            .split("\nBRIDGES")
            .nth(1)
            .and_then(|t| t.split("\nSESSIONS").next())
            .expect("BRIDGES");
        assert!(
            !bridges.contains("n-z"),
            "BRIDGES is this machine's instances:\n{bridges}"
        );
        let far = text
            .lines()
            .find(|l| l.contains("s-far"))
            .expect("the remote session");
        assert!(far.contains("n-z") && far.contains("remote"), "{far}");
        // And the JSON says the same.
        let json = render_json(&r);
        assert!(
            json.contains("\"this\":true") && json.contains("\"where\":\"remote\""),
            "{json}"
        );
        // THIS NODE WITH NO PRESENCE ROW is a row, saying so.
        let mut lonely = healthy();
        lonely.nodes = vec![NodeRow {
            node: "n-z".to_string(),
            host: "m7".to_string(),
            state: "live".to_string(),
            fabric: "connected".to_string(),
        }];
        let text = render_text(&lonely);
        assert!(
            text.contains("no presence row on the bus"),
            "an absent presence row is the finding:\n{text}"
        );
        // Another instance of THIS machine answering as another node is `local`.
        assert_eq!(node_where(&lonely, "n-a"), "this");
        lonely.instances[0].node = Some("n-z".to_string());
        assert_eq!(node_where(&lonely, "n-z"), "local");
        assert_eq!(node_where(&lonely, "n-q"), "remote");
    }

    /// A GUARDED BROKER IS READ THROUGH THE CAP'S OWN FACES: every grant under
    /// the fleet, both modes, minus the consumer-group names — sorted, once.
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

    /// THE TCP BROKER'S PID is found by the port a client dials — its own
    /// bind, or the unspecified bind a loopback dial reaches.
    #[test]
    fn a_tcp_broker_is_found_by_the_address_its_clients_dial() {
        let procs = parse_ps(
            "  10 1 /x/aterm link broker --tcp 0.0.0.0:7000 --key-file /k --secret-file /s /l\n\
             \x20 11 1 /x/aterm-link broker --tcp 127.0.0.1:7001 --key-file /k --secret-file /s /l\n\
             \x20 12 1 /x/aterm link broker /r/bus.sock /r/bus.log\n",
        );
        assert_eq!(broker_pid_tcp(&procs, "127.0.0.1:7000"), Some(10));
        assert_eq!(broker_pid_tcp(&procs, "127.0.0.1:7001"), Some(11));
        assert_eq!(broker_pid_tcp(&procs, "127.0.0.1:7002"), None);
        assert_eq!(broker_pid(&procs, "/r/bus.sock"), Some(12));
    }

    /// **A BROKER THAT ANSWERS AND REFUSES THE READ IS NOT `reachable yes`.**
    ///
    /// MEASURED 2026-09-14 against a broker that answers `Hello` and `Attach`
    /// and errors the `Fetch` — what `Broker::open_guarded` does when the cap
    /// file's grants do not cover `/f/<fleet>/>` (a `--fleet` typo), and what a
    /// broker that dies between the attach and the head query does too. Before
    /// the fix the report said:
    ///
    /// ```text
    ///   reachable  yes — connect, hello, attach and head query answered in 0 ms
    ///   bus head   -
    /// TRAFFIC ... (the bus could not be read)
    /// WARNINGS  none — the broker answers, ...          exit=0
    /// ```
    ///
    /// `probe_broker` sets `attached = true` BEFORE the head query, so neither
    /// `!reachable` nor `!attached` fired; the roster, the traffic and the
    /// standing fleet halts were skipped in silence and `broker.error` — which
    /// `--json` carried all along — was printed NOWHERE in the text.
    #[test]
    fn a_head_query_that_failed_is_not_a_broker_that_answered() {
        let mut r = healthy();
        // Exactly what `probe_broker` leaves behind on a refused read.
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
        let text = render_text(&r);
        assert!(
            !text.contains("head query answered"),
            "the report claims the head query answered when it did not:\n{text}"
        );
        assert!(
            text.contains("yes, but the bus cannot be read: head query: unauthorized"),
            "the broker's refusal is printed nowhere:\n{text}"
        );
    }

    /// **A NODE THE BUS SAYS IS `gone` IS `connected` BEING A LIE.**
    ///
    /// MEASURED 2026-09-14 on this machine's live fleet: instance 66439 said
    /// `fabric=connected`, its bridge (pid 48224) had run since 09:39, the
    /// broker answered — and the roster `aterm link ls` prints said
    /// `n-1b631315bf5cae35 - node state=gone ... fabric=disconnected`. The
    /// report put both on one BRIDGES row (`ON THE BUS  state=gone
    /// fabric=disconnected`) and warned about neither: every peer reading the
    /// fleet's own directory saw this node as dead. (`warnings` never read
    /// `report.nodes` at all.)
    #[test]
    fn a_node_the_bus_calls_gone_is_a_warning_while_its_instance_says_connected() {
        let mut r = healthy();
        r.nodes = vec![NodeRow {
            node: "n-a".to_string(),
            host: "-".to_string(),
            state: "gone".to_string(),
            fabric: "disconnected".to_string(),
        }];
        one(&r, "state=gone");
        one(&r, "sees this node dead");

        // A REMOTE node that is gone is not this machine's problem, and a live
        // row for our own node is not a warning at all.
        let mut ok = healthy();
        ok.nodes = vec![
            NodeRow {
                node: "n-a".to_string(),
                host: "-".to_string(),
                state: "live".to_string(),
                fabric: "connected".to_string(),
            },
            NodeRow {
                node: "n-far".to_string(),
                host: "box".to_string(),
                state: "gone".to_string(),
                fabric: "disconnected".to_string(),
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

    /// EVERY CONDITION THE SPEC NAMES IS A WARNING, and a healthy world has none
    /// — so `exit 0` means something.
    #[test]
    fn every_condition_that_makes_connected_a_lie_or_loses_mail_is_a_warning() {
        let mut r = healthy();
        assert_eq!(warned(&r), Vec::<String>::new());
        r.warnings = warned(&r);
        assert_eq!(exit_of(&r), 0);

        // The broker does not answer while a bridge says `connected` — the
        // bridge's ack is history, this probe is now.
        let mut r = healthy();
        r.broker.reachable = false;
        r.broker.attached = false;
        r.broker.error = Some("connect: Connection refused".to_string());
        one(&r, "does not answer (connect: Connection refused)");
        one(
            &r,
            "instance 100 says fabric=connected (its bridge's last ack was 3s ago), but the \
             broker at /tmp/b.sock does not answer this report's own probe",
        );

        // A bridge whose broker link is DOWN (round 13): its reason and the
        // age of its last ack are in the warning, and `never` when there was
        // none.
        let mut r = healthy();
        r.instances[0].fabric = Some("stalled".to_string());
        r.instances[0].reason = Some("refused".to_string());
        one(
            &r,
            "instance 100's bridge is attached but its broker link is down (fabric=stalled \
             reason=refused, last ack 3s ago)",
        );
        r.instances[0].link_age_ms = None;
        r.instances[0].reason = Some("no-socket".to_string());
        one(&r, "(fabric=stalled reason=no-socket, last ack never)");
        assert_eq!(fabric_cell(&r.instances[0]), "stalled (no-socket)");
        assert_eq!(
            fabric_cell(&healthy().instances[0]),
            "connected (rtt 2 ms, acked 3s ago)"
        );

        // dropped > 0.
        let mut r = healthy();
        r.instances[0].sessions[0].dropped = Some(2);
        one(&r, "@s-one (instance 100) lost 2 message(s)");

        // hold=1, with its reason and origin — and without, when the timeline
        // no longer holds the transition.
        let mut r = healthy();
        r.instances[0].sessions[0].hold = Some(1);
        r.instances[0].sessions[0].hold_why =
            Some(("fabric-lost".to_string(), "fleet".to_string()));
        one(&r, "is HELD (reason=fabric-lost origin=fleet)");
        r.instances[0].sessions[0].hold_why = None;
        one(&r, "is HELD (reason and origin no longer in its timeline)");

        // An unhandled task older than ten minutes; a nine-minute one is not.
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
        assert!(
            w.iter().any(|l| l.contains(
                "has 1 unhandled task/ask older than 10 min — the oldest, task from h-andrew, is 11m old"
            )),
            "{w:#?}"
        );
        ages.insert(("s-one".to_string(), 40), r.now_ms - 9 * 60 * 1000);
        assert!(warnings(&r, &ages, &[]).is_empty(), "a note is not work");
    }

    /// **AN ASK PAST ITS DEADLINE WITH NO REPLY IS A WARNING (R8), AND THE RULE
    /// IS READ OFF THE BUS.** An `answer|report|ack` carrying `re=` settles it;
    /// an `expired` verdict does not settle it but is reported; a `note` with a
    /// `dl=` is not work; a deadline not yet passed is not overdue.
    #[test]
    fn an_ask_past_its_deadline_with_no_reply_is_a_warning() {
        let now = 100 * 60 * 1000u64;
        let lane = |src: &str, kind: &str| format!("/f/lab/in/n-a/s-one/{src}/{kind}");
        let rec = |off: u64, subject: String, body: &str| (off, subject, body.as_bytes().to_vec());
        let records = vec![
            // Twenty minutes old, a ten-minute deadline, nothing names it.
            rec(
                40,
                lane("h-andrew", "ask"),
                &format!("v=1 t={} dl=600000 text=which", now - 20 * 60 * 1000),
            ),
            // The same, answered.
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
            // The same, with the asker's bridge's verdict already on the lane.
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
            // Not yet due.
            rec(
                43,
                lane("h-andrew", "ask"),
                &format!("v=1 t={} dl=600000 text=soon", now - 5 * 60 * 1000),
            ),
            // A note is not work, and a record with no dl= has no deadline.
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
            // An ask settled by an ack, and one settled by a report.
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
        assert!(
            w[0].contains("ask off=40 from h-andrew to @s-one@n-a passed its deadline 10m ago (dl=600000 ms) with no answer, report or ack — not yet recorded expired"),
            "{}",
            w[0]
        );
        assert!(
            w[1].contains("ask off=42 from s-two@n-b to @s-one@n-a")
                && w[1].ends_with("the asker's bridge recorded it expired"),
            "{}",
            w[1]
        );
        r.warnings = w;
        assert_eq!(exit_of(&r), 1);
        let json = render_json(&r);
        assert!(json.contains("\"overdue\":[{\"off\":40,"), "{json}");
        assert!(json.contains("\"expired\":true"), "{json}");

        // A bridge that is not supervised; one that is down.
        let mut r = healthy();
        r.instances[0].supervised = Some(false);
        r.instances[0].fabric = Some("absent".to_string());
        one(
            &r,
            "instance 100 has no bridge (fabric=absent, not supervised)",
        );
        let mut r = healthy();
        r.instances[0].fabric = Some("disconnected".to_string());
        one(&r, "instance 100's bridge is down (fabric=disconnected)");

        // Queued posts with no working bridge.
        r.instances[0].sessions[0].posts = Some(3);
        one(&r, "has 3 queued post(s) and no working bridge");

        // An instance armed with a DIFFERENT command than the config names.
        let mut r = healthy();
        r.instances[0].command =
            Some("aterm link serve --fleet lab --broker /tmp/old.sock --state /s".to_string());
        one(
            &r,
            "instance 100's bridge was armed with a different command than [fabric] command \
             in /c/aterm.toml: it runs --broker /tmp/old.sock (not /tmp/b.sock)",
        );
        // ...and the broker the REPORT probed says nothing about that bridge's.
        r.broker.reachable = false;
        assert!(
            !warned(&r)
                .iter()
                .any(|l| l.contains("says fabric=connected")),
            "{:#?}",
            warned(&r)
        );

        // Two bridges on one state dir.
        let mut r = healthy();
        let mut twin = r.instances[0].clone();
        twin.pid = 200;
        twin.sessions.clear();
        r.instances.push(twin);
        one(&r, "instances 100, 200 run bridges on ONE state dir (/s)");

        // An instance that did not answer.
        let mut r = healthy();
        r.instances[0].error = Some("connect: timed out".to_string());
        one(&r, "instance 100 did not answer (connect: timed out)");

        // No config: the command came from a hand-armed instance.
        let mut r = healthy();
        r.source = Source::Instance(100);
        one(
            &r,
            "no [fabric] command in /c/aterm.toml: this report read instance 100's running bridge",
        );

        // No node id; an unreadable cap file.
        let mut r = healthy();
        r.node = None;
        r.caps = vec![(
            "/c/node.cap".to_string(),
            Err("the tag is not hex".to_string()),
        )];
        one(&r, "no node id in /s/node");
        one(&r, "cap file /c/node.cap: the tag is not hex");

        // A fleet halt standing on the bus.
        let r = healthy();
        let w = warnings(
            &r,
            &BTreeMap::new(),
            &[("h-andrew".to_string(), "stop".to_string())],
        );
        assert!(w
            .iter()
            .any(|l| l.contains("h-andrew has a fleet halt standing (reason=stop)")));

        // A LIVE presence row for this machine's node that no instance hosts.
        let mut r = healthy();
        // `s-gone` CARRIES A FRESH `t=` ON PURPOSE. The listing is bounded by
        // [`GONE_SHOW_MS`], so a row stamped `t=1` is an hour past it, is
        // counted into `exited` and never reaches the warning filter at all —
        // which would make the assertion below pass whether that filter checks
        // the bus state or not. A retired row is only interesting to this test
        // while it is still shown.
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
        assert!(
            w.iter()
                .any(|l| l.contains("the bus advertises @s-ghost live on n-a")),
            "{w:#?}"
        );
        // AND THE RETIRED ROW EARNS NO WARNING, which is the whole of the fix.
        // Listing a `gone` row gave it `pid: None`, and the warning selected on
        // that alone — so every session that ended normally in the last hour was
        // reported as "advertised live and undeliverable", which is false twice
        // over: nothing advertises it, and its `state=` says `exited`.
        assert!(
            !w.iter().any(|l| l.contains("@s-gone")),
            "a retired row is not advertised live and must not be warned about: {w:#?}"
        );
        assert!(
            !w.iter().any(|l| l.contains("@s-far")),
            "a REMOTE node's session is not this machine's to host: {w:#?}"
        );
    }

    /// THE TEXT REPORT: six sections in the spec's order, every number where the
    /// columns say, and no body anywhere.
    #[test]
    fn the_text_report_has_six_sections_in_order_and_prints_no_body() {
        let mut r = healthy();
        r.traffic = vec![traffic_of(
            "lab",
            7,
            "/f/lab/in/n-a/s-one/h-andrew/task",
            b"v=1 t=6000000 text=the%20secret%20plan",
        )];
        r.warnings = warned(&r);
        let text = render_text(&r);
        let at = |h: &str| {
            text.find(&format!("\n{h}"))
                .unwrap_or_else(|| panic!("no {h}: {text}"))
        };
        let order = [
            at("CONFIG"),
            at("BROKER"),
            at("BRIDGES"),
            at("SESSIONS"),
            at("TRAFFIC"),
            at("WARNINGS"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert!(
            !text.contains("secret"),
            "a body reached the report: {text}"
        );
        assert!(text.contains("h-andrew"), "{text}");
        assert!(text.contains("WARNINGS  none"), "{text}");
        let json = render_json(&r);
        assert!(!json.contains("secret"), "{json}");
        assert!(json.contains("\"exit\":0"), "{json}");
    }

    /// THE JSON writer closes every string whatever a value holds.
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
