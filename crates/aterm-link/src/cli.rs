// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-link` — the fabric bridge's command line.
//!
//! A3 implements two of the subcommands §11.2 lists:
//!
//! ```text
//! aterm-link serve --fleet <F> --broker <ep> [--tcp] [--key-file PATH]
//!                  --cap-file <path>… [--state DIR] [--accept-from <p>,…]
//!                  [--sock <path> --token-file <path>]
//! aterm-link ls    --fleet <F> --broker <ep> [--tcp] [--key-file PATH]
//!                  --cap-file <path>… [--attention]
//! ```
//!
//! An unknown word is refused BY NAME rather than answered with the usage text:
//! a typo that prints help reads as a request for help rather than as the
//! mistake it was. Three names — `wake`, `pin` and `lash` — used to have an arm
//! of their own saying they were "a later rung"; round 21 removed it, and they
//! are now unknown subcommands like any other. Both answers are exit 2; what
//! changed is that this file no longer promises a rung nobody is building.
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
//!   so every row shows `-` today. (`role=` and `detail=` HAVE a writer since
//!   round 13 — the bridge's presence sampler, [`crate::presence`] — on a
//!   bridge running with `presence = "meta"`, the default; a `minimal` bridge
//!   and one that predates round 13 still show `-`.)
//! * `host=` and `fabric=` are published on the NODE's own presence row only
//!   (`bridge.rs`'s `bring_presence_up`), deliberately: `fabric=` on a session
//!   row was a constant nothing could falsify, and the bridge's own header says
//!   why it was removed. A session row therefore shows `-` for both, and the
//!   node row above it carries the real answer.
//!
//! SIX COLUMNS ARE ADDED to §7's set, at the end, because each has a real
//! writer and a real reader: `gen=` (§6.6's `<content_seq>:<fp16>`, what a
//! `gen=`-bound approval is minted against), `observer=` (§11.2's watching,
//! non-hosting attachment — a row an operator must not read as a host),
//! `epoch=` (the launch nonce a drive record is fenced on), and round 13's
//! `phase=` (busy | idle | prompt | question | limited | survey — the word
//! `aterm drive phase` prints, from the same reader), `context=` (`<n>%` when
//! Claude Code shows its indicator) and `title=` (the session's `meta set
//! title` user title, else `-`; NEVER the terminal's title, which the program
//! writes — [`crate::presence`]; 128 bytes). Dropping them to match §7
//! exactly would lose the only face they have.
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
/// write into a bridge command ([`crate::enable`]'s `binary_has_sealed`).
const USAGE: &str = concat!(
    "\
aterm-link — the aterm fabric bridge

  aterm-link serve --fleet <F> --broker <ep> --cap-file <path>... [options]
  aterm-link ls    --fleet <F> --broker <ep> --cap-file <path>... [--attention]
  aterm-link hook  install claude [--merge] [--dry-run] [--rewake] [--report-to @sid] | run <event> [--check]   (`hook` for its own usage)
  aterm-link notify --on <attention|ask:<p>|halt>,... --exec <cmd>  (`notify` for its own usage)
  aterm-link mirror <root> --sock <path>            (the file plane; `mirror` for its own usage)
  aterm-link broker <socket> [log] [--secret-file <path>]
                    the local bus itself (`broker` for its usage)
  aterm-link broker --tcp <host:port> --key-file <k> --secret-file <s> <log>
                    the bus on the SEALED TCP wire, always guarded (a `sealed` build)
  aterm-link mint   <grant> --secret-file <path>    one capability line for --cap-file
  aterm-link fabric [status [--json] | tail [--bodies] [--from <offset>]
                    | on [--dry-run] [--service ...] [--tcp <bind> --key-file <k>]
                    | off [--dry-run] | doctor | mint-for <node-id>|new [--out <cap>]
                    | join --broker <host:port> --tcp --key-file <k> --cap-file <c>]
                    the fabric's state on one screen, read from aterm's [fabric] command,
                    the one command that turns it on and proves it, and the two that
                    bring a SECOND HOST into the fleet over the sealed wire
                    (`aterm fabric` is the same code; `fabric help` for its usage)

`ls` and `mirror` default their flags from the rendezvous file
`aterm fabric on` (or `join`) writes beside the instance control sockets (fabric.toml):
--fleet, --broker, --cap-file and --state for the first three — and --tcp --key-file
with a defaulted --broker on a host that joined over the sealed wire — --sock for
mirror. A flag given on the command line wins.

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
                         meta (the default) adds role= detail= phase= context=
                         title= beside attention=. phase= is Claude Code's turn
                         phase read off the last 40 rows of the screen, only when
                         the session's status revision moved; a session running
                         anything else reads idle (its detail= says what runs).
                         title= is `meta set title` alone — never the terminal's
                         title, which a program writes (Claude Code puts a
                         summary of the conversation there) — and never any
                         transcript text. minimal is attention= alone and never
                         reads a screen. Without the flag, `[fabric] presence`
                         in aterm.toml, else meta.
  --receipts             `serve` only: publish a RECEIPT (R8) when one of this
                         node's sessions runs `inbox seen <id> handled|refused|
                         deferred` on an ask or task — `kind=ack re=<off>
                         verdict=<v>` onto the SENDER's inbox lane, so their
                         `post --wait-ack` returns, their `await inbox re=<off>`
                         latches and their `inbox` row reads `ack … verdict=`.
                         A note earns none; a session that only --peeks acks
                         nothing. ON BY DEFAULT, and that default is the same
                         one whichever way the bridge was set up (round 21; it
                         used to be off here and on for a node `aterm fabric on`
                         had configured). `--no-receipts` turns it off for one
                         run, `[fabric] receipts = false` in aterm.toml for
                         good; with neither, that key decides and its absence
                         means on. Off costs the SENDER: nothing can release a
                         `post --wait-ack`, so every `ask` waits out its whole
                         deadline.
  --attention            `ls` only: keep only rows carrying an `attention=` (§9.3)

`ls` prints §7's row — <node> <host> <sid> state= inc= role= detail= driving=
holder= hold= fabric= attention= — then gen=, observer=, epoch=, phase=,
context= and title=. `role=`, `detail=`, `phase=`, `context=` and `title=` are
written by a round-13 bridge in `meta` mode (a `minimal` or older bridge leaves
them `-`); every column is printed pct-encoded, so `context=12%` reads
`context=12%25`; `driving=` has no writer in this fabric yet and always reads
`-`, and since round 21 cut the drive face `holder=` has none either on a row
this node wrote — it is still PRINTED because the roster is a pass-through and
an older node on the wire still publishes one;
`host=` and `fabric=` are on the NODE row only, so a session row reads `-` for
both and the node's own row above it carries them.

`--tcp` alone is PLAINTEXT and for a trusted network only; `--tcp --key-file` is
astream's XChaCha20-Poly1305 sealed wire, which is what a cross-host fleet uses —
compiled only with the `sealed` cargo feature (off by default, and off in the
shipped `aterm` binary): a default build parses the flags and then refuses with
`Unsupported`, naming the rebuild. The line below says which this build is.
astream also builds an ephemeral-X25519 (`handshake`) and a mutual signed-DH
(`identity`) transport; neither is offered here, because each would add a
third-party crypto dependency to a crate whose dependency set the design pins,
and `aterm link broker` serves no `identity` listener to reach.


",
    build_line!()
);

/// The phrase [`USAGE`] carries in a `sealed` build — what a caller checks a
/// binary's `link` output for before writing a sealed bridge command.
pub const SEALED_BUILD_MARK: &str = "sealed TCP transport compiled in";

/// The bridge's whole command line, as a LIBRARY entry.
///
/// It lives here and not behind a `main` so the ONE `aterm` binary can carry
/// it: this repository ships a single Mach-O with argv0 symlinks beside it
/// (`expose = ["aterm"]`, `bundle = []`), so a second executable would simply
/// never reach a user. `aterm link ...` and the `aterm-link` symlink both land
/// here; `src/main.rs` is a shim for a direct `cargo run`.
#[must_use]
pub fn dispatch(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        Some("serve") => run(&args[1..], true),
        // THE READ-SIDE VERBS DEFAULT THEIR FLAGS from the rendezvous file
        // (`crate::enable`); `serve` does not, because aterm launches it with
        // every flag spelled out and a bridge that silently took a different
        // broker than its config names would be the report lying about itself.
        Some("ls") => run(&crate::enable::with_rendezvous_defaults(&args[1..]), false),
        Some("notify") => crate::notify::main(&args[1..]),
        Some("hook") => crate::hook::main(&args[1..]),
        Some("mirror") => crate::mirror::main(&crate::enable::with_default_sock(&args[1..])),
        Some("broker") => broker(&args[1..]),
        Some("mint") => mint(&args[1..]),
        Some("fabric") => crate::fabric::main(&args[1..]),
        // Asked for: stdout, exit 0 — the same contract `aterm --help` and the
        // `broker -h` / `mint -h` children keep. Before this the front door
        // answered its own `--help` with exit 2 on stderr, disagreeing with
        // every one of its children.
        Some("-h" | "--help" | "help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        // Refused BY NAME, the rule this file's header states. An unknown word
        // used to print the usage indistinguishably from `--help`, so a typo
        // read as a request for help rather than as the mistake it was.
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

fn run(args: &[String], serve: bool) -> ExitCode {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("aterm-link: {e}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if serve {
        // `--attention` IS AN `ls` FILTER, and `serve` refuses it BY NAME rather
        // than ignoring it: a flag that parsed and did nothing is the failure
        // this crate refuses everywhere else (`--handshake`, and every unknown
        // subcommand).
        if parsed.attention {
            eprintln!("aterm-link: --attention is an `ls` filter (§9.3); `serve` has no roster");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        let mut cfg = parsed.cfg;
        // THE ONE FLAG `serve` DOES DEFAULT FROM A FILE: `[fabric] presence`
        // is a knob on what the bridge publishes, not on which broker it
        // reaches, so reading it from aterm.toml cannot make the report lie
        // about the bus — and it is where the design says the knob lives.
        if !parsed.presence_given {
            cfg.presence = crate::fabric::presence_from_config();
        }
        // AND `[fabric] receipts`, by the same argument: a knob on what the
        // bridge publishes about its own sessions' decisions, not on which
        // broker it reaches.
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
    // AND IT IS BOUNDED, by the SAME number the other two readers of a presence
    // token use. `presence.rs`'s `token` caps what the bridge publishes at
    // [`crate::glance::ATTENTION_CAP`] and `glance.rs` caps what it renders
    // at the same constant; a roster that printed the token whole would be a
    // third reader of one field with a bound of its own — which is the shape of
    // defect this crate keeps re-finding. A row is written by another host and
    // nothing on the bus obliges it to have used the bridge's own writer.
    //
    // The truncation is on an ESCAPE boundary, never inside a `%XX`, which is
    // the rule `presence::token` already applies; the mark is `pct::encode("…")`,
    // the same ellipsis `glance::truncate` appends, kept ASCII so the row is
    // still one printable token per column.
    if safe.len() > crate::glance::ATTENTION_CAP {
        // THE MARK IS INSIDE THE CAP: a reader sizing anything from
        // `ATTENTION_CAP` must never be handed `cap + <the mark>`, which is the
        // whole reason to have one number rather than one number and a bit.
        safe.truncate(crate::glance::ATTENTION_CAP - ELLIPSIS.len());
        while safe.ends_with('%') || (safe.len() >= 2 && safe.as_bytes()[safe.len() - 2] == b'%') {
            safe.pop();
        }
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
/// IT PAGES ON THE RESUME CURSOR, through the crate's one walk
/// ([`transport::walk_last`]), and it did not always: it used to call
/// `Client::last` — which drops the cursor before its caller can see it — and
/// stop on an EMPTY PAGE, which the broker's own contract says is not the end of
/// the answer. A short roster is indistinguishable from a small fleet, and
/// `--attention` printing nothing is indistinguishable from nobody escalating,
/// so a walk that could not FINISH is an error here and not an absence.
///
/// THE ROW IS §7's, in §7's order, with §7's names — see the module header for
/// which of its columns have no writer in this fabric yet (they print `-`) and
/// for the three that are appended to it. `attention_only` is §9.3's
/// `--attention`: the cross-host escalation view, which is the same one `Last`
/// and a filter on the field the notifier fires on (§4.2, A10).
///
/// A row that carries no `attention=`, an empty one, or the `-` an absent field
/// renders as, is not an escalation — the same three-way test
/// `notify::Sel::Attention` makes, so the two faces of the same question cannot
/// disagree.
fn roster(cfg: &Config, attention_only: bool) -> std::io::Result<()> {
    let (mut client, _closer) = transport::connect(&cfg.transport, &cfg.broker)?;
    for path in &cfg.cap_files {
        for cap in crate::bridge::read_cap_file(path)? {
            client.attach(&cap.grant, &cap.tag)?;
        }
    }
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

/// One roster line, and whether it is an ESCALATION.
///
/// Split out of [`roster`] so the column set §7 pins is checkable without a
/// broker: the order and the names of the columns are the claim, and a claim
/// this crate makes in a doc comment is the only evidence it has (aterm keeps no
/// manifest).
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
        // ROUND 13'S THREE: the phase word, the context percentage and the
        // title, written by a `meta`-mode bridge; `-` from any other.
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
        // `aterm fabric on` writes — one default, whichever door you came
        // through (round 21). This value only ever survives for a caller that
        // builds a `Cfg` without going through the config fallback below.
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
            // REFUSED BY NAME, with the reason. astream builds both; §11.2 pins
            // this crate's third-party set to what `cap` + `aead` already
            // isolate, and each of these adds another vetted-but-new crypto
            // dependency to it. A flag that parsed and silently fell back to the
            // PSK would be worse than one that says it does not exist.
            other @ ("--handshake" | "--identity" | "--identity-file" | "--host-key-file") => {
                return Err(format!(
                    "{other}: aterm-link speaks the sealed PSK wire only (--tcp --key-file); \
                     astream's forward-secret and identity transports would widen this \
                     crate's pinned dependency set (DESIGN-aterm-fabric.md §11.2)"
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
    // A KEY WITHOUT `--tcp` IS A MISTAKE, NOT A DEFAULT. Quietly ignoring it
    // would run the fleet's traffic over a plain socket while the operator
    // believed it was sealed, and quietly turning `--tcp` on would try to
    // connect to a socket path as a host:port. Both are refused.
    cfg.transport = match (tcp, key_file) {
        (false, None) => Transport::Unix,
        (true, None) => {
            eprintln!(
                "aterm-link: --tcp without --key-file is PLAINTEXT: every keystroke and \
                 every message crosses the network in the clear. Trusted networks only."
            );
            Transport::Tcp
        }
        (true, Some(path)) => Transport::Sealed(Box::new(
            transport::read_key_file(&path).map_err(|e| format!("--key-file {path}: {e}"))?,
        )),
        (false, Some(_)) => {
            return Err("--key-file needs --tcp (the sealed wire is a TCP transport)".to_string());
        }
    };
    if !crate::subject::is_fleet(&cfg.fleet) {
        return Err(format!(
            "--fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            cfg.fleet
        ));
    }
    if cfg.state_dir.is_empty() {
        cfg.state_dir = default_state_dir();
    }
    if let Some(path) = token_file {
        cfg.token = Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("--token-file {path}: {e}"))?
                .trim()
                .to_string(),
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

/// `$XDG_STATE_HOME/aterm-link`, else `$HOME/.local/state/aterm-link`, else the
/// working directory — never a temp dir, because losing the node id loses the
/// node's mail lane.
fn default_state_dir() -> String {
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
/// The fabric needs three things on a machine: a broker, a bridge, and aterm.
/// Two of them now ride the one binary; without this the third was `asb`, a
/// SEPARATE executable from the vendored `astream-broker` crate, and
/// `bundle = []` means aterm ships exactly one Mach-O — so an operator had to
/// build another repository's CLI by hand to turn on a feature the shipped
/// binary otherwise supports end to end.
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
///   HOST's broker (round 16): astream's sealed record layer
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
                return usage_error(
                    "--key-file needs --tcp (the sealed wire is a TCP listener; a socket's \
                     boundary is its directory)",
                );
            }
            if allow_remote {
                return usage_error("--allow-remote needs --tcp (a Unix socket is never remote)");
            }
            if unix.is_some() {
                return usage_error(
                    "--unix needs --tcp: without it the socket IS the listener (`broker \
                     <socket> [log]`)",
                );
            }
            // A surplus word is refused, never dropped: `broker d/b.sock d/b.log
            // --guarded` used to run happily with `--guarded` vanished, which is
            // exactly the "parses and does nothing" failure this file's header
            // names.
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
                return usage_error(
                    "--tcp needs --key-file: this verb serves TCP only SEALED — plaintext TCP \
                     would carry every keystroke and message in the clear",
                );
            };
            if secret_file.is_none() {
                return usage_error(
                    "--tcp needs --secret-file: a TCP broker is always GUARDED — the \
                     pre-shared key is one secret every host holds, so only the capability a \
                     node attaches (minted under this secret) says which node may publish what",
                );
            }
            let log = match words.as_slice() {
                [log] => (*log).to_string(),
                [] => return usage_error("--tcp needs <log>: the durable record log's path"),
                [_, extra, ..] => {
                    return usage_error(&format!("unexpected argument `{extra}`"));
                }
            };
            match transport::is_loopback_endpoint(bind) {
                Ok(true) => {}
                Ok(false) if allow_remote => {}
                Ok(false) => {
                    eprintln!(
                        "aterm link broker: --tcp {bind} is not a loopback address, and a broker \
                         bound there is reachable from the network. The sealed wire keeps out a \
                         peer WITHOUT the key — but the key is one pre-shared secret every host \
                         of the fleet holds: it is a transport boundary, not a per-host identity, \
                         and there is no revoking one host short of re-keying all of them. Pass \
                         --allow-remote to say that is what you mean (and open one TCP port to \
                         the hosts that join, no wider)."
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
                eprintln!("aterm link broker: --key-file {key_path}: {e}");
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
    // Whether THIS call is the one creating the log: `open` creates it, and a
    // bind refused afterwards (`a broker is already listening on this socket`)
    // used to leave an empty `b2.log` behind every typo'd retry — stray files
    // that later read as real bus records.
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
    // The line a supervisor waits for. `asb serve` prints the same word, so a
    // launchd/systemd readiness probe written against either one works. On
    // TCP it is the BOUND address, so a port-0 bind names the port it got.
    let shown = handle.tcp_addr().map_or(endpoint, str::to_string);
    println!("listening {shown}");
    if let Some(sock) = unix.as_deref() {
        println!("listening {sock}");
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    eprintln!(
        "aterm link broker: {} on {shown}{}{}",
        if tcp.is_some() {
            "sealed TCP"
        } else {
            "unix socket"
        },
        unix.as_deref()
            .map_or_else(String::new, |sock| format!(" and the unix socket {sock}")),
        if guarded {
            " — GUARDED: an attach must carry a capability minted under the secret file"
        } else {
            " — unguarded: no capability is checked on attach"
        }
    );
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
    transport::check_private(path).map_err(|e| format!("--secret-file {path}: {e}"))?;
    let secret = std::fs::read(path).map_err(|e| format!("--secret-file {path}: {e}"))?;
    if secret.len() < crate::enable::SECRET_LEN {
        return Err(format!(
            "--secret-file {path} holds {} bytes; a mint secret is at least {} (a short key \
             guards nothing)",
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
/// The third thing an operator needs and the one binary did not carry. A cap
/// file is `<grant> <tag-hex>` lines, and `serve --cap-file` reads them; without
/// a mint there was no way to produce one except `asb`, a separate executable
/// from a repository aterm now vendors but does not ship.
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
                // Once. A repeated flag used to win last silently, so
                // `--secret-file good --secret-file empty` minted with the empty
                // key and exited 0.
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
    // A SHORT SECRET IS REFUSED, not silently used. HMAC accepts a key of any
    // length, so an empty or truncated file mints a capability that looks
    // perfectly well-formed and seals nothing — the failure mode is a fleet that
    // appears configured and is not. 32 bytes is what a mint secret is here (see
    // tools/fabric-enable.sh, which writes exactly that from /dev/urandom).
    const SECRET_MIN: usize = 32;
    if key.len() < SECRET_MIN {
        eprintln!(
            "aterm link mint: {secret_path} holds {} bytes; a mint secret is at least \
             {SECRET_MIN} (a short key mints a capability that seals nothing)",
            key.len()
        );
        return ExitCode::FAILURE;
    }
    match astream_cap::mint(&key, grant) {
        Ok(cap) => {
            // `<grant> <tag-hex>`, split at the LAST whitespace by every reader,
            // so a grant whose filter holds a space still reads back.
            let tag: String = cap.tag.iter().map(|b| format!("{b:02x}")).collect();
            println!("{} {tag}", cap.filter);
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

    /// **`ls` PRINTS §7's ROW, AND `--attention` IS §9.3's VIEW.**
    ///
    /// §7 pins the columns and their order; §9.3 names `aterm-link ls
    /// --attention` as the way a human finds an escalation on a headless box.
    /// Before this, `roster` printed a set of its own (`<node> <owner> state=
    /// inc= hold= holder= gen= observer= epoch=` — no `host`, `role`, `detail`,
    /// `driving`, `fabric` or `attention` at all) and `parse` answered a bare
    /// "unknown flag --attention", so the one face the design names for an
    /// escalation refused to run and the fallback had no column to read.
    ///
    /// The columns with no writer are asserted to be PRESENT and `-`: that is
    /// the honest rendering of "unknown", and it is what stops the next reader
    /// from concluding the design's column set was silently abandoned.
    #[test]
    fn ls_prints_section_7s_columns_and_attention_is_a_flag_not_an_error() {
        // §9.3'S FLAG PARSES, on `ls`, and is refused by name on `serve` —
        // `run` does that, so here we assert `parse` no longer rejects it.
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
        // The columns this row's writer did not fill read `-`, which is
        // "unknown" and not a promise — `driving=` and `fabric=` have no
        // session-row writer at all; `role=`, `detail=`, `phase=`, `context=`
        // and `title=` have one since round 13, and this body (an older or
        // `minimal` bridge's) carries none of them.
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
        // A ROUND-13 ROW: the five are printed as written, in their columns.
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

        // AND EVERY COLUMN IS BOUNDED BY THE SAME NUMBER the bridge and glance
        // use. A rogue node's presence row is not obliged to have gone through
        // this crate's own writer, so `ls` cannot inherit its cap.
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
}
