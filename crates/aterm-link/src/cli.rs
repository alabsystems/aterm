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
  aterm-link notify --on <attention|ask:<person>|halt>,... --exec <cmd>
                    a command run when the fabric needs a human (`notify` for its usage)
  aterm-link broker <socket> [log] [--secret-file <path>]
                    the local bus itself (`broker` for its usage)
  aterm-link broker --tcp <host:port> --key-file <k> --secret-file <s> <log>
                    the bus on the SEALED TCP wire, always guarded (a `sealed` build)
  aterm-link broker --repair <log>
                    truncate a log the broker refuses as corrupt at that record
  aterm-link mint   <grant> --secret-file <path>    one capability line for --cap-file
                    (a whole ring: `aterm fabric mint-for <node>|h-<person>`)
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
phase= context= title= proto= aterm=, each value pct-encoded (`12%` reads `12%25`).
driving= always reads `-`; holder= and context= do too unless an older node wrote
them, and role= detail= phase= title= do from a `minimal` bridge. <host>, fabric=,
proto= (the astream protocol the node speaks) and aterm= (the release it runs) are
on the node's own line; a session's line reads `-` for them.

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
/// the `aterm-link` symlink both land here.
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
    // One-shot, so bounded like every operator command: a wedged broker costs
    // a few seconds and an error, never a hung terminal.
    let (mut client, _closer) =
        transport::connect(&cfg.transport, &cfg.broker, crate::fabric::IO_TIMEOUT).map_err(
            |e| {
                std::io::Error::new(
                    e.kind(),
                    format!("cannot connect to the broker at {}: {e}", cfg.broker),
                )
            },
        )?;
    transport::attach_cap_files(&mut client, &cfg.cap_files)?;
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
         phase={} context={} title={} proto={} aterm={}",
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
        // A NODE's row says what it runs (`proto=` the astream protocol,
        // `aterm=` the release); `-` on a session row and from a node whose
        // bridge predates the tokens. Last, so every column before keeps its
        // place.
        field("proto"),
        field("aterm"),
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
///   world-connectable — nothing chmods it and no peer uid is checked — so
///   WHO can connect is bounded by the DIRECTORY you put it in, 0700. What a
///   connection may SAY is the guard's: with `--secret-file` it is
///   `Broker::open_guarded`, every attach must carry a capability minted under
///   that secret, and every request is authorized against the grants attached.
///   THE ROOT'S SECRET GUARDS THE ROOT'S SOCKET: with no `--secret-file`, a
///   `mint.secret` beside the socket (`aterm fabric on`'s layout) is the
///   secret, so a supervisor job written before `on` passed the flag comes back
///   guarded the next time it starts the broker (an update, a login) — one
///   that cannot be read is a refusal, never an unguarded broker. Only a socket
///   with no secret beside it is served unguarded, and the banner says so.
/// * `--tcp <host:port> --key-file <k> --secret-file <s> <log>` — the SECOND
///   HOST's broker: astream's sealed record layer
///   (`serve_tcp_sealed`, XChaCha20-Poly1305 under the 64-hex pre-shared key)
///   and ALWAYS guarded. The guard is not optional on TCP, and that is the
///   point of it: the pre-shared key is ONE secret every host holds, so it
///   keeps a stranger off the wire and says nothing about which host is
///   speaking. What stops the joined host publishing as the first host's node,
///   or as a human (mail delivered `trust=human`, a fleet halt or its lift),
///   is the capability it attaches — minted for its own node id by `aterm
///   fabric mint-for` — and only a guarded broker checks one. A non-loopback bind is refused without `--allow-remote`. Only
///   in a `sealed` build ([`transport::SEALED`]); a default build answers
///   `--tcp` by naming the feature. `--unix <socket>` serves that Unix socket
///   AS WELL, from the same guarded broker and log: the host that serves the
///   sealed wire points its OWN bridges there (`aterm fabric on --tcp`),
///   because astream admits only 64 connections into the sealed handshake at
///   once and a peer that can reach the port can hold them without the key.
///
/// The key and the secret are files, never arguments (an argv is world-readable
/// in `ps`), and both must be mode 0600 ([`transport::check_private`]).
///
/// IT EXITS WHEN ITS BUILD IS REPLACED. A supervised broker is a `KeepAlive`
/// launchd job or a `Restart=always` unit, and an update swaps the file its
/// argv names in place: without this the old broker serves until the next
/// login while every bridge runs the new build, so a broker-side fix — or a
/// wire change both ends must share — never arrives. Every [`EXE_WATCH`] it
/// looks at its executable; once the path names a different, settled file
/// ([`replaced`]) it shuts down (the socket unlinked) and exits 0, and the
/// supervisor starts the new build from the unchanged argv. A broker nothing
/// supervises then stays down: run one by hand under something that restarts
/// it, or restart it yourself.
fn broker(args: &[String]) -> ExitCode {
    // Capture the running image and invocation before opening the broker or
    // announcing readiness. An update during startup must remain observable.
    let watch = ExeWatch::capture();
    const USAGE: &str = "\
usage: aterm link broker <socket> [log] [--secret-file <path>]
       aterm link broker --tcp <host:port> --key-file <path> --secret-file <path>
                         [--allow-remote] [--unix <socket>] <log>
       aterm link broker --repair <log>

  <socket>         the Unix socket path the bridge's `--broker` names
  [log]            the durable record log (default: <socket>.log; required with --tcp)
  --secret-file    32+ raw bytes, 0600: the MINT secret. The broker becomes
                   GUARDED — every attach must carry a capability minted under
                   it (`aterm link mint`, `aterm fabric mint-for`), and each
                   request is checked against the grants attached. Without it
                   on a socket, the `mint.secret` beside <socket> is the secret
                   when there is one (`aterm fabric on`'s root)
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
  --repair <log>   serve nothing: TRUNCATE <log> at its first CORRUPT record,
                   which the broker refuses to open, and print how many bytes
                   went. The records from that one on are lost — possibly
                   acked mail — and everything before it is kept. A log that
                   opens prints `nothing to repair`; a log a NEWER aterm wrote
                   is refused untouched (run that release). Refused while a
                   broker holds the log

The Unix socket is created world-connectable, so who can connect is bounded by the
DIRECTORY you put it in: use a 0700 one (`aterm fabric on` does). A socket broker
with no --secret-file and no mint.secret beside its socket checks no capability on
attach, and says `(unguarded)` on stderr. Prints `listening <endpoint>` once
it is bound — the bound TCP address when the port was 0 — and, with --unix, a
second `listening <socket>` line once that is bound too. It exits 0 within a minute
of its executable being replaced by another build, for its supervisor (launchd
KeepAlive, systemd Restart=always) to start that one.
";
    let mut tcp: Option<String> = None;
    let mut key_file: Option<String> = None;
    let mut secret_file: Option<String> = None;
    let mut unix: Option<String> = None;
    let mut allow_remote = false;
    let mut repair = false;
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
            "--repair" => repair = true,
            other if other.starts_with('-') => {
                eprintln!("aterm link broker: unknown flag `{other}`");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
            word => words.push(word),
        }
        i += 1;
    }
    // THE REPAIR SERVES NOTHING: one local file, and no other flag of the
    // line means anything to it — refused rather than ignored.
    if repair {
        if tcp.is_some()
            || key_file.is_some()
            || secret_file.is_some()
            || unix.is_some()
            || allow_remote
        {
            eprintln!("aterm link broker: --repair takes <log> alone: it serves nothing");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        let [log] = words.as_slice() else {
            eprintln!("aterm link broker: --repair takes exactly one <log>");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        };
        return repair_log(log);
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
    // THE ROOT'S SECRET GUARDS THE ROOT'S SOCKET (the doc above). Present at
    // all — a symlink, an unreadable file — is enough to make it the secret,
    // so a broken one is the reader's refusal below, never an unguarded bus.
    let secret_file = secret_file.or_else(|| {
        let beside = std::path::Path::new(&endpoint).with_file_name(crate::enable::MINT_SECRET);
        (tcp.is_none() && std::fs::symlink_metadata(&beside).is_ok())
            .then(|| beside.to_string_lossy().into_owned())
    });
    let secret = match secret_file
        .as_deref()
        .map(|p| (p, transport::read_mint_secret(p)))
    {
        None => None,
        Some((_, Ok(s))) => Some(s),
        Some((path, Err(e))) => {
            eprintln!(
                "aterm link broker: --secret-file {}",
                transport::describe(path, &e)
            );
            return ExitCode::FAILURE;
        }
    };
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
            // THE LAST LINE IS ATERM'S REMEDY, when the refusal has one: the
            // supervisor appends this stderr to `<root>/broker.err`, whose last
            // line `aterm fabric` and doctor quote ([`log_refusal_remedy`]).
            if let Some(remedy) = log_refusal_remedy(e.kind(), &log) {
                eprintln!("aterm link broker: {remedy}");
            }
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
    // A secret that did not read stopped the broker above, so a named one
    // here is the one guarding it.
    let guard_note = match secret_file.as_deref() {
        Some(path) => format!(" (guarded by {path})"),
        None => " (unguarded: it checks no capability)".to_string(),
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
    // The broker serves on its own threads; this one watches the executable.
    // `BrokerHandle` has no `join` — `asb serve` parks the same way — and
    // dropping the handle shuts the broker down and unlinks the socket, which
    // is how it EXITS when its build is replaced (the doc above).
    let keep = (handle, beside);
    let Some(mut watch) = watch else {
        // No executable to watch: serve until killed, as before the watch.
        loop {
            std::thread::park();
        }
    };
    loop {
        std::thread::park_timeout(EXE_WATCH);
        if watch.poll() {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "aterm link broker: {} was replaced by another build; exiting so the supervisor \
                 starts that one",
                watch.path.display()
            );
            drop(keep);
            return ExitCode::SUCCESS;
        }
    }
}

/// The words that OPEN aterm's remedy for a log astream refuses as CORRUPT —
/// the last line `aterm link broker` writes before it exits, and so the last
/// word of `<root>/broker.err` that `aterm fabric` and doctor quote. The fix
/// they print for it ([`crate::enable::fix_for`]) is chosen on these words,
/// which this binary writes; astream's own refusal names `asb repair`, which
/// aterm does not ship.
pub const CORRUPT_LOG_REMEDY: &str = "the log has a corrupt record and is not opened: run `aterm \
                                      link broker --repair";

/// The words that OPEN aterm's line for a log astream refuses because a NEWER
/// format wrote it (`Unsupported`): no repair applies — truncating would
/// destroy records the release that wrote them reads — so the remedy is that
/// release. Chosen on by [`crate::enable::fix_for`], like
/// [`CORRUPT_LOG_REMEDY`], and never answered with a repair.
pub const NEWER_LOG_REMEDY: &str = "the log was written by a newer aterm and is not opened";

/// Aterm's remedy for a broker log astream refused to open with `kind`, as one
/// line naming `log` — `None` for a refusal with no remedy of aterm's own (a
/// lock held, a permission), whose astream words say enough.
///
/// By KIND, never by astream's wording. In aterm's build (no retention, no
/// anti-rollback) `InvalidData` from an open is the corrupt-record refusal,
/// and `Unsupported` a record format newer than this build reads.
#[must_use]
pub fn log_refusal_remedy(kind: std::io::ErrorKind, log: &str) -> Option<String> {
    match kind {
        std::io::ErrorKind::InvalidData => Some(format!(
            "{CORRUPT_LOG_REMEDY} '{}'` to cut it there (the records from it on are lost)",
            log.replace('\'', "'\\''")
        )),
        std::io::ErrorKind::Unsupported => Some(format!(
            "{NEWER_LOG_REMEDY}: run that aterm release or a later one — no repair applies, and \
             {log} is untouched"
        )),
        _ => None,
    }
}

/// `aterm link broker --repair <log>`: astream's explicit, operator-invoked
/// repair (`BrokerLog::open_repair`, the `asb repair` aterm does not ship) —
/// the log TRUNCATED at its first corrupt record, the bytes dropped counted.
/// Idempotent: a log that opens is left as it is and says `nothing to repair`.
///
/// A log that is not there is refused, not created: `open_repair` would leave
/// an empty log behind a typo. A broker holding the log (astream's exclusive
/// lock, `AlreadyExists`) is refused — repair it between its supervisor's
/// restarts, or with the job stopped. A NEWER-format log is refused untouched
/// by astream itself. The repair moves the log's identity on, so every bridge
/// forgets the offsets it held on the cut log at its next attach.
fn repair_log(log: &str) -> ExitCode {
    match std::fs::metadata(log) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            eprintln!("aterm link broker --repair: {log} is not a file");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("aterm link broker --repair: {log}: {e}");
            return ExitCode::FAILURE;
        }
    }
    let said = match astream_broker::BrokerLog::open_repair(log, astream_broker::Durability::Strict)
    {
        Ok((_, 0)) => format!("{log}: nothing to repair"),
        Ok((_, dropped)) => {
            format!("{log}: repaired, dropped {dropped} byte(s) from the corrupt record on")
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            eprintln!(
                "aterm link broker --repair: {log} is held by a running broker ({e}); repair it \
                 while no broker has it open"
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("aterm link broker --repair: {log}: {e}");
            // A newer format is refused by astream on the repair too, and
            // aterm's line names the release that reads it.
            if e.kind() == std::io::ErrorKind::Unsupported {
                if let Some(remedy) = log_refusal_remedy(e.kind(), log) {
                    eprintln!("aterm link broker --repair: {remedy}");
                }
            }
            return ExitCode::FAILURE;
        }
    };
    use std::io::Write as _;
    // WRITTEN, NEVER PRINTED: a closed stdout must not panic the repair.
    let _ = writeln!(std::io::stdout().lock(), "{said}");
    ExitCode::SUCCESS
}

/// How often a serving broker looks at its executable: slow, because a
/// replaced build can wait half a minute and the look is one `stat`.
const EXE_WATCH: std::time::Duration = std::time::Duration::from_secs(30);

/// What a file IS — device, inode, length and modification time — so a
/// REPLACED executable (a new inode renamed over the path, or the same inode
/// rewritten) is told from the one a process started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl FileId {
    /// The regular file at `path`, symlinks followed — the file a supervisor
    /// that runs `path` would start.
    ///
    /// # Errors
    ///
    /// The `stat`, or a path that is not a regular file.
    fn of(path: &std::path::Path) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        if !m.is_file() {
            return Err(std::io::Error::other(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(Self {
            dev: m.dev(),
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        })
    }
}

/// The mapped image, rather than a pathname that an update may already have
/// replaced. Linux exposes its full metadata through `/proc`; elsewhere the
/// report's existing process-image probe can retain the original inode even
/// when `current_exe` now names another file.
enum RunningImage {
    File(FileId),
    #[cfg(not(target_os = "linux"))]
    Inode(u64),
}

impl RunningImage {
    fn capture() -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            FileId::of(std::path::Path::new("/proc/self/exe"))
                .ok()
                .map(Self::File)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let inode = crate::fabric::running_image_inode(std::process::id());
            let file = std::env::current_exe()
                .ok()
                .and_then(|p| FileId::of(&p).ok());
            match (file, inode) {
                (Some(file), Some(inode)) if file.ino == inode => Some(Self::File(file)),
                (_, Some(inode)) => Some(Self::Inode(inode)),
                // Without an OS image answer, retain the early file snapshot.
                // This still covers replacement after broker startup.
                (Some(file), None) => Some(Self::File(file)),
                (None, None) => None,
            }
        }
    }

    fn matches(&self, now: &FileId) -> bool {
        match self {
            Self::File(start) => start == now,
            #[cfg(not(target_os = "linux"))]
            Self::Inode(inode) => *inode == now.ino,
        }
    }
}

/// The supervisor's invocation, without resolving its final symlink. Watching
/// `current_exe` on Linux instead would pin the old symlink target forever.
fn invocation_path(
    argv0: &std::ffi::OsStr,
    path: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let invocation = std::path::Path::new(argv0);
    let file = if invocation.components().count() == 1 && !invocation.is_absolute() {
        crate::enable::which_in(argv0, path?)?
    } else {
        invocation.to_path_buf()
    };
    std::path::absolute(file).ok()
}

struct ExeWatch {
    path: std::path::PathBuf,
    start: RunningImage,
    seen: Option<FileId>,
}

impl ExeWatch {
    fn capture() -> Option<Self> {
        let start = RunningImage::capture()?;
        let argv0 = std::env::args_os().next()?;
        let path = invocation_path(&argv0, std::env::var_os("PATH").as_deref())?;
        Some(Self {
            path,
            start,
            seen: None,
        })
    }

    fn poll(&mut self) -> bool {
        let now = FileId::of(&self.path);
        let changed = replaced(&self.start, self.seen.as_ref(), &now);
        self.seen = now.ok();
        changed
    }
}

/// Whether the executable a broker started from has been REPLACED, so it
/// should exit and let its supervisor (`KeepAlive`, `Restart=always`) start
/// the new build — otherwise an update leaves the old broker serving until the
/// next login, and broker-side fixes in the new build never run.
///
/// `start` is the file at start, `seen` the one the PREVIOUS look found, `now`
/// this look's. Replaced means a different file that is SETTLED: the same on
/// two looks running, so a build still being written is never the one the
/// supervisor is sent to start. A path that is gone or is not a regular file
/// (an update mid-swap, an uninstall) is not a replacement: exiting into a
/// supervisor with nothing to start is an outage, so the broker keeps serving
/// and looks again.
fn replaced(start: &RunningImage, seen: Option<&FileId>, now: &std::io::Result<FileId>) -> bool {
    matches!(now, Ok(now) if !start.matches(now) && seen == Some(now))
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
  --secret-file    32+ raw bytes, 0600: the mint key, used byte for byte (a file
                   any other user can read, or a shorter one, is refused)

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
    // The one reader the broker that verifies these caps reads the secret
    // with: a group-readable or short secret is refused here as it is there.
    let key = match transport::read_mint_secret(secret_path) {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "aterm link mint: --secret-file {}",
                transport::describe(secret_path, &e)
            );
            return ExitCode::FAILURE;
        }
    };
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

    /// A REPLACED BUILD is a different, SETTLED file at the executable's
    /// path: the same file never is; a new build renamed over the path, or
    /// the file rewritten in place, is once two looks agree; a path that is
    /// gone or is not a regular file never is.
    #[test]
    fn a_broker_exits_only_for_a_settled_replacement_of_its_executable() {
        let dir = std::env::temp_dir().join(format!("atlink-exe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let exe = dir.join("aterm");
        std::fs::write(&exe, b"build one").expect("write");
        let start = FileId::of(&exe).expect("start");
        let image = RunningImage::File(start);
        let same = FileId::of(&exe);
        assert!(!replaced(&image, None, &same), "the same file");
        assert!(
            !replaced(&image, same.as_ref().ok(), &same),
            "the same file, looked at twice"
        );
        // A new build renamed over the path: replaced once it is settled.
        std::fs::write(dir.join("aterm.new"), b"build two").expect("write");
        std::fs::rename(dir.join("aterm.new"), &exe).expect("rename over");
        let first = FileId::of(&exe);
        assert!(
            !replaced(&image, Some(&start), &first),
            "seen once: it may still be being written"
        );
        let second = FileId::of(&exe);
        assert!(replaced(&image, first.as_ref().ok(), &second), "settled");
        // The same inode rewritten in place is a replacement too.
        let rewritten = FileId::of(&exe).expect("now");
        std::fs::write(&exe, b"build three, longer").expect("rewrite in place");
        let again = FileId::of(&exe);
        assert!(replaced(
            &RunningImage::File(rewritten),
            again.as_ref().ok(),
            &FileId::of(&exe)
        ));
        // Gone, or a directory in its place: never — exiting would leave the
        // supervisor nothing to start.
        std::fs::remove_file(&exe).expect("remove");
        let gone = FileId::of(&exe);
        assert!(gone.is_err());
        assert!(!replaced(&image, second.as_ref().ok(), &gone));
        std::fs::create_dir(&exe).expect("a directory where the file was");
        assert!(FileId::of(&exe).is_err(), "not a regular file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_executable_watch_follows_the_invocation_symlink_through_a_swap() {
        use std::ffi::OsStr;
        use std::os::unix::fs::{symlink, PermissionsExt};

        let scratch = aterm_tempfile::TempDir::new().expect("scratch");
        let dir = scratch.path();
        let old = dir.join("build-one");
        let new = dir.join("build-two");
        for file in [&old, &new] {
            std::fs::write(file, b"executable fixture").expect("write");
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
        }
        let invocation = dir.join("aterm");
        symlink(&old, &invocation).expect("invocation symlink");
        let resolved = invocation_path(OsStr::new("aterm"), Some(dir.as_os_str()))
            .expect("bare argv0 resolved on PATH");
        assert_eq!(resolved, invocation, "retain the final symlink");
        assert_eq!(
            invocation_path(invocation.as_os_str(), None),
            Some(invocation.clone())
        );
        assert!(invocation_path(OsStr::new("aterm"), None).is_none());
        let original = FileId::of(&old).expect("running image");
        let mut watch = ExeWatch {
            path: resolved,
            start: RunningImage::File(original),
            seen: None,
        };
        assert!(!watch.poll());
        assert!(!watch.poll(), "an unchanged invocation keeps serving");

        symlink(&new, dir.join("aterm.next")).expect("new symlink");
        std::fs::rename(dir.join("aterm.next"), &invocation).expect("retarget atomically");
        assert!(!watch.poll(), "one observation is not settled");
        std::fs::remove_file(&invocation).expect("swap gap");
        assert!(
            !watch.poll(),
            "a missing invocation never requests a restart"
        );
        symlink(&new, &invocation).expect("finish swap");
        assert!(!watch.poll(), "the gap resets the settlement observation");
        assert!(watch.poll(), "restart onto the new symlink target");
        assert_eq!(FileId::of(&old).expect("old image untouched"), original);
    }

    #[test]
    fn an_executable_watch_keeps_its_running_identity_across_readiness() {
        let scratch = aterm_tempfile::TempDir::new().expect("scratch");
        let invocation = scratch.path().join("aterm");
        std::fs::write(&invocation, b"running image").expect("old image");
        let original = FileId::of(&invocation).expect("image at startup");
        let mut watch = ExeWatch {
            path: invocation.clone(),
            start: RunningImage::File(original),
            seen: None,
        };

        // An update completes while the broker opens its log and listener,
        // before it announces readiness. Observations still compare with the
        // captured running image, rather than adopting the replacement.
        let replacement = scratch.path().join("aterm.next");
        std::fs::write(&replacement, b"replacement image").expect("replacement");
        std::fs::rename(&replacement, &invocation).expect("replace before readiness");
        let after_ready = FileId::of(&invocation).expect("file after readiness");
        assert!(!watch.poll());
        assert!(watch.poll(), "the replacement cannot become the baseline");
        assert!(
            !replaced(
                &RunningImage::File(after_ready),
                Some(&after_ready),
                &Ok(after_ready)
            ),
            "negative control: a baseline captured after readiness misses the update"
        );

        // A path already replaced before the watcher is constructed must
        // likewise be compared to the OS's mapped image, not its current file.
        let mut already_replaced = ExeWatch {
            path: invocation,
            start: RunningImage::capture().expect("this process's mapped executable"),
            seen: None,
        };
        assert!(!already_replaced.poll());
        assert!(already_replaced.poll());
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
                "title",
                "proto",
                "aterm"
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
            "proto=-",
            "aterm=-",
        ] {
            assert!(line.contains(absent), "{absent} missing from: {line}");
        }
        // A NODE's row says what it runs, in the last two columns.
        let (node_body, _) = crate::body::Body::decode(
            b"v=1 t=1 state=live inc=2 fabric=connected host=m7 pid=1 proto=2 aterm=0.5.0",
        );
        let (line, _) = row("/f/f1/pub/n-a/node/presence", &node_body);
        assert!(line.ends_with(" proto=2 aterm=0.5.0"), "{line}");
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
            20,
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
