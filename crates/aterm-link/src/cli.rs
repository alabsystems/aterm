// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link`: the fabric bridge's command line (`USAGE` lists the subcommands).
//!
//! ```text
//! aterm-link serve --fleet <F> --broker <socket>
//!                  --cap-file <path>… [--state DIR] [--accept-from <p>,…]
//!                  [--sock <path> --token-file <path>]
//! aterm-link ls    --fleet <F> --broker <socket>
//!                  --cap-file <path>… [--attention]
//! ```
//!
//! An unknown word is refused by name (exit 2) rather than answered with the
//! usage text: a typo that prints help reads as a request for help. The one
//! exception is the retired `hook`: a surviving hook entry still runs it on
//! every prompt, where exit 2 is Claude Code's blocking error, so it answers
//! exit 0, nothing on stdout, and one line on stderr ([`retired_hook`]).
//!
//! ## `ls` columns
//!
//! §7 pins the row as `<node> <host> <sid> state= inc= role= detail= driving=
//! holder= hold= fabric= attention=`, and §9.3 uses `ls --attention` as the
//! human's escalation view. Both are printed in that order, and `--attention`
//! keeps only the rows carrying one.
//!
//! Three of those columns have no writer on session rows and print `-` ("unknown"):
//!
//! * `driving=` is in §4.2's presence body, but
//!   [`crate::bridge::Bridge::publish_session_presence`] does not publish it.
//!   (`role=` and `detail=` are written by [`crate::presence`] on a
//!   `presence = "meta"` bridge, the default; a `minimal` bridge shows `-`.)
//! * `host=` and `fabric=` are published on the node's own presence row only
//!   (`bridge.rs`'s `bring_presence_up`): `fabric=` on a session row was a
//!   constant nothing could falsify. A session row shows `-` for both; the
//!   node row above it carries the answer.
//!
//! Six columns are added at the end: `gen=` (§6.6's `<content_seq>:<fp16>`, what a
//! `gen=`-bound approval is minted against), `observer=` (§11.2's watching,
//! non-hosting attachment — a row an operator must not read as a host),
//! `epoch=` (the launch nonce a drive record is fenced on), `phase=` (the
//! server's `status agent=` verdict, relayed), `context=` (no longer written;
//! printed for rows an older bridge wrote) and `title=` (the session's `meta
//! set title` user title, else `-`; never the terminal's title, which the
//! program writes — [`crate::presence`]; 128 bytes).
//!
//! Every printed field is untrusted: a presence body is written by whatever node
//! holds `rw,p=<n>:/f/<F>/pub/<n>/>`, and `attention=` is free text a session
//! chose. Each column is normalised through [`crate::pct`] before printing, so
//! a row is always one whitespace-delimited token per column of printable ASCII
//! and no body can move an operator's cursor.

use std::process::ExitCode;

use crate::bridge::{Bridge, Config};
use crate::transport;

/// Usage, printed to stderr on a usage error (exit 2, aterm's convention).
const USAGE: &str = "\
aterm-link — the aterm fabric bridge

  aterm-link serve --fleet <F> --broker <socket> --cap-file <path>... [options]
  aterm-link ls    --fleet <F> --broker <socket> --cap-file <path>... [--attention]
  aterm-link notify --on <attention|ask:<person>|halt>,... --exec <cmd>
                    a command run when the fabric needs a human (`notify` for its usage)
  aterm-link broker <socket> [log] [--secret-file <path>] [--durability strict|ordered|relaxed]
                    the local bus itself (`broker` for its usage); ordered and
                    relaxed ack before the full flush, and a power loss can take
                    the newest machine records they acked
  aterm-link broker --repair <log>
                    truncate a log the broker refuses as corrupt at that record
  aterm-link mint   <grant> --secret-file <path>    one capability line for --cap-file
                    (a person's whole ring: `aterm fabric mint-for h-<person>`)
  aterm-link fabric ...  the same as `aterm fabric` (`fabric help` for its usage)

`ls` defaults its flags from the rendezvous file `aterm fabric on` writes beside the
instance control sockets (fabric.toml): --fleet and --broker, and WITH a defaulted
--broker only, the node's --cap-file and --state (a cap is minted for that one broker,
so it is never presented to a --broker you named). A flag given on the command line wins.

  --fleet <F>            the fleet name (the `/f/<F>/` subtree)
  --broker <socket>      the broker's Unix socket path
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
  --log <auto|off|turns> `serve` only, `auto` by default: mirror each
                         session log's turns, runs, needs and the steps they
                         rise from onto its session's `ev` face, deduplicated
                         by readers (`aterm fabric brief` folds them), while
                         the broker retains them (`fabric status` says
                         `mirror=unretained` where it does not); `turns`
                         mirrors over any broker, `off` never. Without the
                         flag, `[fabric] log` in aterm.toml.
  --attention            `ls` only: keep only rows carrying an `attention=`

`ls` prints one line per node and per session: <node> <host> <sid> state= inc=
role= detail= driving= holder= hold= fabric= attention= gen= observer= epoch=
phase= context= title= proto= aterm=, each value pct-encoded (`12%` reads `12%25`).
driving= always reads `-`; holder= and context= do too unless an older node wrote
them, and role= detail= phase= title= do from a `minimal` bridge. <host>, fabric=,
proto= (the astream protocol the node speaks) and aterm= (the release it runs) are
on the node's own line; a session's line reads `-` for them.
";

/// The bridge's whole command line, as a LIBRARY entry.
///
/// A library entry so the ONE `aterm` binary carries it: `aterm link ...` and
/// the `aterm-link` symlink both land here.
#[must_use]
pub fn dispatch(args: &[String]) -> ExitCode {
    dispatch_with_inherited_lanes(args, || {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "inherited bridge lanes require explicit ownership admission at executable startup",
        ))
    })
}

/// Dispatch with the composing executable's one-shot lane admission. It is
/// called only after parsing selects inherited `serve`, never for observer or
/// other verbs. The provider transfers already-owned descriptors; this safe
/// library never infers ownership from raw descriptor flags.
#[must_use]
pub fn dispatch_with_inherited_lanes(
    args: &[String],
    take_lanes: impl FnOnce() -> std::io::Result<crate::bridge::InheritedLanes>,
) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("serve") => run(&args[1..], true, take_lanes),
        // The read-side verbs default their flags from the rendezvous file
        // (`crate::enable`); `serve` does not, because aterm launches it with
        // every flag spelled out and a bridge that silently took a different
        // broker than its config names would make the report lie about itself.
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
            run(
                &crate::enable::with_rendezvous_defaults(&args[1..]),
                false,
                take_lanes,
            )
        }
        Some("notify") => crate::notify::main(&args[1..]),
        Some("broker") => broker(&args[1..]),
        Some("mint") => mint(&args[1..]),
        Some("fabric") => crate::fabric::main(&args[1..]),
        // Retired: a no-op, never a refusal ([`retired_hook`]).
        Some("hook") => retired_hook(),
        // Asked for: stdout, exit 0, like `aterm --help` and `broker -h`.
        Some("-h" | "--help" | "help") => aterm_uds::stdout::answer_exit("aterm link", USAGE, 0),
        // Refused by name (module doc).
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

/// `aterm link hook …`, any arguments: exit 0, nothing on stdout, one stderr line
/// ([`RETIRED_HOOK_LINE`]). Permanent: no cleanup reaches every project's settings file.
///
/// A hook entry aterm once wrote can survive in a project's `.claude/settings.local.json` or a
/// running Claude Code, where exit 2 is a blocking error and a `UserPromptSubmit` or
/// `SessionStart` hook's stdout joins the model's context. Stdin (the event's JSON) is drained
/// first, uncapped, so the writer never meets a closed pipe; a terminal is never read.
fn retired_hook() -> ExitCode {
    use std::io::IsTerminal as _;
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let _ = std::io::copy(&mut stdin.lock(), &mut std::io::sink());
    }
    eprintln!("{RETIRED_HOOK_LINE}");
    ExitCode::SUCCESS
}

fn run(
    args: &[String],
    serve: bool,
    take_lanes: impl FnOnce() -> std::io::Result<crate::bridge::InheritedLanes>,
) -> ExitCode {
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
        // `serve` defaults `[fabric] presence` and `receipts` from aterm.toml: they shape what
        // the bridge publishes, not which broker it reaches.
        if !parsed.presence_given {
            cfg.presence = crate::fabric::presence_from_config();
        }
        if !parsed.receipts_given {
            cfg.receipts = crate::fabric::receipts_from_config();
        }
        // And `[fabric] log`, the bus mirror's switch (session log phase 4):
        // what the bridge publishes, never which broker it reaches.
        if !parsed.log_given {
            cfg.log = crate::fabric::log_from_config();
        }
        let bridge = if cfg.sock.is_some() && cfg.token.is_some() {
            Bridge::new(cfg)
        } else {
            take_lanes().and_then(|lanes| Bridge::with_inherited_lanes(cfg, lanes))
        };
        match bridge.and_then(Bridge::run) {
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
    /// Whether `--log` was on the command line — else `serve` reads
    /// `[fabric] log` ([`crate::fabric::log_from_config`]).
    pub(crate) log_given: bool,
}

/// One printed column as one safe token: printable ASCII, no whitespace, no escape sequence,
/// `-` when empty.
///
/// A value arrives pct-encoded by its writer, or not if the writer is hostile. Decoding and
/// re-encoding ([`crate::pct`]) is idempotent on a well-formed token and total on a malformed
/// one.
fn column(raw: &str) -> String {
    let mut safe = crate::pct::encode(&crate::pct::decode(raw));
    // Bounded by the cap the bridge's writer and `glance` use (another host's row need not
    // have gone through that writer), cut on an escape boundary, never inside a `%XX`.
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

/// `aterm-link ls` — every node's roster: `Last{/f/<F>/pub/*/*/presence}`, which
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
    let (mut client, _closer) = transport::connect(&cfg.broker, crate::fabric::IO_TIMEOUT)
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("cannot connect to the broker at {}: {e}", cfg.broker),
            )
        })?;
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

/// One roster line, and whether it is an escalation; split out of [`roster`] so §7's column
/// set is testable without a broker.
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
    // The test `notify::Sel::Attention` makes: absent, empty and `-` are all no escalation, so
    // `ls --attention` and the notifier cannot disagree.
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
        // Known body fields (§4.1): parsed into `Body`, so never in `unknown`.
        body.gen.as_deref().map_or_else(|| "-".into(), column),
        // Absent means `0`, not unknown: only an observer attachment writes it (§11.2).
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

/// Parse argv. A flag that takes a value and has none is a usage error, not a defaulted
/// setting.
///
/// `aterm fabric` reads the fleet, broker, cap files and state dir out of `[fabric] command`
/// with this parser, the one the bridge was started with, so the two cannot disagree.
pub(crate) fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut attention = false;
    let mut presence_given = false;
    let mut receipts_given = false;
    let mut log_given = false;
    let mut cfg = Config {
        fleet: String::new(),
        broker: String::new(),
        cap_files: Vec::new(),
        state_dir: String::new(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::Meta,
        // On, matching `receipts_from_config`'s absent-key answer and what `aterm fabric on`
        // writes.
        receipts: true,
        // AUTO, matching `log_from_config`'s absent-key answer (D4, session
        // log Phase 6 step 6E-3): on over a broker that retains it.
        log: crate::mirror::LogMode::Auto,
    };
    let mut token_file: Option<String> = None;
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
            "--log" => {
                let v = value()?;
                cfg.log = crate::mirror::LogMode::parse(&v)
                    .ok_or_else(|| format!("--log {v}: `auto`, `off` or `turns`"))?;
                log_given = true;
            }
            "--sock" => cfg.sock = Some(value()?),
            "--token-file" => token_file = Some(value()?),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if cfg.fleet.is_empty() || cfg.broker.is_empty() {
        return Err("--fleet and --broker are required".to_string());
    }
    if !crate::subject::is_fleet(&cfg.fleet) {
        return Err(format!(
            "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            cfg.fleet
        ));
    }
    if cfg.state_dir.is_empty() {
        cfg.state_dir = default_state_dir();
    }
    // Read as every control client reads an instance token
    // (`aterm_ctl::read_control_token_file`): a symlink is refused, only a regular file is
    // read, and no more than a token's bound of it.
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
        log_given,
    })
}

/// `$XDG_STATE_HOME/aterm-link`, else `$HOME/.local/state/aterm-link`, else
/// `./aterm-link-state`; never a temp dir, because losing the node id loses the node's mail
/// lane. `aterm-link notify` keeps its journal under the same directory (§9.3).
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
/// A Unix socket created world-connectable — nothing chmods it and no peer uid
/// is checked — so who can connect is bounded by the 0700 directory you put it
/// in. What a connection may say is the guard's: with `--secret-file` it is
/// `Broker::open_guarded`, every attach must carry a capability minted under
/// that secret, and every request is authorized against the grants attached.
/// The root's secret guards the root's socket: with no `--secret-file`, a
/// `mint.secret` beside the socket (`aterm fabric on`'s layout) is the secret,
/// so a supervisor job written before `on` passed the flag comes back guarded
/// the next time it starts the broker — one that cannot be read is a refusal,
/// never an unguarded broker. Only a socket with no secret beside it is served
/// unguarded, and the banner says so.
///
/// The secret is a file, never an argument (an argv is world-readable in `ps`),
/// and it must be mode 0600 or tighter, read by astream's one secret-file rule
/// ([`transport::read_mint_secret`]).
///
/// `--durability strict|ordered|relaxed` is what an ack promises, guarded or
/// not ([`crate::durability`]): `strict` acks after the full flush; `ordered`
/// after an ordering barrier, with a 100 ms horizon plus one batch and one
/// full flush, and before the ack of every record aterm's escalation table
/// names (a halt, a person's own words, mail to a person, a person's read
/// mark), served strict by astream where the device cannot promise the order;
/// `relaxed` after the page-cache write, which survives a broker crash and not
/// a power loss (the table applies there too). A broker of any tier but strict
/// takes the stop signals: its clean stop — SIGTERM, SIGINT or SIGHUP, or its
/// exit for a replaced build — drops its listener, and astream flushes the log
/// and records it `Closed`, so a reboot after it moves nothing. A weak run
/// that never stopped cleanly moves the log's generation, with lineage, at the
/// first open in another boot ([`astream_broker::LogIdentity`]), so every
/// bridge and notifier resumes where the log continues instead of trusting an
/// offset a power loss took. A strict broker keeps the signals' default action.
/// A bare argv runs the build's default ([`crate::durability::default_for`]:
/// ordered for a broker whose argv names `--secret-file`, which astream serves
/// strict where the device cannot keep the order, and strict for one naming
/// none) and a named tier — strict included — is kept, so `aterm fabric` reads
/// the tier off the process table and the tier served off the broker's `Hello`.
///
/// It exits when its build is replaced. A supervised broker is a `KeepAlive`
/// launchd job or a `Restart=always` unit, and an update swaps the file its
/// argv names in place: without this the old broker serves until the next
/// login while every bridge runs the new build, so a broker-side fix — or a
/// wire change both ends must share — never arrives. Every [`EXE_WATCH`] it
/// looks at its executable; once the path names a different, settled file
/// ([`replaced`]) it shuts down (the socket unlinked) and exits 0, and the
/// supervisor starts the new build from the unchanged argv. A broker nothing
/// supervises then stays down: run one by hand under something that restarts
/// it, or restart it yourself.
///
/// It exits when its log is poisoned ([`crate::broker_health`]). astream
/// refuses every write to a log whose full flush failed with acked records at
/// risk, or whose rollback could not put the file back, until the log is
/// reopened — and only a new open reopens it. Every
/// [`crate::broker_health::POLL`] (2 s) the broker asks astream whether its
/// log is poisoned; when it is, it writes one line naming astream's reason on
/// stderr (the last line of `<root>/broker.err`) and exits
/// [`crate::broker_health::POISONED_EXIT`] (75), and the supervisor's restart
/// reopens the log, moving its generation when astream recorded the run
/// lossy. A broker no `aterm fabric on` job runs exits the same, and its line
/// names the command that restarts it.
///
/// It retains its log by aterm's default rules ([`crate::broker_rules`],
/// session log Phase 6 step 6E-2), set at every open before anything is
/// served, so its first `Hello` advertises them.
fn broker(args: &[String]) -> ExitCode {
    // Capture the running image and invocation before opening the broker or
    // announcing readiness. An update during startup must remain observable.
    let watch = ExeWatch::capture();
    const USAGE: &str = "\
usage: aterm link broker <socket> [log] [--secret-file <path>] [--durability <tier>]
       aterm link broker --repair <log>

  <socket>         the Unix socket path the bridge's `--broker` names
  [log]            the durable record log (default: <socket>.log)
  --secret-file    32+ raw bytes, 0600: the MINT secret. The broker becomes
                   GUARDED — every attach must carry a capability minted under
                   it (`aterm link mint`, `aterm fabric mint-for h-<name>`), and
                   each request is checked against the grants attached. Without
                   it, the `mint.secret` beside <socket> is the secret when there
                   is one (`aterm fabric on`'s root)
  --durability <tier>
                   what an ack promises: strict, ordered or relaxed. Unnamed,
                   it is the build's default: ordered for a broker with
                   --secret-file (what `aterm fabric on` runs), strict for a
                   broker naming no secret. strict acks once the batch is
                   flushed in full (on macOS F_FULLFSYNC, a flush of the
                   drive's own cache): a power loss loses nothing acked.
                   relaxed acks once it is written to the page cache, without
                   waiting for that flush: it survives a crash or restart of
                   the broker, but NOT a power loss, kernel panic or hard
                   reset, which can take any machine record acked since the
                   broker last started or stopped cleanly. ordered acks once
                   the batch is ORDERED on the disk (F_BARRIERFSYNC, a
                   fraction of the full flush's cost) and flushes in full
                   100 ms after the first ack no full flush covers, once the
                   batch in hand is written: a panic or forced power-off can
                   lose the last 100 ms of machine records, plus a batch and a
                   full flush, and the log keeps a prefix of whole batches. On
                   both, a halt, a person's own words, mail to a person and a
                   person's read mark are flushed in full before their ack
                   (on an unguarded broker a person's
                   posts are not told apart). astream serves ordered strict
                   where the disk cannot promise the order: off macOS, a log
                   off the boot volume, and says why in its start line. A
                   broker of any tier but strict stops cleanly on SIGTERM
                   (launchd at logout or shutdown, `aterm fabric off`), SIGINT
                   or SIGHUP, and when it exits for a replaced build: the log
                   is flushed and marked closed, so the next boot changes
                   nothing. After an ordered or relaxed stop that never got
                   that far (power loss, panic, reset, SIGKILL) the first open
                   in another boot
                   moves the log's GENERATION and records where the two agree:
                   every bridge and notifier resumes where the log continues —
                   nothing below that point re-read, nothing posted after the
                   restart skipped — and only the records the loss took are
                   gone. A kill -9 in the same boot costs nothing when the log
                   is on the boot volume. Off it (a separate /home partition,
                   an external disk), and on a host whose kernel names no
                   boot, every relaxed stop without that clean close moves the
                   generation, a kill -9 included. A bare argv runs the
                   build's default; a named tier, strict included, is kept. A
                   build older than this one refuses --durability ordered, so
                   its broker never starts on a job that names it: before
                   going back to one, run `aterm fabric off`, then `aterm
                   fabric on` with that build
  --repair <log>   serve nothing: TRUNCATE <log> at its first CORRUPT record,
                   which the broker refuses to open, and print how many bytes
                   went. The records from that one on are lost — possibly
                   acked mail — and everything before it is kept. A log that
                   opens prints `nothing to repair`; a log a NEWER aterm wrote
                   is refused untouched (run that release). Refused while a
                   broker holds the log

The socket is created world-connectable, so who can connect is bounded by the
DIRECTORY you put it in: use a 0700 one (`aterm fabric on` does). A broker with no
--secret-file and no mint.secret beside its socket checks no capability on attach,
and says `(unguarded)` on stderr. Prints `listening <socket>` once it is bound. It
exits 0 within a minute of its executable being replaced by another build, for its
supervisor (launchd KeepAlive, systemd Restart=always) to start that one. It exits
75 within seconds of its log being POISONED (a full flush that failed with acked
records at risk, or a rollback that could not undo a failed write, after which the
log refuses every write until it is reopened), saying astream's reason on stderr,
so its supervisor's restart reopens the log; run by no `aterm fabric on` job, its
line names the command that starts it again, unless something else restarts it.
While it serves, astream keeps its health beside the log, <log>.health (0600: what
the log holds, who is attached, its flushes, a poison), rewritten every 30 s, at
once when the log is poisoned, and a last time when it stops cleanly: when it
exits on its own, or, at any durability but strict, on SIGTERM, SIGINT or SIGHUP
(a strict broker keeps their default action, so its last snapshot is the last
periodic one); `aterm fabric` and `aterm fabric doctor` read it.

It keeps its log within bounds: the `ev` records for at most 7 days within
256 MiB, and the log is cut from its oldest end as they go. Below the cut it
keeps the latest presence, unread mail for up to 7 days within 64 MiB of its own
(read mail goes with the cut), and everything else for 7 days within 32 MiB.
The trimming runs beside the log (<log>.held keeps what is held) and survives a
crash. A read below what is kept answers `retained-from=` (`inbox get @<off>`
says it). Unread mail given up past its limit is logged here, one line naming
its subject and count; the recipient's inbox reads on past it.
";
    let parsed = match crate::broker_args::BrokerArgs::parse(args) {
        Ok(parsed) => parsed,
        Err(crate::broker_args::ParseError::Help) => {
            return aterm_uds::stdout::answer_exit("aterm link", USAGE, 0);
        }
        Err(crate::broker_args::ParseError::Usage(error)) => {
            eprintln!("aterm link broker: {error}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let secret_file = parsed.secret_file.cloned();
    let durability = parsed.durability.cloned();
    let repair = parsed.repair;
    let words: Vec<&str> = parsed.words.iter().map(|word| word.as_str()).collect();
    // `--repair` serves nothing: one local file, and any other flag is refused.
    if repair {
        if secret_file.is_some() || durability.is_some() {
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
    let usage_error = |why: &str| {
        eprintln!("aterm link broker: {why}");
        eprint!("{USAGE}");
        ExitCode::from(2)
    };
    let durability = match durability
        .as_deref()
        .map(crate::durability::Durability::parse)
        .transpose()
    {
        // A bare argv runs the build's default for a broker its argv names a
        // mint secret for or not ([`crate::durability::default_for`]), judged
        // from the argv's words alone — before the fallback to the secret
        // beside the socket below — as `aterm fabric` and doctor judge them off
        // the running process.
        Ok(d) => crate::durability::resolve(d, secret_file.is_some()),
        Err(e) => return usage_error(&e),
    };
    // A surplus word is refused, never dropped.
    let (endpoint, log) = match words.as_slice() {
        [sock] => ((*sock).to_string(), format!("{sock}.log")),
        [sock, log] => ((*sock).to_string(), (*log).to_string()),
        [] => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        [_, _, extra, ..] => {
            return usage_error(&format!("unexpected argument `{extra}`"));
        }
    };
    // A socket broker without `--secret-file` takes the `mint.secret` beside its socket.
    // Present at all (a symlink, an unreadable file) makes it the secret, so a broken one is
    // refused below, never served unguarded.
    let secret_file = secret_file.or_else(|| {
        let beside = std::path::Path::new(&endpoint).with_file_name(crate::enable::MINT_SECRET);
        std::fs::symlink_metadata(&beside)
            .is_ok()
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
    // Whether this call creates the log, so a refused bind can remove the empty file.
    let log_created_here = !std::path::Path::new(&log).exists();
    // Whether it checks capabilities: what its promise says of a person's own
    // posts ([`crate::durability::describe_served`]).
    let guarded = secret.is_some();
    // The tier, and the boot it is opened in ([`crate::durability::open`]): a
    // legacy relaxed mark from another boot is read into astream's unclean
    // verdict, under the log's lock, whatever this run's tier. The boot is
    // astream's own reading, the one the bridge's incarnation lease trusts
    // ([`crate::durability::this_boot`]).
    let boot = crate::durability::this_boot();
    // A broker of any tier but strict stops cleanly: from here the stop
    // signals wake the loop below instead of killing the process, so its
    // listeners are dropped and astream closes the log cleanly (a full flush,
    // then `Closed` in its sidecar) before it exits. Taken before the open:
    // astream's writer and acceptors are this process's first threads after
    // main, and each inherits the mask. A strict broker's acks are on the
    // disk already, and it keeps the default action.
    let stop = (durability != crate::durability::Durability::Strict)
        .then(|| take_stop_signals(std::thread::current()))
        .flatten();
    // The fault seam ([`crate::broker_health::Seam`]): nothing in a release
    // build. In a dev one, the escalation rules a test adds go into the open,
    // and its `sync` is armed once the log is open and before anything is
    // served, so the first full flush meets it.
    let seam = crate::broker_health::Seam::from_env(std::path::Path::new(&log));
    let opened = crate::durability::open_escalating(
        std::path::Path::new(&log),
        secret,
        durability,
        boot,
        seam.escalation(),
    );
    let broker = match opened {
        Ok(opened) => {
            use std::io::Write as _;
            let mut err = std::io::stderr();
            if let Some(why) = &opened.renewed {
                let _ = writeln!(
                    err,
                    "aterm link broker: {log}: {} — its generation moves, and every bridge and \
                     notifier resumes where it continues",
                    why.describe()
                );
            }
            if let Some(kept) = &opened.mark_kept {
                let _ = writeln!(
                    err,
                    "aterm link broker: could not remove the legacy relaxed mark {kept}; the \
                     log's generation moves once more after the next reboot"
                );
            }
            opened.broker
        }
        Err(e) => {
            eprintln!("aterm link broker: open {log}: {e}");
            // The last line is aterm's remedy, when the refusal has one: the
            // supervisor appends this stderr to `<root>/broker.err`, whose last
            // line `aterm fabric` and doctor quote ([`log_refusal_remedy`]).
            if let Some(remedy) = log_refusal_remedy(e.kind(), &log) {
                eprintln!("aterm link broker: {remedy}");
            }
            return ExitCode::FAILURE;
        }
    };
    seam.arm(&broker, std::path::Path::new(&log));
    // THE DEFAULT RETENTION (session log Phase 6 step 6E-2, ruling U24):
    // astream retains, and its `Hello` advertises, only the rules set since
    // this open, so they are set at every open, before anything is served.
    // A broker whose rules cannot be set (a poisoned log, a held store that
    // cannot be written) still serves, unretained, and says so: its log grows
    // as a broker before R4's did, and no bridge's mirror defaults on over it
    // (it advertises no rule). Exiting instead would take the whole fabric
    // down over its bound.
    let retained = match crate::broker_rules::retain(&broker) {
        Ok(()) => format!(
            "retained by aterm's default rules ({})",
            crate::broker_rules::summary()
        ),
        Err(e) => {
            eprintln!(
                "aterm link broker: {log}: its retention rules could not be set ({e}); it serves \
                 unretained, and no bridge mirrors the session logs onto it by default"
            );
            "unretained".to_string()
        }
    };
    let handle = match broker.serve(&endpoint) {
        Ok(h) => h,
        Err(e) => {
            // A refused bind leaves no empty log behind (see `log_created_here`).
            eprintln!("aterm link broker: serve {endpoint}: {e}");
            if log_created_here
                && std::fs::metadata(&log)
                    .map(|m| m.len() == 0)
                    .unwrap_or(false)
            {
                drop(broker);
                let _ = std::fs::remove_file(&log);
            }
            return ExitCode::FAILURE;
        }
    };
    // The broker's health, in astream's format ([`crate::broker_health`]):
    // astream keeps `<log>.health` from here — written now, every
    // [`crate::broker_health::HEALTH_EVERY`], at once when the log is
    // poisoned, and a last time when the broker stops cleanly (the loop
    // below drops it: an exit of its own, or a stop signal, which a strict
    // broker does not take — killed, it leaves the last periodic one) — and
    // `aterm fabric` and doctor read it with astream's reader. aterm writes
    // no health file of its own. Kept once the socket is served, so a refused
    // bind leaves no snapshot of a broker that never served. One astream
    // cannot write is said here and tried again every period by astream; the
    // broker serves either way, since its health is a report about it, never
    // a condition of it.
    if let Err(e) = broker.keep_health(crate::broker_health::HEALTH_EVERY) {
        eprintln!(
            "aterm link broker: {}: could not write its health ({e}); astream tries again every \
             {} s",
            astream_broker::health_path(std::path::Path::new(&log)).display(),
            crate::broker_health::HEALTH_EVERY.as_secs()
        );
    }
    // A secret that did not read stopped the broker above, so a named one
    // here is the one guarding it.
    let guard_note = match secret_file.as_deref() {
        Some(path) => format!(" (guarded by {path})"),
        None => " (unguarded: it checks no capability)".to_string(),
    };
    // The readiness line a supervisor waits for (`asb serve`'s word).
    //
    // Written, never `println!`: a supervisor may close the pipe after the first
    // line, and `println!` on a closed stdout panics (Rust ignores SIGPIPE).
    {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "listening {endpoint}");
        let _ = out.flush();
        // The tier served is the broker's own word (`Hello`'s tag 0x01, the
        // same answer status and doctor read), never inferred from the flag:
        // astream serves `ordered` strict where the disk cannot promise the
        // order, and the line then says "served strict" and why.
        let _ = writeln!(
            std::io::stderr(),
            "aterm link broker: unix socket on {endpoint}{guard_note}, durability {}: {}; \
             {retained}; log {}",
            durability.name(),
            crate::durability::describe_served(
                durability,
                Some(broker.served_durability()),
                Some(guarded)
            ),
            broker.log_identity()
        );
    }
    // The broker serves on its own threads; this one watches it and its
    // executable and, for any tier but strict, waits for a stop signal.
    // `BrokerHandle` has no `join` — `asb serve` parks the same way — and
    // dropping the handle shuts the broker down and unlinks the socket, which
    // is how it exits when its build is replaced, it is stopped, or its log is
    // poisoned (the doc above).
    //
    // Always a timed park, whatever else is watched: the poison look runs
    // every [`crate::broker_health::POLL`] on every broker, so one whose
    // executable cannot be watched (no image answer) is cured too. The
    // executable is still looked at every [`EXE_WATCH`] (a `stat`), and a stop
    // signal still unparks the loop at once.
    //
    // The look takes no lock ([`astream_broker::Broker::poisoned`]): astream's
    // writer holds the log's lock across a rollback and the log-identity I/O —
    // slow on the failing disk this is for — and publishes the poison only
    // after that I/O, so the look never waits on the device and a poison it
    // sees has its `Lossy` record already written. It stays a timed park
    // rather than astream's `wait_poisoned`, because a stop signal wakes this
    // thread by unparking it, which astream's wait does not hear.
    //
    // Its last snapshot names the poison: the exit below drops the listeners,
    // which stops astream's broker, whose stop writes `<log>.health` a last
    // time and lets it go, so astream's reader calls it the last word of a
    // broker that is gone, never a serving one's. `aterm fabric` reads the
    // snapshot beside the log a serving process names, so while nothing serves
    // it quotes this exit's line from `broker.err` instead, and the restarted
    // broker's first snapshot replaces the file.
    let keep = handle;
    let mut watch = watch;
    let mut exe_looked = std::time::Instant::now();
    let mut poison_said = false;
    loop {
        std::thread::park_timeout(crate::broker_health::POLL);
        let why = if let Some(sig) = stop.as_ref().and_then(StopSignal::taken) {
            format!("stopped by {sig}")
        } else if let Some(reason) = broker.poisoned() {
            use std::io::Write as _;
            if seam.on_poison == crate::broker_health::OnPoison::Stay {
                // The negative control's seam (a dev build's only): said once,
                // and the broker serves on over its poisoned log.
                if !poison_said {
                    let _ = writeln!(
                        std::io::stderr(),
                        "aterm link broker: {log}: {} ({reason}); NOT exiting: {}=stay keeps it \
                         serving",
                        crate::broker_health::POISONED_WORDS,
                        crate::broker_health::FAULT_SEAM
                    );
                    poison_said = true;
                }
                continue;
            }
            // The cure ([`crate::broker_health`]): astream refuses every write
            // until the log is reopened, and only a new open reopens it. The
            // listeners go first, then the broker — astream's close stays
            // silent over a poisoned log and records nothing `Closed`, so the
            // run stays as astream left it (`Lossy` after a failed full flush,
            // which moves the generation at the reopen) — and the line naming
            // astream's reason last, so it is `broker.err`'s last word, the one
            // `aterm fabric` and doctor quote.
            let restart = (!crate::broker_health::supervised()).then(|| {
                let argv: Vec<String> = std::env::args_os()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect();
                crate::fabric::shown_argv(&argv)
            });
            drop(keep);
            drop(broker);
            let _ = writeln!(
                std::io::stderr(),
                "{}",
                crate::broker_health::poisoned_line(&log, &reason, restart.as_deref())
            );
            return ExitCode::from(crate::broker_health::POISONED_EXIT);
        } else {
            let Some(w) = watch.as_mut() else {
                continue;
            };
            if exe_looked.elapsed() < EXE_WATCH {
                continue;
            }
            exe_looked = std::time::Instant::now();
            if !w.poll() {
                continue;
            }
            format!(
                "{} was replaced by another build; exiting so the supervisor starts that one",
                w.path.display()
            )
        };
        use std::io::Write as _;
        let mut err = std::io::stderr();
        let _ = writeln!(err, "aterm link broker: {why}");
        // The clean close: dropping the last listener joins the writer —
        // nothing acks after it — and astream then flushes the log in full and
        // records it `Closed` before it releases the lock, so a reboot after
        // this keeps the identity at any tier. astream says on stderr, just
        // above this line, if that close failed (the run then stays `Open`,
        // and the next open in another boot moves the generation once: the
        // safe direction). Over a poisoned log astream closes nothing and says
        // nothing, so this line says it.
        //
        // Read after the drop, never before: the writer's join drains the
        // batch still queued at the signal, and that batch's failed full flush
        // (or rollback) can poison the log inside the drop — after which
        // astream's close returns silently over it. The broker itself is still
        // alive here (only its listeners went), and poison is sticky, so this
        // one look sees a poison from before the stop and one from its drain.
        drop(keep);
        if let Some(reason) = broker.poisoned() {
            let _ = writeln!(
                err,
                "aterm link broker: {log}: not closed cleanly: its log is {} ({reason}), which \
                 astream does not close, so the next open treats this run as an unclean stop",
                crate::broker_health::POISONED_WORDS
            );
        } else if durability != crate::durability::Durability::Strict {
            let _ = writeln!(
                err,
                "aterm link broker: {log}: closed by astream's clean close — flushed in full and \
                 marked closed, so a reboot keeps its identity, unless astream-broker said \
                 otherwise just above"
            );
        }
        return ExitCode::SUCCESS;
    }
}

/// The stop signals a broker of any tier but strict takes itself ([`take_stop_signals`]):
/// the first one to arrive, once the loop asks.
struct StopSignal(std::sync::Arc<std::sync::atomic::AtomicI32>);

impl StopSignal {
    /// The signal's name, once one has arrived.
    fn taken(&self) -> Option<&'static str> {
        match self.0.load(std::sync::atomic::Ordering::Acquire) {
            0 => None,
            sig => Some(signal_name(sig)),
        }
    }
}

/// `SIGTERM` for 15, as a broker's stderr says it.
fn signal_name(sig: i32) -> &'static str {
    #[cfg(unix)]
    {
        match sig {
            libc::SIGTERM => "SIGTERM",
            libc::SIGINT => "SIGINT",
            libc::SIGHUP => "SIGHUP",
            _ => "a signal",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = sig;
        "a signal"
    }
}

/// Take the stop signals, so a broker of any tier but strict stops cleanly
/// instead of dying with its run still `Open` in the log's sidecar: SIGTERM
/// (launchd at logout or shutdown, `launchctl bootout` under `aterm fabric
/// off`, `systemctl stop`), SIGINT (^C) and SIGHUP (its terminal went away).
/// They are blocked in the calling thread — which must be the process's only
/// one: every thread started after inherits the mask — and one thread of
/// their own takes the first with `sigwait`, records it and unparks `main`,
/// which drops the listeners, so astream closes the log cleanly
/// ([`aterm_uds::process::take_stop_signals`]). `None` when that thread
/// cannot start: the signals keep their default action, the run stays `Open`,
/// and the next open in another boot moves the generation — the safe
/// direction. A second signal while the broker closes is left pending, and a
/// supervisor's escalation to SIGKILL still ends it.
#[cfg(unix)]
fn take_stop_signals(main: std::thread::Thread) -> Option<StopSignal> {
    let taken = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
    let flag = std::sync::Arc::clone(&taken);
    aterm_uds::process::take_stop_signals("broker-stop", move |sig| {
        flag.store(sig, std::sync::atomic::Ordering::Release);
        main.unpark();
    })
    .then_some(StopSignal(taken))
}

/// No POSIX signals here: the broker serves until it is killed.
#[cfg(not(unix))]
fn take_stop_signals(_main: std::thread::Thread) -> Option<StopSignal> {
    None
}

/// The words that open aterm's remedy for a log astream refuses as corrupt —
/// the last line `aterm link broker` writes before it exits, and so the last
/// word of `<root>/broker.err` that `aterm fabric` and doctor quote. The fix
/// they print for it ([`crate::enable::fix_for`]) is chosen on these words,
/// which this binary writes; astream's own refusal names `asb repair`, which
/// aterm does not ship.
pub const CORRUPT_LOG_REMEDY: &str = "the log has a corrupt record and is not opened: run `aterm \
                                      link broker --repair";

/// The words that open aterm's line for a log astream refuses because a newer
/// format wrote it (`Unsupported`): no repair applies — truncating would
/// destroy records the release that wrote them reads — so the remedy is that
/// release. Chosen on by [`crate::enable::fix_for`], like
/// [`CORRUPT_LOG_REMEDY`], and never answered with a repair.
pub const NEWER_LOG_REMEDY: &str = "the log was written by a newer aterm and is not opened";

/// Aterm's remedy for a broker log astream refused to open with `kind`, as one
/// line naming `log` — `None` for a refusal with no remedy of aterm's own (a
/// lock held, a permission), whose astream words say enough.
///
/// By kind, never by astream's wording. In aterm's build (no anti-rollback)
/// `InvalidData` from an open is the corrupt-record refusal, and
/// `Unsupported` a record format newer than this build reads. Since the
/// default retention (session log Phase 6 step 6E-2) the log also has the
/// retention's sidecars (`<log>.held`, `.state`, `.base`), and one that does
/// not read is refused `InvalidData` too, astream's words naming the file: the
/// repair named here then opens the log the same way and touches nothing, so
/// the remedy costs nothing where it does not apply.
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
/// the log truncated at its first corrupt record, the bytes dropped counted.
/// Idempotent: a log that opens is left as it is and says `nothing to repair`.
///
/// A log that is not there is refused, not created: `open_repair` would leave
/// an empty log behind a typo. A broker holding the log (astream's exclusive
/// lock, `AlreadyExists`) is refused — repair it between its supervisor's
/// restarts, or with the job stopped. A newer-format log is refused untouched
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
    // Written, never `println!`: a closed stdout must not panic the repair.
    let _ = writeln!(std::io::stdout().lock(), "{said}");
    ExitCode::SUCCESS
}

/// How often a serving broker looks at its executable: slow, because a
/// replaced build can wait half a minute and the look is one `stat`.
const EXE_WATCH: std::time::Duration = std::time::Duration::from_secs(30);

/// A file's identity (device, inode, length, mtime), so a replaced executable (a new inode
/// renamed over the path, or the same inode rewritten) is told from the one a process started
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

impl FileId {
    /// The regular file at `path`, symlinks followed: what a supervisor that runs `path` starts.
    /// Errors on a failed `stat` or a path that is not a regular file.
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

/// The running image, not a pathname an update may already have replaced: on Linux
/// `/proc/self/exe`'s metadata; elsewhere `crate::fabric::running_image_inode`, which keeps the
/// original inode when `current_exe` names another file.
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
                // No OS image answer: the early file snapshot still catches a later swap.
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

/// Whether the broker's executable has been replaced, so it should exit for its supervisor to
/// start the new build.
///
/// `start` is the running image, `seen` the previous look's file, `now` this look's. Replaced
/// means a different file that is settled (the same on two looks running), so a build still
/// being written is never the one started. A path that is gone or not a regular file (mid-swap,
/// an uninstall) is not a replacement: exiting into a supervisor with nothing to start is an
/// outage, so the broker keeps serving and looks again.
fn replaced(start: &RunningImage, seen: Option<&FileId>, now: &std::io::Result<FileId>) -> bool {
    matches!(now, Ok(now) if !start.matches(now) && seen == Some(now))
}

/// `aterm link mint <grant> --secret-file <path>` — print one capability line.
///
/// A cap file is `<grant> <tag-hex>` lines, and `serve --cap-file` reads them.
///
/// The secret is a file, never an argument: an argv is world-readable in `ps`, and this is the
/// key that mints every capability on the fleet.
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
            "-h" | "--help" => return aterm_uds::stdout::answer_exit("aterm link", USAGE, 0),
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
    // The broker's own reader: a group-readable or short secret is refused here as there.
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
            minted_cap_to(
                &mut std::io::stdout().lock(),
                &astream_cap::capfile::format_line(&cap.filter, &cap.tag),
            )
        }
        Err(e) => {
            eprintln!("aterm link mint: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Print a minted capability only through the checked complete-result writer.
fn minted_cap_to(out: &mut dyn std::io::Write, line: &str) -> ExitCode {
    match crate::person::write_capability_to(out, &format!("{line}\n")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "aterm link mint: the capability was not written whole ({error}); mint again"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_cap_output_requires_a_complete_write_and_flush() {
        let line = astream_cap::capfile::format_line("n-synthetic:r:/f/test", &[42; 32]);
        let mut ordinary = Vec::new();
        assert_eq!(minted_cap_to(&mut ordinary, &line), ExitCode::SUCCESS);
        assert_eq!(ordinary, format!("{line}\n").as_bytes());
        struct Refuses {
            bytes: Vec<u8>,
            accept: usize,
            kind: std::io::ErrorKind,
            flush: bool,
            zero: bool,
        }
        impl std::io::Write for Refuses {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.zero {
                    return Ok(0);
                }
                let n = bytes
                    .len()
                    .min(self.accept.saturating_sub(self.bytes.len()));
                if n == 0 {
                    return Err(self.kind.into());
                }
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                if self.flush {
                    Err(self.kind.into())
                } else {
                    Ok(())
                }
            }
        }
        let mut admitted = Vec::new();
        for (accept, kind, flush, zero) in [
            (0, std::io::ErrorKind::StorageFull, false, false),
            (3, std::io::ErrorKind::StorageFull, false, false),
            (0, std::io::ErrorKind::BrokenPipe, false, false),
            (usize::MAX, std::io::ErrorKind::StorageFull, true, false),
            (0, std::io::ErrorKind::WriteZero, false, true),
        ] {
            let mut out = Refuses {
                bytes: Vec::new(),
                accept,
                kind,
                flush,
                zero,
            };
            admitted.push(minted_cap_to(&mut out, &line) == ExitCode::FAILURE);
        }
        assert_eq!(
            admitted,
            vec![true; 5],
            "no partial, lost or unflushed capability result is success"
        );
    }

    fn argv(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    /// A replacement is a different file, settled over two looks (renamed over the path or
    /// rewritten in place); the same file, or a gone or non-regular path, never is.
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
        // Gone, or a directory in its place: never.
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

        // An update lands before readiness: the watch still compares with the captured image.
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

        // A path replaced before the watch is built is compared with the OS's mapped image.
        let mut already_replaced = ExeWatch {
            path: invocation,
            start: RunningImage::capture().expect("this process's mapped executable"),
            seen: None,
        };
        assert!(!already_replaced.poll());
        assert!(already_replaced.poll());
    }

    /// **`ls` PRINTS §7's ROW.** The columns with no writer are asserted
    /// PRESENT and `-` ("unknown"), and no body moves an operator's cursor.
    #[test]
    fn ls_prints_section_7s_columns_one_safe_token_each() {
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

        // No escalation in all three spellings: absent, empty, and `-`.
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

        // A hostile body still prints one token of printable ASCII per column.
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
        // The cut never lands inside a `%XX` escape.
        assert!(
            crate::pct::decode(printed).ends_with('…'),
            "the cut landed inside a %XX escape: {printed}"
        );
    }

    /// `--token-file` refuses a symlink rather than following it.
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
