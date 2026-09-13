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
//! The rest (`wake`, `pin`, `lash`) belong to later
//! rungs and are refused by name rather than silently accepted: a subcommand
//! that parses and does nothing is worse than one that says it does not exist.
//!
//! ## `ls` PRINTS §7'S COLUMNS, AND SAYS WHICH OF THEM HAVE NO WRITER
//!
//! §7 pins the row as `<node> <host> <sid> state= inc= role= detail= driving=
//! holder= hold= fabric= attention=`, and §9.3 sells `ls --attention` as the
//! human's cross-host escalation view. Both are printed here, in that order, and
//! `--attention` keeps only the rows carrying one.
//!
//! FOUR OF THOSE COLUMNS HAVE NO WRITER IN THIS FABRIC YET, and they print `-`
//! rather than being dropped, because `-` is the truth ("unknown") and a missing
//! column is a reader silently disagreeing with the design:
//!
//! * `role=`, `detail=` and `driving=` are in §4.2's presence body but
//!   [`crate::bridge::Bridge::publish_session_presence`] does not publish
//!   them, so every row shows `-` today.
//! * `host=` and `fabric=` are published on the NODE's own presence row only
//!   (`bridge.rs`'s `bring_presence_up`), deliberately: `fabric=` on a session
//!   row was a constant nothing could falsify, and the bridge's own header says
//!   why it was removed. A session row therefore shows `-` for both, and the
//!   node row above it carries the real answer.
//!
//! THREE COLUMNS ARE ADDED to §7's set, at the end, because each has a real
//! writer and a real reader: `gen=` (§6.6's `<content_seq>:<fp16>`, what a
//! `gen=`-bound approval is minted against), `observer=` (§11.2's watching,
//! non-hosting attachment — a row an operator must not read as a host), and
//! `epoch=` (the launch nonce a drive record is fenced on). Dropping them to
//! match §7 exactly would lose the only face they have.
//!
//! EVERY PRINTED FIELD IS UNTRUSTED. A presence body is written by whatever node
//! holds `rw,p=<n>:/f/<F>/pub/<n>/>`, and `attention=` is free text a session
//! chose. Each column is normalised through [`crate::pct`] before it is
//! printed, so a row is always one whitespace-delimited token per column of
//! printable ASCII and no body can move an operator's cursor.

use std::process::ExitCode;

use crate::bridge::{Bridge, Config};
use crate::transport::{self, Transport};

/// Usage, printed to stderr on a usage error (exit 2, aterm's convention).
const USAGE: &str = "\
aterm-link — the aterm fabric bridge

  aterm-link serve --fleet <F> --broker <ep> --cap-file <path>... [options]
  aterm-link ls    --fleet <F> --broker <ep> --cap-file <path>... [--attention]
  aterm-link hook  install claude [--rewake] | run <event>   (`hook` for its own usage)
  aterm-link notify --on <attention|ask:<p>|halt>,... --exec <cmd>  (`notify` for its own usage)
  aterm-link mirror <root> --sock <path>            (the file plane; `mirror` for its own usage)
  aterm-link glance --fleet <F> --broker <ep> --cap-file <path>...   (writes <state>/fabric/glance.json)
  aterm-link tui    --fleet <F> --broker <ep> --cap-file <path>... [--from <offset>]\n\
  aterm-link broker <socket> [log]                  the local bus itself (`broker` for its usage)\n\
  aterm-link mint   <grant> --secret-file <path>    one capability line for --cap-file

  --fleet <F>            the fleet name (the `/f/<F>/` subtree)
  --broker <ep>          the broker: a Unix socket path, or <host>:<port> with --tcp
  --tcp                  reach the broker over TCP instead of a Unix socket
  --key-file <path>      64 hex chars: the sealed transport's pre-shared key (needs --tcp)
  --cap-file <path>      a minted capability (`<grant> <tag-hex>` lines); repeatable
  --state <dir>          the state dir (node id, incarnation, sequence, watermarks)
  --accept-from <p>,...  principals whose task/control arrive undemoted
  --screen <sid|all>,... publish these sessions' screens on the bus (OFF by default)
  --sock <path>          hand-started OBSERVER mode against a control socket
  --token-file <path>    the instance token, for --sock
  --attention            `ls` only: keep only rows carrying an `attention=` (§9.3)

`ls` prints §7's row — <node> <host> <sid> state= inc= role= detail= driving=
holder= hold= fabric= attention= — then gen=, observer= and epoch=. `role=`,
`detail=` and `driving=` have no writer in this fabric yet and always read `-`;
`host=` and `fabric=` are on the NODE row only, so a session row reads `-` for
both and the node's own row above it carries them.

`--tcp` alone is PLAINTEXT and for a trusted network only; `--tcp --key-file` is
astream's XChaCha20-Poly1305 sealed wire, which is what a cross-host fleet uses —
compiled only with the `sealed` cargo feature (off by default, and off in the
shipped `aterm` binary): a default build parses the flags and then refuses with
`Unsupported`, naming the rebuild.
astream also builds an ephemeral-X25519 (`handshake`) and a mutual signed-DH
(`identity`) transport; neither is offered here, because each would add a
third-party crypto dependency to a crate whose dependency set the design pins,
and `asb` serves no `identity` listener to reach.

Later rungs own `wake`, `pin` and `lash`.
";

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
        Some("ls") => run(&args[1..], false),
        Some("notify") => crate::notify::main(&args[1..]),
        Some("hook") => crate::hook::main(&args[1..]),
        Some("mirror") => crate::mirror::main(&args[1..]),
        Some("glance") => crate::glance::main(&args[1..]),
        Some("tui") => crate::tui::main(&args[1..]),
        Some("broker") => broker(&args[1..]),
        Some("mint") => mint(&args[1..]),
        Some(other @ ("wake" | "pin" | "lash")) => {
            eprintln!("aterm-link: `{other}` is a later rung and is not implemented yet");
            ExitCode::from(2)
        }
        _ => {
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
        // this crate refuses everywhere else (`--handshake`, `wake`).
        if parsed.attention {
            eprintln!("aterm-link: --attention is an `ls` filter (§9.3); `serve` has no roster");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
        match Bridge::new(parsed.cfg).and_then(Bridge::run) {
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
struct Parsed {
    cfg: Config,
    /// `ls --attention` (§9.3): keep only the rows carrying an escalation.
    attention: bool,
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
    // token use. `bridge.rs`'s `attention_of` caps what it publishes at
    // [`crate::glance::ATTENTION_CAP`] and `glance.rs` caps what it renders
    // at the same constant; a roster that printed the token whole would be a
    // third reader of one field with a bound of its own — which is the shape of
    // defect this crate keeps re-finding. A row is written by another host and
    // nothing on the bus obliges it to have used the bridge's own writer.
    //
    // The truncation is on an ESCAPE boundary, never inside a `%XX`, which is
    // the rule `attention_of` already applies; the mark is `pct::encode("…")`,
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
         holder={} hold={} fabric={} attention={attention} gen={} observer={} epoch={}",
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
    );
    (line, escalating)
}

/// Parse argv. Every flag takes a value; a flag without one is a usage error
/// rather than a silently defaulted setting.
fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut attention = false;
    let mut cfg = Config {
        fleet: String::new(),
        broker: String::new(),
        transport: Transport::Unix,
        cap_files: Vec::new(),
        state_dir: String::new(),
        accept_from: Vec::new(),
        screen: Vec::new(),
        sock: None,
        token: None,
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
            "--screen" => cfg.screen.extend(value()?.split(',').map(str::to_string)),
            "--attention" => attention = true,
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
    // `--screen` NAMES SESSIONS, and `all` is spelled out rather than implied by
    // a bare flag: this face carries screen CONTENT onto an append-forever log
    // (§10) that has no encrypt-at-rest yet (T12), so "every screen on this node"
    // is a sentence an operator should have to write.
    for s in &cfg.screen {
        let bare = s.trim_start_matches('@');
        if s != "all" && !(crate::subject::is_principal(bare) && bare.starts_with("s-")) {
            return Err(format!("--screen {s} is not `all` or an @s-<sid>"));
        }
    }
    Ok(Parsed { cfg, attention })
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
/// binary otherwise supports end to end. Thirty lines here close that.
///
/// It is deliberately the PLAIN local broker: a Unix socket, whose boundary is
/// the filesystem (put it in a 0700 directory; it is same-uid only). Capability
/// enforcement on attach is `Broker::open_guarded`, which needs a mint secret
/// this verb does not take, and the sealed TCP wire needs the `sealed` feature
/// this build does not enable. Both are named here rather than implied, because
/// "a broker is running" and "a broker is guarded" are different sentences.
fn broker(args: &[String]) -> ExitCode {
    const USAGE: &str = "\
usage: aterm link broker <socket> [log]

  <socket>  the Unix socket path the bridge's `--broker` names
  [log]     the durable record log (default: <socket>.log)

The local bus: no capability enforcement on attach, no TLS, and the SOCKET ITSELF
is created world-connectable — so the boundary is the DIRECTORY you put it in.
Use a 0700 one (tools/fabric-enable.sh does). Guarded and sealed transports are
astream's `Broker::open_guarded` / the `sealed` feature; neither is reachable
from here.
";
    let Some(sock) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    if sock == "-h" || sock == "--help" {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let log = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("{sock}.log"));
    let broker = match astream_broker::Broker::open(&log) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aterm link broker: open {log}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let handle = match broker.serve(sock) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("aterm link broker: serve {sock}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The line a supervisor waits for. `asb serve` prints the same word, so a
    // launchd/systemd readiness probe written against either one works.
    println!("listening {sock}");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // The broker serves on its own threads; this one parks. `BrokerHandle` has
    // no `join` — `asb serve` parks exactly this way — and dropping the handle
    // would shut the broker down and unlink the socket.
    let _keep = handle;
    loop {
        std::thread::park();
    }
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

        // §7'S ROW, IN §7'S ORDER, then the three documented additions.
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
                "epoch"
            ],
            "the row must be §7's columns in §7's order, then the additions: {line}"
        );
        assert!(
            line.contains(" attention=needs%20a%20key ") && line.contains(" gen=9:beef "),
            "{line}"
        );
        // The four columns §4.2 specifies and this fabric has no writer for read
        // `-`, which is "unknown" and not a promise.
        for absent in ["role=-", "detail=-", "driving=-", "fabric=-"] {
            assert!(line.contains(absent), "{absent} missing from: {line}");
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
            15,
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

    /// `--screen` NAMES SESSIONS, and `all` is spelled out.
    ///
    /// This is the one fabric face that puts screen CONTENT on an
    /// append-forever log with no encrypt-at-rest (§10, T12, §14's open question
    /// 2), so the flag is empty by default and a typo is a usage error rather
    /// than a session quietly not being published — or, worse, a bare `--screen`
    /// meaning "all of them".
    #[test]
    fn the_screen_flag_names_sessions_and_all_is_spelled_out() {
        let base = "--fleet f1 --broker /tmp/b.sock --state /tmp/s";
        assert!(parse(&argv(base)).expect("ok").cfg.screen.is_empty());
        assert_eq!(
            parse(&argv(&format!("{base} --screen all")))
                .expect("ok")
                .cfg
                .screen,
            vec!["all".to_string()]
        );
        // A comma list, and an `@`-prefixed sid, both the way `--accept-from`
        // and `post to=` already spell theirs.
        assert_eq!(
            parse(&argv(&format!("{base} --screen @s-aaa,s-bbb")))
                .expect("ok")
                .cfg
                .screen,
            vec!["@s-aaa".to_string(), "s-bbb".to_string()]
        );
        for bad in [
            "--screen",          // a flag with no value
            "--screen n-node",   // a node is not a session
            "--screen h-andrew", // nor is a human
            "--screen every",    // "all" or nothing
        ] {
            assert!(parse(&argv(&format!("{base} {bad}"))).is_err(), "{bad}");
        }
    }
}
