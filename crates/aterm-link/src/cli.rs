// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link` — the fabric bridge's command line.
//!
//! The two bridge subcommands §11.2 lists (USAGE has the rest):
//!
//! ```text
//! aterm-link serve --fleet <F> --broker <ep> [--tcp] [--key-file PATH]
//!                  --cap-file <path>… [--state DIR] [--accept-from <p>,…]
//!                  [--sock <path> --token-file <path>]
//! aterm-link ls    --fleet <F> --broker <ep> [--tcp] [--key-file PATH]
//!                  --cap-file <path>… [--attention]
//! ```
//!
//! An unknown word is refused BY NAME (exit 2) rather than answered with the
//! usage text: a typo that prints help reads as a request for help.
//!
//! The one exception is the retired `hook`: a surviving hook entry still runs
//! it on every prompt, and exit 2 there is Claude Code's BLOCKING error, so it
//! answers exit 0, nothing on stdout, one line on stderr ([`retired_hook`]) —
//! permanently, since no cleanup can reach every project's settings file.
//!
//! ## `ls` PRINTS §7'S COLUMNS, AND SAYS WHICH OF THEM HAVE NO WRITER
//!
//! §7 pins the row as `<node> <host> <sid> state= inc= role= detail= driving=
//! holder= hold= fabric= attention=`, and §9.3 sells `ls --attention` as the
//! human's cross-host escalation view. Both are printed here, in that order, and
//! `--attention` keeps only the rows carrying one.
//!
//! THREE OF THOSE COLUMNS HAVE NO WRITER IN THIS FABRIC YET, and they print `-`
//! rather than being dropped, because `-` is the truth ("unknown") and a missing
//! column is a reader silently disagreeing with the design:
//!
//! * `driving=` is in §4.2's presence body but
//!   [`crate::bridge::Bridge::publish_session_presence`] does not publish it,
//!   so every row shows `-`. (`role=` and `detail=` are written by
//!   [`crate::presence`] on a `presence = "meta"` bridge, the default; a
//!   `minimal` bridge shows `-`.)
//! * `host=` and `fabric=` are published on the NODE's own presence row only
//!   (`bridge.rs`'s `bring_presence_up`): `fabric=` on a session row was a
//!   constant nothing could falsify. A session row shows `-` for both; the
//!   node row above it carries the answer.
//!
//! SIX COLUMNS ARE ADDED to §7's set, at the end, because each has a real
//! writer and a real reader: `gen=` (§6.6's `<content_seq>:<fp16>`, what a
//! `gen=`-bound approval is minted against), `observer=` (§11.2's watching,
//! non-hosting attachment — a row an operator must not read as a host),
//! `epoch=` (the launch nonce a drive record is fenced on), `phase=` (the
//! server's `status agent=` verdict, relayed), `context=` (no longer written;
//! printed for rows an older bridge wrote) and `title=` (the session's `meta
//! set title` user title, else `-`; NEVER the terminal's title, which the
//! program writes — [`crate::presence`]; 128 bytes).
//!
//! EVERY PRINTED FIELD IS UNTRUSTED. A presence body is written by whatever node
//! holds `rw,p=<n>:/f/<F>/pub/<n>/>`, and `attention=` is free text a session
//! chose. Each column is normalised through [`crate::pct`] before it is
//! printed, so a row is always one whitespace-delimited token per column of
//! printable ASCII and no body can move an operator's cursor.

use std::process::ExitCode;

use crate::bridge::{Bridge, Config};
use crate::transport::{self, Transport};

/// The build line [`USAGE`] ends with, per build. A macro rather than a
/// `const` because `concat!` takes literals only.
#[cfg(feature = "sealed")]
macro_rules! build_line {
    () => {
        "this build: sealed TCP transport compiled in (--tcp --key-file dials and serves)\n"
    };
}
#[cfg(not(feature = "sealed"))]
macro_rules! build_line {
    () => {
        "this build: no sealed TCP transport (a default build: --tcp --key-file is refused)\n"
    };
}

/// Usage, printed to stderr on a usage error (exit 2, aterm's convention).
///
/// Its LAST LINE says whether this build carries the sealed transport
/// (`build_line!`) — the one fact about a binary its flags cannot show, and
/// the one `aterm fabric join` and `on --tcp` read off the binary they will
/// write into a bridge command ([`crate::enable`]'s `binary_has_link`).
const USAGE: &str = concat!(
    "\
aterm-link — the aterm fabric bridge

  aterm-link serve --fleet <F> --broker <ep> --cap-file <path>... [options]
  aterm-link ls    --fleet <F> --broker <ep> --cap-file <path>... [--attention]
  aterm-link notify --on <attention|ask:<p>|halt>,... --exec <cmd>  (`notify` for its own usage)
  aterm-link broker <socket> [log] [--secret-file <path>]
                    the local bus itself (`broker` for its usage)
  aterm-link broker --tcp <host:port> --key-file <k> --secret-file <s> <log>
                    the bus on the SEALED TCP wire, always guarded (a `sealed` build)
  aterm-link mint   <grant> --secret-file <path>    one capability line for --cap-file
  aterm-link fabric ...  the same as `aterm fabric` (`fabric help` for its usage)

`ls` defaults its flags from the rendezvous file `aterm fabric on` (or `join`) writes
beside the instance control sockets (fabric.toml): --fleet and --broker, and WITH a
defaulted --broker only, the node's --cap-file and --state (a cap is minted for that one
broker, so it is never presented to a --broker you named) and, on a host that joined
over the sealed wire, --tcp --key-file. A flag given on the command line wins.

  --fleet <F>            the fleet name (the `/f/<F>/` subtree)
  --broker <ep>          the broker: a Unix socket path, or <host>:<port> with --tcp
  --tcp                  reach the broker over TCP instead of a Unix socket
  --key-file <path>      64 hex chars: the sealed transport's pre-shared key (needs --tcp)
  --cap-file <path>      a minted capability (`<grant> <tag-hex>` lines); repeatable
  --state <dir>          the state dir (node id, incarnation, sequence, watermarks)
  --accept-from <p>,...  principals whose task/control arrive undemoted
  --sock <path>          hand-started OBSERVER mode against a control socket
  --token-file <path>    the instance token, for --sock
  --presence <mode>      `serve` only: what a session's presence row carries.
                         meta (the default): role= detail= phase= title= beside
                         attention=; phase= is aterm's agent verdict, title= the
                         `meta set title` name only. minimal: attention= alone.
                         Without the flag, `[fabric] presence` in aterm.toml.
                         No screen text is published; gen= is a hash of it.
  --receipts             `serve` only, on by default: when one of this node's
                         sessions runs `inbox seen <id> handled|refused|deferred`
                         on an ask or task, tell the sender (`kind=ack`), which
                         releases their `post --wait-ack`. `--no-receipts` turns
                         it off for one run, `[fabric] receipts = false` for good;
                         off, a sender's `post --wait-ack` waits out its deadline.
  --attention            `ls` only: keep only rows carrying an `attention=`

`ls` prints one line per node and per session: <node> <host> <sid> state= inc=
role= detail= driving= holder= hold= fabric= attention= gen= observer= epoch=
phase= context= title=, each value pct-encoded (`12%` reads `12%25`). driving=
always reads `-`; holder= and context= do too unless an older node wrote them,
and role= detail= phase= title= do from a `minimal` bridge. <host> and fabric=
are on the node's own line; a session's line reads `-` for both.

`--tcp` alone is plaintext: trusted networks only. `--tcp --key-file` is the sealed
wire a cross-host fleet uses; the shipped `aterm` does not carry it, and a build
without it refuses the flags. The line below says which this build is.

",
    build_line!()
);

/// The phrase [`USAGE`] carries in a `sealed` build — what a caller checks a
/// binary's `link` output for before writing a sealed bridge command.
pub const SEALED_BUILD_MARK: &str = "sealed TCP transport compiled in";

/// The bridge's whole command line, as a LIBRARY entry.
///
/// A library entry so the ONE `aterm` binary carries it: `aterm link ...` and
/// the `aterm-link` symlink both land here; `src/main.rs` is a dev shim.
#[must_use]
pub fn dispatch(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("serve") => run(&args[1..], true),
        // THE READ-SIDE VERBS DEFAULT THEIR FLAGS from the rendezvous file
        // (`crate::enable`); `serve` does not, because aterm launches it with
        // every flag spelled out and a bridge that silently took a different
        // broker than its config names would be the report lying about itself.
        //
        // No rendezvous file and no flags: say what was checked, not "no
        // fabric" — a hand-written `[fabric] command` writes no rendezvous file.
        Some("ls") => {
            let given = |flag: &str| args[1..].iter().any(|a| a == flag);
            if !given("--fleet")
                && !given("--broker")
                && matches!(crate::enable::Rendezvous::read(), Ok(None))
            {
                eprintln!(
                    "aterm-link ls: no --fleet/--broker and no rendezvous file — \
                     `aterm fabric on` writes one"
                );
                return ExitCode::from(2);
            }
            run(&crate::enable::with_rendezvous_defaults(&args[1..]), false)
        }
        Some("notify") => crate::notify::main(&args[1..]),
        Some("broker") => broker(&args[1..]),
        Some("mint") => mint(&args[1..]),
        Some("fabric") => crate::fabric::main(&args[1..]),
        // THE RETIRED `hook` VERB: a silent no-op, never a refusal ([`retired_hook`]).
        Some("hook") => retired_hook(),
        // Asked for: stdout, exit 0, like `aterm --help` and `broker -h`.
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        // Refused BY NAME (module doc).
        Some(other) => {
            eprintln!("aterm-link: unknown subcommand `{other}`");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        None => {
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// The one line `aterm link hook …` prints, on stderr ([`retired_hook`]).
pub const RETIRED_HOOK_LINE: &str =
    "aterm-link: `hook` is retired and does nothing — delete this entry from the settings file \
     that runs it";

/// `aterm link hook …`, any arguments: EXIT 0, NOTHING ON STDOUT, one stderr line
/// ([`RETIRED_HOOK_LINE`]) — kept permanently.
///
/// A hook entry aterm once wrote can survive in a project's
/// `.claude/settings.local.json` or a running Claude Code, and exit 2 there is
/// Claude Code's BLOCKING error. Exit 0 is "go on"; stdout stays empty because a
/// `UserPromptSubmit` or `SessionStart` hook's stdout joins the model's context.
/// Stdin (the event's JSON, possibly a whole pasted prompt) is drained first, with
/// no byte cap, so the writer never meets a closed pipe. Never read from a
/// terminal: a person typing the verb gets the line at once.
fn retired_hook() -> ExitCode {
    use std::io::IsTerminal as _;
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let _ = std::io::copy(&mut stdin.lock(), &mut std::io::sink());
    }
    eprintln!("{RETIRED_HOOK_LINE}");
    ExitCode::SUCCESS
}

fn run(args: &[String], serve: bool) -> ExitCode {
    // THE SEALED WIRE IN A BUILD WITHOUT IT is refused here, by name, before
    // `parse` reads the key file: `serve` would otherwise redial for ever. Not
    // in `parse`, which `aterm fabric` also reads a sealed host's
    // `[fabric] command` with.
    if let Some(refusal) = transport::sealed_flags_refusal(args) {
        eprintln!("aterm-link: --key-file: {refusal}");
        return ExitCode::from(2);
    }
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("aterm-link: {e}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if serve {
        // `--attention` is an `ls` filter; `serve` refuses it by name rather
        // than parsing it and doing nothing.
        if parsed.attention {
            eprintln!("aterm-link: --attention is an `ls` filter; `serve` has no roster");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        let mut cfg = parsed.cfg;
        // `serve` defaults `[fabric] presence` and `[fabric] receipts` from
        // aterm.toml: they are knobs on what the bridge publishes, not on which
        // broker it reaches, so they cannot make the report lie about the bus.
        if !parsed.presence_given {
            cfg.presence = crate::fabric::presence_from_config();
        }
        if !parsed.receipts_given {
            cfg.receipts = crate::fabric::receipts_from_config();
        }
        match Bridge::new(cfg).and_then(Bridge::run) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("aterm-link serve: {e}");
                ExitCode::FAILURE
            }
        }
    } else {
        match roster(&parsed.cfg, parsed.attention) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("aterm-link ls: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

/// What `parse` answers: the bridge's own config, plus the flags that belong to
/// the command line rather than to a running bridge.
pub(crate) struct Parsed {
    pub(crate) cfg: Config,
    /// `ls --attention` (§9.3): keep only the rows carrying an escalation.
    pub(crate) attention: bool,
    /// Whether `--presence` was on the command line — else `serve` reads
    /// `[fabric] presence` from aterm.toml ([`crate::fabric::presence_from_config`]).
    pub(crate) presence_given: bool,
    /// Whether `--receipts`/`--no-receipts` was on the command line — else
    /// `serve` reads `[fabric] receipts` ([`crate::fabric::receipts_from_config`]).
    pub(crate) receipts_given: bool,
}

/// One printed column, made safe and made a single token.
///
/// A presence body is written by another host and `attention=` is free text a
/// session chose, so a value reaches here already pct-encoded by its writer —
/// or NOT, if its writer is hostile. Decoding and re-encoding is idempotent on
/// a well-formed token and total on a malformed one ([`crate::pct`]), and
/// it is the step that guarantees what the row's shape promises: printable
/// ASCII, no whitespace, no escape sequence, one token per column.
fn column(raw: &str) -> String {
    let mut safe = crate::pct::encode(&crate::pct::decode(raw));
    // BOUNDED by the same [`crate::glance::ATTENTION_CAP`] the bridge's writer
    // and `glance` use: a row from another host need not have gone through
    // the bridge's writer. The cut is on an escape boundary, never inside a
    // `%XX`, and the mark is the ASCII-encoded ellipsis `glance` appends.
    if safe.len() > crate::glance::ATTENTION_CAP {
        // The mark is inside the cap.
        safe = crate::presence::clamp_pct(&safe, crate::glance::ATTENTION_CAP - ELLIPSIS.len());
        safe.push_str(ELLIPSIS);
    }
    if safe.is_empty() {
        "-".to_string()
    } else {
        safe
    }
}

/// `pct::encode("…")` — the mark a truncated column ends with, spelled in the
/// escape so a row stays printable ASCII.
const ELLIPSIS: &str = "%E2%80%A6";

/// `aterm-link ls` — the cross-host `ls`: `Last{/f/<F>/pub/*/*/presence}`, which
/// is one bounded last-value walk and no filesystem scan, no `dial`, and no
/// saved peer names.
///
/// It pages on the resume cursor through [`transport::walk_last`]; an empty
/// page is not the end of the answer. A short roster is indistinguishable from
/// a small fleet, so a walk that could not FINISH is an error, not an absence.
///
/// The row is §7's (module header). `attention_only` is §9.3's `--attention`:
/// the same `Last` filtered on the field the notifier fires on (§4.2, A10).
fn roster(cfg: &Config, attention_only: bool) -> std::io::Result<()> {
    let (mut client, _closer) = transport::connect(&cfg.transport, &cfg.broker).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("cannot connect to the broker at {}: {e}", cfg.broker),
        )
    })?;
    crate::fabric::attach_caps(&mut client, cfg)?;
    let filter = format!("/f/{}/pub/*/*/presence", cfg.fleet);
    transport::walk_last(&mut client, &filter, |(_, subject, raw)| {
        let (body, _) = crate::body::Body::decode(raw);
        let (line, escalating) = row(subject, &body);
        if !attention_only || escalating {
            println!("{line}");
        }
        Ok(())
    })
}

/// One roster line, and whether it is an ESCALATION. Split out of [`roster`]
/// so §7's column set is testable without a broker.
fn row(subject: &str, body: &crate::body::Body) -> (String, bool) {
    let segs: Vec<&str> = subject.split('/').collect();
    let node = column(segs.get(4).unwrap_or(&"-"));
    let owner = column(segs.get(5).unwrap_or(&"-"));
    let field = |k: &str| {
        body.unknown
            .get(k)
            .map_or_else(|| "-".into(), |v| column(v))
    };
    let attention = field("attention");
    // The same three-way test `notify::Sel::Attention` makes: absent, empty and
    // the `-` an absent field renders as are all "no escalation", so the two
    // faces of §9.3's question cannot disagree.
    let escalating = attention != "-";
    let line = format!(
        "{node} {} {owner} state={} inc={} role={} detail={} driving={} \
         holder={} hold={} fabric={} attention={attention} gen={} observer={} epoch={} \
         phase={} context={} title={}",
        field("host"),
        field("state"),
        field("inc"),
        field("role"),
        field("detail"),
        field("driving"),
        field("holder"),
        field("hold"),
        field("fabric"),
        // `gen=` and `epoch=` are KNOWN body fields (§4.1), so they are parsed
        // into `Body` and never reach `unknown` — reading them through the
        // unknown map would print `-` for every row.
        body.gen.as_deref().map_or_else(|| "-".into(), column),
        // ABSENT MEANS `0`, not unknown: only an observer attachment writes the
        // token (§11.2), so a row without one is a host.
        body.unknown
            .get("observer")
            .map_or_else(|| "0".to_string(), |v| column(v)),
        body.epoch.as_deref().map_or_else(|| "-".into(), column),
        // Written by a `meta`-mode bridge; `-` from any other.
        field("phase"),
        field("context"),
        field("title"),
    );
    (line, escalating)
}

/// Parse argv. Every flag takes a value; a flag without one is a usage error
/// rather than a silently defaulted setting.
///
/// `pub(crate)` because `aterm fabric` reads the fleet, broker, cap files and
/// state dir out of `[fabric] command` with THIS parser — the one the bridge it
/// describes was started with — rather than a second one that could disagree.
pub(crate) fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut attention = false;
    let mut presence_given = false;
    let mut receipts_given = false;
    let mut cfg = Config {
        fleet: String::new(),
        broker: String::new(),
        transport: Transport::Unix,
        cap_files: Vec::new(),
        state_dir: String::new(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::Meta,
        // ON, matching `receipts_from_config`'s absent-key answer and what
        // `aterm fabric on` writes.
        receipts: true,
    };
    let mut token_file: Option<String> = None;
    let mut tcp = false;
    let mut key_file: Option<String> = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--fleet" => cfg.fleet = value()?,
            "--broker" => cfg.broker = value()?,
            "--tcp" => tcp = true,
            "--key-file" => key_file = Some(value()?),
            // REFUSED BY NAME: astream's forward-secret and identity transports
            // would widen this crate's pinned dependency set (DESIGN-aterm-fabric.md
            // §11.2), and silently falling back to the PSK would be worse.
            other @ ("--handshake" | "--identity" | "--identity-file" | "--host-key-file") => {
                return Err(format!(
                    "{other}: unsupported; the only TCP wire is the sealed PSK one (--tcp --key-file)"
                ));
            }
            "--cap-file" => cfg.cap_files.push(value()?),
            "--state" => cfg.state_dir = value()?,
            "--accept-from" => {
                cfg.accept_from
                    .extend(value()?.split(',').map(str::to_string));
            }
            "--attention" => attention = true,
            "--presence" => {
                let v = value()?;
                cfg.presence = crate::presence::Mode::parse(&v)
                    .ok_or_else(|| format!("--presence {v}: `meta` or `minimal`"))?;
                presence_given = true;
            }
            "--receipts" => {
                cfg.receipts = true;
                receipts_given = true;
            }
            "--no-receipts" => {
                cfg.receipts = false;
                receipts_given = true;
            }
            "--sock" => cfg.sock = Some(value()?),
            "--token-file" => token_file = Some(value()?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if cfg.fleet.is_empty() || cfg.broker.is_empty() {
        return Err("--fleet and --broker are required".to_string());
    }
    cfg.transport = transport_from_flags(
        tcp,
        key_file,
        "aterm-link: --tcp without --key-file is plaintext; trusted networks only",
    )?;
    if !crate::subject::is_fleet(&cfg.fleet) {
        return Err(format!(
            "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            cfg.fleet
        ));
    }
    if cfg.state_dir.is_empty() {
        cfg.state_dir = default_state_dir();
    }
    // Read the way every control client reads an instance token
    // (`aterm_ctl::read_control_token_file`): a symlink planted at the name is
    // refused rather than followed, only a regular file is read, and no more
    // than a token's bound of it.
    if let Some(path) = token_file {
        cfg.token = Some(
            aterm_ctl::read_control_token_file(std::path::Path::new(&path))
                .map_err(|e| format!("--token-file {path}: {e}"))?,
        );
    }
    if cfg.sock.is_some() && cfg.token.is_none() {
        return Err("--sock needs --token-file".to_string());
    }
    for p in &cfg.accept_from {
        if !crate::subject::is_principal(p) {
            return Err(format!("--accept-from {p} is not a principal"));
        }
    }
    Ok(Parsed {
        cfg,
        attention,
        presence_given,
        receipts_given,
    })
}

/// The transport `--tcp` and `--key-file` name — shared by `serve` and `notify`. A
/// KEY WITHOUT `--tcp` IS REFUSED: ignoring it would run unsealed while the operator
/// believed otherwise, and implying `--tcp` would dial a socket path as a host:port.
/// Plain `--tcp` prints `plaintext_warning` to stderr.
pub(crate) fn transport_from_flags(
    tcp: bool,
    key_file: Option<String>,
    plaintext_warning: &str,
) -> Result<Transport, String> {
    Ok(match (tcp, key_file) {
        (false, None) => Transport::Unix,
        (true, None) => {
            eprintln!("{plaintext_warning}");
            Transport::Tcp
        }
        (true, Some(path)) => Transport::Sealed(Box::new(
            transport::read_key_file(&path)
                .map_err(|e| format!("--key-file {}", transport::describe(&path, &e)))?,
        )),
        (false, Some(_)) => {
            return Err("--key-file needs --tcp".to_string());
        }
    })
}

/// `$XDG_STATE_HOME/aterm-link`, else `$HOME/.local/state/aterm-link`, else the
/// working directory — never a temp dir, because losing the node id loses the
/// node's mail lane. `aterm-link notify` reads its journal under the same
/// directory (§9.3: "beside the bridge's other durable state").
pub(crate) fn default_state_dir() -> String {
    if let Ok(x) = std::env::var("XDG_STATE_HOME") {
        if !x.is_empty() {
            return format!("{x}/aterm-link");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return format!("{home}/.local/state/aterm-link");
        }
    }
    "./aterm-link-state".to_string()
}

/// `aterm link broker <socket> [log]` — run the bus this instance's bridge talks to.
///
/// TWO LISTENERS — and the second host's broker serves both:
///
/// * `<socket> [log]` — the LOCAL broker: a Unix socket created
///   world-connectable — nothing chmods it and no peer uid is checked — so the
///   boundary is the DIRECTORY you put it in, 0700. With no `--secret-file`
///   it checks no capability on attach either; with one it is
///   `Broker::open_guarded`: every attach must carry a capability minted under
///   that secret, and every request is authorized against it.
/// * `--tcp <host:port> --key-file <k> --secret-file <s> <log>` — the SECOND
///   HOST's broker: astream's sealed record layer
///   (`serve_tcp_sealed`, XChaCha20-Poly1305 under the 64-hex pre-shared key)
///   and ALWAYS guarded. The guard is not optional on TCP, and that is the
///   point of it: the pre-shared key is ONE secret every host holds, so it
///   keeps a stranger off the wire and says nothing about which host is
///   speaking. What stops the joined host publishing as the first host's node,
///   or as a human's drive lane, is the capability it attaches — minted for
///   its own node id by `aterm fabric mint-for` — and only a guarded broker
///   checks one. A non-loopback bind is refused without `--allow-remote`. Only
///   in a `sealed` build ([`transport::SEALED`]); a default build answers
///   `--tcp` by naming the feature. `--unix <socket>` serves that Unix socket
///   AS WELL, from the same guarded broker and log: the host that serves the
///   sealed wire points its OWN bridges there (`aterm fabric on --tcp`),
///   because astream admits only 64 connections into the sealed handshake at
///   once and a peer that can reach the port can hold them without the key.
///
/// The key and the secret are files, never arguments (an argv is world-readable
/// in `ps`), and both must be mode 0600 ([`transport::check_private`]).
fn broker(args: &[String]) -> ExitCode {
    const USAGE: &str = "\
usage: aterm link broker <socket> [log] [--secret-file <path>]
       aterm link broker --tcp <host:port> --key-file <path> --secret-file <path>
                         [--allow-remote] [--unix <socket>] <log>

  <socket>         the Unix socket path the bridge's `--broker` names
  [log]            the durable record log (default: <socket>.log; required with --tcp)
  --secret-file    32+ raw bytes, 0600: the MINT secret. The broker becomes
                   GUARDED — every attach must carry a capability minted under
                   it (`aterm link mint`, `aterm fabric mint-for`), and each
                   request is checked against the grants attached
  --tcp <h:p>      serve the SEALED TCP wire instead of a socket: astream's
                   XChaCha20-Poly1305 record layer under the pre-shared key.
                   Only in a `sealed` build. Always guarded (--secret-file is
                   required): the key is one secret every host holds, so the
                   capability is what says which node may publish what
  --key-file       64 hex characters, 0600: the pre-shared key (--tcp only)
  --allow-remote   bind a non-loopback address (--tcp only). Without it only
                   127.0.0.1 / ::1 is bound, because a broker on the network is
                   reachable by anyone who holds the key file
  --unix <socket>  --tcp only: ALSO serve this Unix socket, from the same broker
                   and log, for the host's own bridges (`aterm fabric on --tcp`
                   passes the root's bus.sock) — a peer that can reach the TCP
                   port can hold all 64 of its pre-authentication slots without
                   the key, and nothing on the socket waits on them

The Unix socket is created world-connectable, so its boundary is the DIRECTORY
you put it in: use a 0700 one (`aterm fabric on` does). Without --secret-file the
socket broker checks no capability on attach. Prints `listening <endpoint>` once
it is bound — the bound TCP address when the port was 0 — and, with --unix, a
second `listening <socket>` line once that is bound too.
";
    let mut tcp: Option<String> = None;
    let mut key_file: Option<String> = None;
    let mut secret_file: Option<String> = None;
    let mut unix: Option<String> = None;
    let mut allow_remote = false;
    let mut words: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let mut value = |name: &str| -> Result<String, ExitCode> {
            i += 1;
            args.get(i).cloned().ok_or_else(|| {
                eprintln!("aterm link broker: {name} needs a value");
                eprint!("{USAGE}");
                ExitCode::from(2)
            })
        };
        // A REPEATED flag is refused, never won last: `--secret-file good
        // --secret-file empty` quietly guarding with the wrong secret is the
        // shape `mint` already refuses.
        let once = |slot: &Option<String>, name: &str| -> Result<(), ExitCode> {
            if slot.is_some() {
                eprintln!("aterm link broker: {name} given twice");
                return Err(ExitCode::from(2));
            }
            Ok(())
        };
        match flag {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--tcp" => {
                if let Err(c) = once(&tcp, "--tcp") {
                    return c;
                }
                match value("--tcp") {
                    Ok(v) => tcp = Some(v),
                    Err(c) => return c,
                }
            }
            "--key-file" => {
                if let Err(c) = once(&key_file, "--key-file") {
                    return c;
                }
                match value("--key-file") {
                    Ok(v) => key_file = Some(v),
                    Err(c) => return c,
                }
            }
            "--secret-file" => {
                if let Err(c) = once(&secret_file, "--secret-file") {
                    return c;
                }
                match value("--secret-file") {
                    Ok(v) => secret_file = Some(v),
                    Err(c) => return c,
                }
            }
            "--unix" => {
                if let Err(c) = once(&unix, "--unix") {
                    return c;
                }
                match value("--unix") {
                    Ok(v) => unix = Some(v),
                    Err(c) => return c,
                }
            }
            "--allow-remote" => allow_remote = true,
            other if other.starts_with('-') => {
                eprintln!("aterm link broker: unknown flag `{other}`");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
            word => words.push(word),
        }
        i += 1;
    }
    // THE BUILD FIRST: a default build answers `--tcp` by naming the feature,
    // before any other word of the line is judged — what is missing is the
    // transport, not a flag.
    if tcp.is_some() && !transport::SEALED {
        eprintln!(
            "aterm link broker: --tcp: {}",
            transport::SEALED_UNAVAILABLE
        );
        return ExitCode::from(2);
    }
    let usage_error = |why: &str| {
        eprintln!("aterm link broker: {why}");
        eprint!("{USAGE}");
        ExitCode::from(2)
    };
    let (endpoint, log) = match &tcp {
        None => {
            if key_file.is_some() {
                return usage_error("--key-file needs --tcp");
            }
            if allow_remote {
                return usage_error("--allow-remote needs --tcp");
            }
            if unix.is_some() {
                return usage_error("--unix needs --tcp");
            }
            // A surplus word is refused, never dropped.
            match words.as_slice() {
                [sock] => ((*sock).to_string(), format!("{sock}.log")),
                [sock, log] => ((*sock).to_string(), (*log).to_string()),
                [] => {
                    eprint!("{USAGE}");
                    return ExitCode::from(2);
                }
                [_, _, extra, ..] => {
                    return usage_error(&format!("unexpected argument `{extra}`"));
                }
            }
        }
        Some(bind) => {
            let Some(key_path) = key_file.as_deref() else {
                return usage_error("--tcp needs --key-file: the broker serves TCP only sealed");
            };
            if secret_file.is_none() {
                return usage_error("--tcp needs --secret-file: a TCP broker is always guarded");
            }
            let log = match words.as_slice() {
                [log] => (*log).to_string(),
                [] => return usage_error("--tcp needs <log>"),
                [_, extra, ..] => {
                    return usage_error(&format!("unexpected argument `{extra}`"));
                }
            };
            match transport::is_loopback_endpoint(bind) {
                Ok(true) => {}
                Ok(false) if allow_remote => {}
                Ok(false) => {
                    eprintln!(
                        "aterm link broker: --tcp {bind}: not a loopback address, and every host \
                         of the fleet holds the one key; add --allow-remote"
                    );
                    return ExitCode::from(2);
                }
                Err(e) => {
                    eprintln!("aterm link broker: --tcp {bind}: {e} (a <host>:<port> to bind)");
                    return ExitCode::from(2);
                }
            }
            // Checked here, before the log is opened, so a bad key never leaves
            // an empty log behind.
            if let Err(e) = transport::read_private_key_file(key_path) {
                eprintln!(
                    "aterm link broker: --key-file {}",
                    transport::describe(key_path, &e)
                );
                return ExitCode::FAILURE;
            }
            (bind.clone(), log)
        }
    };
    let secret = match secret_file.as_deref().map(read_secret) {
        None => None,
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => {
            eprintln!("aterm link broker: {e}");
            return ExitCode::FAILURE;
        }
    };
    let guarded = secret.is_some();
    // Whether THIS call creates the log, so a refused bind can remove the
    // empty file it would otherwise leave behind.
    let log_created_here = !std::path::Path::new(&log).exists();
    let opened = match secret {
        Some(secret) => astream_broker::Broker::open_guarded(&log, secret),
        None => astream_broker::Broker::open(&log),
    };
    let broker = match opened {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aterm link broker: open {log}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let served = match &tcp {
        None => broker.serve(&endpoint),
        Some(bind) => serve_sealed(&broker, bind, key_file.as_deref().unwrap_or_default()),
    };
    // A refused bind leaves no empty log behind (see `log_created_here`).
    let refused = |what: &str, e: std::io::Error, broker: astream_broker::Broker| {
        eprintln!("aterm link broker: serve {what}: {e}");
        if log_created_here
            && std::fs::metadata(&log)
                .map(|m| m.len() == 0)
                .unwrap_or(false)
        {
            drop(broker);
            let _ = std::fs::remove_file(&log);
        }
        ExitCode::FAILURE
    };
    let handle = match served {
        Ok(h) => h,
        Err(e) => return refused(&endpoint, e, broker),
    };
    // THE SOCKET BESIDE THE PORT, from the same broker: one log, one guard,
    // one store — two acceptors.
    let beside = match unix.as_deref().map(|sock| (sock, broker.serve(sock))) {
        None => None,
        Some((_, Ok(h))) => Some(h),
        Some((sock, Err(e))) => {
            drop(handle);
            return refused(sock, e, broker);
        }
    };
    // The readiness line a supervisor waits for (`asb serve`'s word). On TCP
    // it is the BOUND address, so a port-0 bind names the port it got.
    //
    // WRITTEN, NEVER PRINTED: a supervisor may close the pipe after the first
    // line, and `println!` on a closed stdout panics (Rust ignores SIGPIPE).
    let shown = handle.tcp_addr().map_or(endpoint, str::to_string);
    let transport = if tcp.is_some() {
        "sealed TCP"
    } else {
        "unix socket"
    };
    let beside_note = unix
        .as_deref()
        .map_or_else(String::new, |sock| format!(" and the unix socket {sock}"));
    let guard_note = if guarded {
        " (guarded)"
    } else {
        " (unguarded)"
    };
    {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "listening {shown}");
        if let Some(sock) = unix.as_deref() {
            let _ = writeln!(out, "listening {sock}");
        }
        let _ = out.flush();
        let _ = writeln!(
            std::io::stderr(),
            "aterm link broker: {transport} on {shown}{beside_note}{guard_note}"
        );
    }
    // The broker serves on its own threads; this one parks. `BrokerHandle` has
    // no `join` — `asb serve` parks exactly this way — and dropping the handle
    // would shut the broker down and unlink the socket.
    let _keep = (handle, beside);
    loop {
        std::thread::park();
    }
}

/// A mint secret FILE: 0600 and at least 32 bytes — the length `mint` refuses
/// below, for its reason (HMAC takes any key, and a short one guards nothing).
fn read_secret(path: &str) -> Result<Vec<u8>, String> {
    transport::check_private(path)
        .map_err(|e| format!("--secret-file {}", transport::describe(path, &e)))?;
    let secret = std::fs::read(path).map_err(|e| format!("--secret-file {path}: {e}"))?;
    if secret.len() < crate::enable::SECRET_LEN {
        return Err(format!(
            "--secret-file {path} holds {} bytes; a mint secret is at least {}",
            secret.len(),
            crate::enable::SECRET_LEN
        ));
    }
    Ok(secret)
}

/// `serve_tcp_sealed` — in a `sealed` build. The default build never reaches
/// here ([`broker`] refuses `--tcp` by name first); this arm exists so the
/// call site has one shape in both builds.
#[cfg(feature = "sealed")]
fn serve_sealed(
    broker: &astream_broker::Broker,
    bind: &str,
    key_file: &str,
) -> std::io::Result<astream_broker::BrokerHandle> {
    let key = transport::read_private_key_file(key_file)?;
    broker.serve_tcp_sealed(bind, key)
}

#[cfg(not(feature = "sealed"))]
fn serve_sealed(
    _broker: &astream_broker::Broker,
    _bind: &str,
    _key_file: &str,
) -> std::io::Result<astream_broker::BrokerHandle> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        transport::SEALED_UNAVAILABLE,
    ))
}

/// `aterm link mint <grant> --secret-file <path>` — print one capability line.
///
/// A cap file is `<grant> <tag-hex>` lines, and `serve --cap-file` reads them.
///
/// The secret is read from a FILE and never an argument: an argv is world-
/// readable in `ps` for the life of the call, and this one is the key that mints
/// every capability on the fleet.
fn mint(args: &[String]) -> ExitCode {
    const USAGE: &str = "\
usage: aterm link mint <grant> --secret-file <path>

  <grant>          e.g. `rw,p=<node>:/f/<fleet>/pub/<node>/>` or `ro:/f/<fleet>/pub/>`
  --secret-file    32 raw bytes; the mint key. Keep it 0600 and off argv.

Prints one `<grant> <tag-hex>` line — append it to the file `serve --cap-file` reads.
";
    let mut grant: Option<&str> = None;
    let mut secret: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--secret-file" => {
                // Once: a repeated flag never wins last.
                if secret.is_some() {
                    eprintln!("aterm link mint: --secret-file given twice");
                    return ExitCode::from(2);
                }
                i += 1;
                match args.get(i) {
                    Some(v) => secret = Some(v),
                    None => {
                        eprintln!("aterm link mint: --secret-file needs a path");
                        return ExitCode::from(2);
                    }
                }
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if grant.is_none() && !other.starts_with('-') => grant = Some(other),
            other => {
                eprintln!("aterm link mint: unknown argument `{other}`");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let (Some(grant), Some(secret_path)) = (grant, secret) else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    let key = match std::fs::read(secret_path) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("aterm link mint: {secret_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // A SHORT SECRET IS REFUSED: HMAC accepts any key length, so an empty
    // or truncated file would mint a well-formed capability that seals nothing.
    if key.len() < crate::enable::SECRET_LEN {
        eprintln!(
            "aterm link mint: {secret_path} holds {} bytes; a mint secret is at least {}",
            key.len(),
            crate::enable::SECRET_LEN
        );
        return ExitCode::FAILURE;
    }
    match astream_cap::mint(&key, grant) {
        Ok(cap) => {
            // `<grant> <tag-hex>` in astream's one cap-file format, which every
            // reader splits at the LAST whitespace, so a grant whose filter holds
            // a space still reads back.
            println!(
                "{}",
                astream_cap::capfile::format_line(&cap.filter, &cap.tag)
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("aterm link mint: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    /// **`ls` PRINTS §7's ROW, AND `--attention` IS §9.3's VIEW.** The columns
    /// with no writer are asserted PRESENT and `-` ("unknown").
    #[test]
    fn ls_prints_section_7s_columns_and_attention_is_a_flag_not_an_error() {
        // §9.3's flag parses (`run` refuses it on `serve`).
        let base = "--fleet f1 --broker /tmp/b.sock --state /tmp/s";
        assert!(!parse(&argv(base)).expect("ok").attention);
        assert!(
            parse(&argv(&format!("{base} --attention")))
                .expect("--attention is a flag, not an unknown one")
                .attention
        );

        // §7'S ROW, IN §7'S ORDER, then the six documented additions.
        let (body, _) = crate::body::Body::decode(
            b"v=1 t=1 inc=4 epoch=ab12 gen=9:beef state=live hold=0 holder=h-andrew \
              attention=needs%20a%20key",
        );
        let (line, escalating) = row("/f/f1/pub/n-a/s-w/presence", &body);
        assert!(escalating, "a row carrying an attention IS an escalation");
        let cols: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(cols[0], "n-a", "{line}");
        assert_eq!(
            cols[1], "-",
            "§7 puts <host> positionally, and no session row has one: {line}"
        );
        assert_eq!(cols[2], "s-w", "{line}");
        let names: Vec<&str> = cols[3..]
            .iter()
            .map(|c| c.split('=').next().unwrap_or(c))
            .collect();
        assert_eq!(
            names,
            [
                "state",
                "inc",
                "role",
                "detail",
                "driving",
                "holder",
                "hold",
                "fabric",
                "attention",
                "gen",
                "observer",
                "epoch",
                "phase",
                "context",
                "title"
            ],
            "the row must be §7's columns in §7's order, then the additions: {line}"
        );
        assert!(
            line.contains(" attention=needs%20a%20key ") && line.contains(" gen=9:beef "),
            "{line}"
        );
        // The columns this body (a `minimal` bridge's) did not fill read `-`.
        for absent in [
            "role=-",
            "detail=-",
            "driving=-",
            "fabric=-",
            "phase=-",
            "context=-",
            "title=-",
        ] {
            assert!(line.contains(absent), "{absent} missing from: {line}");
        }
        // A `meta` row: the five are printed as written, in their columns.
        let (body, _) = crate::body::Body::decode(
            b"v=1 t=1 inc=4 state=live hold=0 holder=- attention=- role=worker \
              detail=claude phase=busy context=12%25 title=satcomp%20run",
        );
        let (line, _) = row("/f/f1/pub/n-a/s-w/presence", &body);
        for present in [
            " role=worker ",
            " detail=claude ",
            " phase=busy ",
            " context=12%25 ",
            " title=satcomp%20run",
        ] {
            assert!(line.contains(present), "{present} missing from: {line}");
        }

        // A ROW WITH NO ESCALATION IS NOT ONE, in all three spellings — absent,
        // empty, and the dash an absent field renders as.
        for quiet in [
            &b"v=1 t=1 state=live"[..],
            b"v=1 t=1 state=live attention=",
            b"v=1 t=1 state=live attention=-",
        ] {
            let (_, esc) = row(
                "/f/f1/pub/n-a/s-w/presence",
                &crate::body::Body::decode(quiet).0,
            );
            assert!(!esc, "{}", String::from_utf8_lossy(quiet));
        }

        // NO BODY MOVES AN OPERATOR'S CURSOR. Every column is one token of
        // printable ASCII, whatever bytes the writing node chose.
        let (hostile, _) = crate::body::Body::decode(
            "v=1 t=1 state=\u{1b}[2J\u{1b}[H attention=%1B%5B31mred".as_bytes(),
        );
        let (line, _) = row("/f/f1/pub/n-a/s-w/presence", &hostile);
        assert!(
            !line.contains('\u{1b}') && line.is_ascii() && !line.contains('\n'),
            "an untrusted presence body reached the terminal raw: {line:?}"
        );
        assert_eq!(
            line.split_whitespace().count(),
            18,
            "one token per column, always: {line:?}"
        );

        // Every column is bounded by the cap the bridge and glance use.
        let long = format!("v=1 t=1 attention={}", "é".repeat(400));
        let (line, _) = row(
            "/f/f1/pub/n-a/s-w/presence",
            &crate::body::Body::decode(long.as_bytes()).0,
        );
        let printed = line
            .split_whitespace()
            .find_map(|t| t.strip_prefix("attention="))
            .expect("the column");
        assert!(
            printed.len() <= crate::glance::ATTENTION_CAP,
            "an unbounded token reached the roster: {} bytes",
            printed.len()
        );
        assert!(printed.ends_with(ELLIPSIS), "{printed}");
        // NEVER INSIDE AN ESCAPE: what is printed still decodes to the prefix of
        // what was published, with no `%C3` stump on the end.
        assert!(
            crate::pct::decode(printed).ends_with('…'),
            "the cut landed inside a %XX escape: {printed}"
        );
    }

    /// `serve --token-file` reads the instance token the way every control
    /// client reads one: a symlink planted at the named path is refused, never
    /// followed to whatever file it names.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_token_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("atlk-tokfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let elsewhere = dir.join("elsewhere");
        std::fs::write(&elsewhere, "feed1234\n").expect("a token-shaped file");
        let planted = dir.join("a.sock.token");
        std::os::unix::fs::symlink(&elsewhere, &planted).expect("plant");
        let args = argv(&format!(
            "--fleet f1 --broker /tmp/b.sock --state /tmp/s --sock /tmp/a.sock --token-file {}",
            planted.display()
        ));
        let refused = parse(&args).map(|p| p.cfg.token);
        assert!(
            refused.as_ref().is_err_and(|e| e.contains("--token-file")),
            "the symlink was followed: {refused:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
