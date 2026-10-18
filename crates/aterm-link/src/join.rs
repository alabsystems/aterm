// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric mint-for` and `aterm fabric join` — a SECOND HOST joins the
//! fleet over the sealed transport.
//!
//! ```text
//! host 1:  aterm fabric on --tcp <bind> --key-file <k> [--allow-remote]
//!          aterm fabric mint-for <node-id>|new [--out <cap>] [--expires <N>d|<N>h]
//! host 2:  aterm fabric join --broker <host:port> --tcp --key-file <k> --cap-file <c>
//!                            [--node <id>] [--accept-from <p>,...] [--dry-run]
//!                            [--service launchd|systemd|none]
//! ```
//!
//! ## What crosses from host 1 to host 2, and what never does
//!
//! TWO FILES: the pre-shared KEY (`--key-file`, 64 hex characters — the sealed
//! wire's transport secret) and the joining node's CAP (`mint-for`'s 8 grants,
//! each tag an HMAC under host 1's mint secret). The MINT SECRET never leaves
//! host 1: it is the one thing that can make a capability, so a host holding it
//! could act as any node or human on the fleet, and host 2 has no use for it —
//! the broker on host 1 checks host 2's cap against it on every attach.
//!
//! ## Why the broker a second host dials is ALWAYS guarded
//!
//! The key is ONE secret every host of the fleet holds. It keeps a stranger off
//! the wire and says nothing about WHICH host is speaking; with it alone, host 2
//! could publish as host 1's node (its sessions' mail and presence rows), or as
//! a human: `h-` is a cap-forced `<src>`, so that mail is delivered
//! `trust=human`, a halt in that human's name holds every session on the
//! fleet, and a lift in it withdraws theirs. (Not keystrokes: no bus record
//! reaches a PTY, `bridge.rs`'s "The path to a PTY: there is none".) What stops
//! that is the capability: `rw,p=<node>:` grants bind every publish to the node
//! they were minted for (the broker's producer binding), and only a guarded
//! broker checks one. So `aterm link broker --tcp` refuses to run without
//! `--secret-file`, and `aterm fabric on --tcp` hands it the root's own mint
//! secret.
//!
//! ## What it does NOT protect
//!
//! The PSK is a transport boundary, not a per-peer identity (astream
//! REQUIREMENTS R6: identity-bound principals and revocation are designed, not
//! built). A copied key plus a copied cap IS that node, from any machine that
//! can reach the port; there is no revoking one host short of re-keying every
//! host and rotating the mint secret. What DOES bound a copied cap is its
//! expiry: `mint-for --expires 30d` stamps `exp=` into every grant, the broker
//! refuses the ring from that instant on (at attach, and on every request of a
//! connection already open). It is renewed by a fresh `mint-for` and a `join`
//! with the new cap: the running bridge reads its cap file again before every
//! dial, so it attaches the new ring at its next redial — at the latest when
//! the broker ends the lapsed ring's streams — with no restart. A ring minted
//! without `--expires` lasts until the secret is rotated.
//!
//! And host 1's uid IS the fleet's root of trust: `<root>/mint.secret` is 0600
//! to it, so every process running as it there — every agent in every session
//! — can mint a cap for any node or human, and the same uid supervises the
//! broker. A joined host's uid holds only its own node's ring. Agents not to
//! be trusted that far belong on a joined host. `docs/FABRIC-SECOND-HOST.md`
//! says all of this to the operator.
//!
//! ## `mint-for`
//!
//! Mints `<node-id>`'s eight grants (§8.2's node ring, [`crate::enable::node_grants`])
//! on THIS host's fleet, under THIS host's `<root>/mint.secret` — the root the
//! fabric is on, found the way `off` finds it. `new` provisions a fresh
//! `n-<16 hex>` for a host that has none yet. Like every sealed-wire surface
//! it is refused, by naming the feature, in a build without `sealed`. It refuses
//! a malformed id, this host's OWN node id (two bridges answering as one node
//! read each other's mail and fire each other's wills), a secret that is
//! missing, short or readable by another user (the one reader,
//! [`crate::transport::read_mint_secret`]) — and a root that JOINED another
//! host's fleet: its own node cap was not minted under its secret (a stale one
//! from an earlier `on`, or none), so the broker it dials would refuse every
//! cap it minted.
//! It never prints the secret; `--out` writes the cap 0600, atomically, never
//! over a DIFFERENT file and never through a symlink, and makes an IDENTICAL
//! file already there 0600 before it says so; without `--out` the eight lines
//! go to stdout (like `aterm link mint`) and the guidance to stderr. Its last
//! lines are the exact `join` to run on the other host.
//!
//! `mint-for h-<name>` (a human) or `a-<name>` (an agent with a cap of its
//! own) mints a PERSON's ring instead ([`crate::enable::person_grants`]) —
//! in EVERY build, because the one-host broker `on` runs is guarded as well:
//! the cap `aterm fabric halt|inbox|post` ([`crate::person`]) take, and the
//! one `aterm link notify --on ask:<person>` reads its mail with. The same
//! secret and joined-root refusals apply; the own-node one does not, and its
//! last lines name those commands instead of a `join`.
//!
//! ## `join`
//!
//! Every input is checked BEFORE anything is written: the build carries the
//! sealed transport, `--broker` resolves to a fixed port, the key is 64 hex in
//! a 0600 file, the cap is 0600 and is exactly one node's eight grants on one
//! fleet (the node and the fleet are READ from it), `--node` agrees with it,
//! and the cap was not minted under this root's own secret (that root is the
//! fleet's first host: joining itself would stop the broker it serves). Then:
//!
//! 1. the remote broker PROBED with the files as given — the sealed handshake
//!    (a wrong key fails here), every grant attached (a cap minted under
//!    another secret is refused here), and a read — so a refused join writes
//!    NOTHING on this host (the broker binds the node's producer id the first
//!    time its cap attaches — one hidden `/a/bind` record in the FIRST host's
//!    log, which the real join would write anyway). The read includes the node's
//!    own presence row: a node that is LIVE right now, on a root that never
//!    recorded it, is another root's — two bridges answering as one node
//!    deliver each other's mail as `not-hosted` and fire each other's wills —
//!    and is refused, exit 2;
//! 2. the root, 0700 (`~/.local/share/aterm-fabric`; a test's `$ATERM_FABRIC_HOME`
//!    seam redirects it in a debug build);
//! 3. the node id: RECORDED from the cap into `<root>/link-state/node` when
//!    there is none; the same one is `already`; a DIFFERENT one is refused —
//!    identity is provisioned, never re-minted — with the `mint-for` that fixes
//!    it;
//! 4. the key and the cap INSTALLED as `<root>/fleet.key` and `<root>/node.cap`,
//!    0600 — the root is then laid out like host 1's, `off` finds it, and the
//!    files that were copied in are never read again;
//! 5. this root's own LOCAL broker job, if `on` installed one, stopped and
//!    removed under `--service launchd|systemd` (a host dials ONE broker, and a
//!    `KeepAlive` bus nobody dials still holds the socket and its log);
//!    `--service none` touches no supervisor;
//! 6. then `on`'s own tail ([`crate::enable::finish`]): `[fabric] command`
//!    for the remote broker (with `--accept-from <node>,<--accept-from>`), the
//!    rendezvous file (`transport = "tcp+sealed"`), every running instance
//!    armed, and the PROOF — a note from a session to itself, out through the
//!    REMOTE broker and back, within 5 s — then the undo and `aterm fabric`.
//!
//! `--accept-from` names the principals whose `task`s this host takes as tasks
//! rather than notes (§8.4): the first host's node, for a manager there
//! assigning work here. `mint-for` prints it in the `join` line it suggests.

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::enable::{
    self, argv_safe, launchctl, launchd_loaded, node_grants, read_node, systemctl, uid,
    write_atomic, Out, Paths, Service, Verb, Wire,
};
use crate::render::safe;

/// `aterm fabric mint-for`'s flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MintForOpts {
    /// The node id to mint for, or `new`.
    pub node: String,
    /// `--out <cap>`: write the cap there (0600) instead of stdout.
    pub out: Option<String>,
    /// `--fleet <F>`: default this host's configured fleet.
    pub fleet: Option<String>,
    /// `--expires <N>d|<N>h` ([`parse_lifetime`]), as milliseconds: every
    /// grant is stamped `exp=<now + this>`, and the broker refuses the ring
    /// from that instant on. `None` mints a ring that never expires.
    pub expires: Option<u64>,
}

/// `aterm fabric join`'s flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct JoinOpts {
    /// `--broker <host:port>`: the remote broker.
    pub broker: Option<String>,
    /// `--tcp`: required, and said out loud — `join` speaks only the sealed wire.
    pub tcp: bool,
    /// `--key-file <k>`: the pre-shared key copied from the first host.
    pub key_file: Option<String>,
    /// `--cap-file <c>`: this node's cap, minted by `mint-for` on the first host.
    pub cap_file: Option<String>,
    /// `--node <id>`: assert the node id (it must be the cap's).
    pub node: Option<String>,
    /// `--accept-from <p>,...`: principals whose tasks arrive undemoted.
    pub accept_from: Vec<String>,
    /// `--dry-run`: print every step, touch nothing.
    pub dry_run: bool,
    /// `--service`: what stops this root's local broker job; `None` is the
    /// platform's default.
    pub service: Option<Service>,
}

/// The word `mint-for` takes in place of a node id to provision a fresh one.
pub const NEW_NODE: &str = "new";

// ---------------------------------------------------------------------------
// shared checks
// ---------------------------------------------------------------------------

/// Whether `node` is a NODE id: `n-` and a principal segment.
#[must_use]
pub fn is_node_id(node: &str) -> bool {
    node.starts_with("n-") && crate::subject::is_principal(node)
}

/// Whether `id` is a PERSON: an `h-` human or an `a-` agent holding its own cap
/// (§3.2), the principals `mint-for` mints [`crate::enable::person_grants`] for.
#[must_use]
pub fn is_person_id(id: &str) -> bool {
    (id.starts_with("h-") || id.starts_with("a-")) && crate::subject::is_principal(id)
}

/// The `(fleet, node)` a cap file's grants were minted for — exactly §8.2's
/// eight for ONE node on ONE fleet ([`node_grants`]), in any order, nothing
/// missing, nothing extra.
///
/// AUTHORITY IS EXACT, LIFETIME IS FREE. Every grant is read with astream's
/// own parser ([`astream_cap::Grant::parse`]) — never by hand, so `p=` and
/// `exp=` may come in either order — and the ring is compared by mode,
/// principal and filter. A ring `mint-for --expires` stamped with `exp=` is
/// the same ring; one whose grants have ALREADY lapsed is refused here, where
/// the remedy can be named, rather than as the bridge's `reason=expired` after
/// the join.
///
/// # Errors
///
/// A line that is not a grant, no `rw,p=<N>:/f/<F>/pub/<N>/>` grant to read
/// the identity off, a set of grants that is not that node's ring, or a ring
/// that has expired.
pub fn node_ring_of(grants: &[String]) -> Result<(String, String), String> {
    node_ring_at(grants, crate::now_ms())
}

/// [`node_ring_of`] at the instant `now_ms` (unix milliseconds).
fn node_ring_at(grants: &[String], now_ms: u64) -> Result<(String, String), String> {
    use astream_cap::{Grant, Mode};
    let parse =
        |g: &String| Grant::parse(g).map_err(|e| format!("`{}` is not a grant: {e}", safe(g, 128)));
    let parsed: Vec<Grant> = grants.iter().map(parse).collect::<Result<_, _>>()?;
    let identity = parsed.iter().find_map(|g| {
        let node = g.principal.as_deref()?;
        let fleet = g.filter.strip_prefix("/f/")?.split('/').next()?;
        (g.mode == Mode::ReadWrite && g.filter == format!("/f/{fleet}/pub/{node}/>"))
            .then(|| (fleet.to_string(), node.to_string()))
    });
    let Some((fleet, node)) = identity else {
        return Err(
            "it holds no `rw,p=<node>:/f/<fleet>/pub/<node>/>` grant, so it is not a node's cap \
             (`aterm fabric mint-for <node-id>` on the first host makes one)"
                .to_string(),
        );
    };
    if !is_node_id(&node) || !crate::subject::is_fleet(&fleet) {
        return Err(format!(
            "its identity grant names node {} on fleet {}, which are not a node id and a fleet",
            safe(&node, 64),
            safe(&fleet, 64)
        ));
    }
    let authority = |g: &Grant| {
        (
            g.mode == Mode::ReadWrite,
            g.principal.clone(),
            g.filter.clone(),
        )
    };
    let mut want: Vec<_> = node_grants(&fleet, &node)
        .iter()
        .map(parse)
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .map(authority)
        .collect();
    let mut have: Vec<_> = parsed.iter().map(authority).collect();
    want.sort();
    have.sort();
    if want != have {
        return Err(format!(
            "it holds {} grant(s), and they are not the eight of {node}'s ring on fleet {fleet} \
             (`aterm fabric mint-for {node}` mints exactly those)",
            grants.len()
        ));
    }
    if let Some(exp) = parsed
        .iter()
        .filter_map(|g| g.expires_at)
        .filter(|&exp| now_ms >= exp)
        .min()
    {
        return Err(format!(
            "it expired at {} (exp={exp}): on the first host, `aterm fabric mint-for {node} \
             --out <cap>` mints it again (`--expires` for a bounded one)",
            aterm_types::rfc3339::format_rfc3339(exp / 1000)
        ));
    }
    Ok((fleet, node))
}

/// `--expires <N>d|<N>h`: a capability lifetime, as milliseconds.
///
/// # Errors
///
/// Anything but a positive whole number of days or hours, at most ten years.
pub fn parse_lifetime(word: &str) -> Result<u64, String> {
    const HOUR_MS: u64 = 3_600_000;
    const MAX_HOURS: u64 = 10 * 366 * 24;
    let refuse = || {
        format!(
            "--expires {}: a whole number of days or hours, e.g. 30d or 12h",
            safe(word, 32)
        )
    };
    let (n, unit_hours) = if let Some(d) = word.strip_suffix('d') {
        (d, 24)
    } else if let Some(h) = word.strip_suffix('h') {
        (h, 1)
    } else {
        return Err(refuse());
    };
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refuse());
    }
    let hours = n
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit_hours))
        .filter(|&h| (1..=MAX_HOURS).contains(&h))
        .ok_or_else(refuse)?;
    Ok(hours * HOUR_MS)
}

/// `grant` with `exp=<exp_ms>` added to its prefix: `rw,p=<N>:` becomes
/// `rw,p=<N>,exp=<ms>:` and `ro:` becomes `ro,exp=<ms>:` — the grammar
/// astream-cap reads (`field := "p=" … | "exp=" …`). Every node grant has a
/// prefix, and the mint re-parses, so a stamp that went wrong fails closed.
fn with_expiry(grant: &str, exp_ms: u64) -> String {
    grant.replacen(':', &format!(",exp={exp_ms}:"), 1)
}

/// The fleet, the SEALED endpoint and its key file of THIS host's fabric,
/// when it has one: what `mint-for` names in the `join` it prints. On the
/// host that serves the sealed wire that is the rendezvous file's
/// `serves_tcp`/`serves_key_file` — its own bridges dial the socket, so its
/// `[fabric] command` carries no key. Else the config's `[fabric] command`
/// (its `--key-file` word on a joined host's sealed wire), else the rendezvous
/// file's broker and key.
fn this_fabric() -> Option<(String, String, Option<String>)> {
    let served = enable::Rendezvous::read()
        .ok()
        .flatten()
        .and_then(|r| Some((r.fleet, r.serves_tcp?, r.serves_key_file)));
    if served.is_some() {
        return served;
    }
    let from_config = crate::fabric::resolve_command(None, crate::fabric::config_path().as_deref())
        .ok()
        .flatten()
        .and_then(|(_, cmd)| {
            let flags = crate::fabric::serve_flags(&cmd)?;
            let key = flags
                .windows(2)
                .find(|w| w[0] == "--key-file")
                .map(|w| w[1].clone());
            let cfg = crate::fabric::bridge_config(&cmd).ok()?;
            Some((cfg.fleet, cfg.broker, key))
        });
    from_config.or_else(|| {
        enable::Rendezvous::read()
            .ok()
            .flatten()
            .map(|r| (r.fleet, r.broker, r.key_file))
    })
}

// ---------------------------------------------------------------------------
// mint-for
// ---------------------------------------------------------------------------

/// `aterm fabric mint-for <node-id>|new [--out <cap>] [--fleet <F>] [--expires <N>d|<N>h]`.
#[must_use]
pub fn mint_for(opts: &MintForOpts) -> ExitCode {
    // A PERSON'S RING IS FOR EVERY BUILD: the one-host broker `on` runs is
    // guarded too, so a human halts, reads and answers there with a cap of
    // their own. A NODE's is REFUSED BY NAME IN A DEFAULT BUILD, like `on
    // --tcp` and `join`: a second node is only for a host that joins over
    // the sealed wire.
    let person = is_person_id(&opts.node);
    if !person && !crate::transport::SEALED {
        eprintln!(
            "aterm fabric mint-for: {}",
            crate::transport::SEALED_UNAVAILABLE
        );
        return ExitCode::from(2);
    }
    let (p, root_from) = match Paths::resolve_for_off() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric mint-for: {e}");
            return ExitCode::from(2);
        }
    };
    let here = this_fabric();
    let fleet = opts
        .fleet
        .clone()
        .or_else(|| here.as_ref().map(|(f, _, _)| f.clone()))
        .unwrap_or_else(|| p.fleet.clone());
    if !crate::subject::is_fleet(&fleet) {
        eprintln!(
            "aterm fabric mint-for: fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
            safe(&fleet, 64)
        );
        return ExitCode::from(2);
    }
    let own = read_node(&p.state());
    let node = if opts.node == NEW_NODE {
        match aterm_uds::rand::hex_token::<8>() {
            Ok(hex) => format!("n-{hex}"),
            Err(e) => {
                eprintln!("aterm fabric mint-for: no entropy for a node id: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        opts.node.clone()
    };
    if !person && !is_node_id(&node) {
        eprintln!(
            "aterm fabric mint-for: `{}` is not a node id (`n-` and [a-z0-9-], in the \
             joining host's <root>/link-state/node), `{NEW_NODE}` for a fresh one, or a \
             person (`h-<name>` for a human, `a-<name>` for an agent with its own cap)",
            safe(&node, 64)
        );
        return ExitCode::from(2);
    }
    if !person && own.as_deref() == Some(node.as_str()) {
        eprintln!(
            "aterm fabric mint-for: {node} is THIS host's own node id ({}/node); a second host \
             needs its own. Use the other host's id, or `{NEW_NODE}`",
            p.state().display()
        );
        return ExitCode::from(2);
    }
    // The secret, through the one reader (0600, 32+ bytes, its exact bytes).
    // A missing one is answered below; any other refusal is answered now,
    // before the joined-root test reads "no usable secret" as "joined".
    let secret_path = p.secret();
    let shown = secret_path.to_string_lossy().into_owned();
    let secret = match crate::transport::read_mint_secret(&shown) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!(
                "aterm fabric mint-for: {}",
                crate::transport::describe(&shown, &e)
            );
            return ExitCode::FAILURE;
        }
    };
    // A ROOT THAT JOINED ANOTHER HOST'S FLEET MINTS NOTHING: the broker it
    // dials checks caps against THAT host's secret, so a secret left here by
    // an earlier `on` would mint caps the broker refuses. The test is the
    // root's own cap, minted under this root's secret only on the serving host.
    let cap = p.cap();
    if cap.exists() && !secret.as_deref().is_some_and(|s| minted_under(s, &cap)) {
        eprintln!(
            "aterm fabric mint-for: this root's node cap ({}) was not minted under {}: this \
             host JOINED a fleet whose broker another host serves{}. Run `aterm fabric \
             mint-for` on that host",
            cap.display(),
            p.secret().display(),
            here.as_ref().map_or_else(String::new, |(_, b, _)| format!(
                " (it dials {})",
                safe(b, 128)
            ))
        );
        return ExitCode::from(2);
    }
    let Some(secret) = secret else {
        eprintln!(
            "aterm fabric mint-for: no mint secret at {shown}: `aterm fabric on` on the fleet's \
             first host mints it"
        );
        return ExitCode::FAILURE;
    };
    let exp = opts
        .expires
        .map(|lifetime| crate::now_ms().saturating_add(lifetime));
    let ring = if person {
        enable::person_grants(&fleet, &node)
    } else {
        node_grants(&fleet, &node)
    };
    let mut lines = String::new();
    for grant in &ring {
        let grant = exp.map_or_else(|| grant.clone(), |exp| with_expiry(grant, exp));
        match astream_cap::mint(&secret, &grant) {
            Ok(c) => {
                lines.push_str(&astream_cap::capfile::format_line(&c.filter, &c.tag));
                lines.push('\n');
            }
            Err(e) => {
                eprintln!("aterm fabric mint-for: the mint refused `{grant}`: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let written = match &opts.out {
        None => {
            // THE CAP IS THE ANSWER: one that did not reach stdout whole is no mint the
            // caller holds, so it is refused before the guidance below says "to stdout".
            if let Err(line) = cap_on_stdout(write_capability_to(&mut io::stdout(), &lines)) {
                eprintln!("{line}");
                return ExitCode::FAILURE;
            }
            None
        }
        Some(out) => {
            let path = enable::absolute(out);
            // NEVER THROUGH A SYMLINK: `read` and the write below would both
            // follow one at `--out`, into a file somebody else chose.
            if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
                eprintln!(
                    "aterm fabric mint-for: {} exists and is not a regular file; remove it or \
                     choose another --out",
                    path.display()
                );
                return ExitCode::FAILURE;
            }
            match std::fs::read(&path) {
                // THE SAME CAP IS ALREADY THERE: made 0600 first, since the
                // line below says so, and a wider mode is reported.
                Ok(old) if old == lines.as_bytes() => match private_mode(&path) {
                    Ok(None) => {}
                    Ok(Some(was)) => eprintln!(
                        "aterm fabric mint-for: {} held this cap at mode {was:03o}, readable by \
                         other users; now 0600. If it may have been copied, mint for a fresh id \
                         (`mint-for {NEW_NODE}`)",
                        path.display()
                    ),
                    Err(e) => {
                        eprintln!("aterm fabric mint-for: {}: {e}", path.display());
                        return ExitCode::FAILURE;
                    }
                },
                Ok(_) => {
                    eprintln!(
                        "aterm fabric mint-for: {} exists and holds a different cap; remove it or \
                         choose another --out",
                        path.display()
                    );
                    return ExitCode::FAILURE;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if let Err(e) = write_atomic(&path, lines.as_bytes(), 0o600) {
                        eprintln!("aterm fabric mint-for: {}: {e}", path.display());
                        return ExitCode::FAILURE;
                    }
                }
                Err(e) => {
                    eprintln!("aterm fabric mint-for: {}: {e}", path.display());
                    return ExitCode::FAILURE;
                }
            }
            Some(path)
        }
    };
    // THE GUIDANCE, on stderr when the cap itself went to stdout, so a
    // redirect captures exactly the eight lines.
    let say = |line: String| {
        if written.is_some() {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    };
    say(format!(
        "minted {node}'s {} grants on fleet {fleet} under {} (root from {root_from}){}",
        ring.len(),
        secret_path.display(),
        written.as_ref().map_or_else(
            || " — to stdout".to_string(),
            |w| format!(" into {} (0600)", w.display())
        )
    ));
    let cap_shown = written
        .as_ref()
        .map_or_else(|| "<cap>".to_string(), |w| w.display().to_string());
    if person {
        if let Some(exp) = exp {
            say(format!(
                "they expire at {} (exp={exp}): the broker refuses them from then on; mint \
                 again here before then",
                aterm_types::rfc3339::format_rfc3339(exp / 1000)
            ));
        }
        let broker = here.as_ref().map_or("<broker>", |(_, b, _)| b.as_str());
        say(person_guidance(&node, &fleet, broker, &cap_shown));
        return ExitCode::SUCCESS;
    }
    if let Some(exp) = exp {
        say(format!(
            "they expire at {} (exp={exp}): the broker refuses them from then on. Before then, \
             mint again here and `join` with the new cap on the other host (its running bridge \
             attaches the new cap at its next redial)",
            aterm_types::rfc3339::format_rfc3339(exp / 1000)
        ));
    }
    say(
        "the joining host needs the cap above and the fleet's key file; the mint secret stays \
         here"
            .to_string(),
    );
    match here {
        Some((_, broker, Some(key))) => {
            say(format!(
                "on the joining host (a `sealed` build), with the key and the cap copied there \
                 and `chmod 600`'d:\n  aterm fabric join --broker {broker} --tcp --key-file \
                 <copied {key}> --cap-file <copied {cap_shown}>{}",
                own.as_ref()
                    .map_or_else(String::new, |o| format!(" --accept-from {o}"))
            ));
            if crate::transport::is_loopback_endpoint(&broker).unwrap_or(false) {
                say(format!(
                    "  ({broker} is loopback: another machine names this host's address, and the \
                     broker must bind one it can reach — `aterm fabric on --tcp 0.0.0.0:<port> \
                     --allow-remote`)"
                ));
            }
        }
        _ => say(
            "this host's fabric is not on the sealed TCP wire: run `aterm fabric on --tcp <bind> \
             --key-file <k>` first (a `sealed` build)"
                .to_string(),
        ),
    }
    ExitCode::SUCCESS
}

/// Write a capability result completely and flush it. Unlike incidental diagnostic
/// printing, every error (including a departed reader) prevents claiming delivery.
///
/// # Errors
/// Any write or flush failure; no partial result is certified as delivered.
pub(crate) fn write_capability_to(out: &mut dyn io::Write, text: &str) -> io::Result<()> {
    out.write_all(text.as_bytes()).and_then(|()| out.flush())
}

/// Whether the cap `mint-for` wrote to stdout reached its reader whole, as `written` says: any
/// write error refuses, a reader that went away included. A verb's answer into `| head -1` is no
/// failure ([`aterm_uds::stdout::answer_status`]), but a cap cut short (`| ssh host 'cat > cap'`
/// whose ssh died, `> cap` on a full disk) is no cap a host can join with, and "minted … to
/// stdout" would say it was.
fn cap_on_stdout(written: io::Result<()>) -> Result<(), String> {
    written.map_err(|e| {
        format!(
            "aterm fabric mint-for: the cap was not written whole to stdout ({e}), so none was \
             handed over; mint again"
        )
    })
}

/// What a person does with the cap `mint-for <person>` just made: the
/// commands, spelled with that cap. Run on any host whose `[fabric] command`
/// names this fleet's broker. Only a human (`h-`) halts, and every node takes
/// a human's task as a task; an `a-` service's ring has no halt grant, and its
/// task is one only where `--accept-from` lists it (the bridge's `accepted`).
fn person_guidance(person: &str, fleet: &str, broker: &str, cap: &str) -> String {
    let human = person.starts_with("h-");
    let halt = if human {
        format!(
            "  aterm fabric halt --cap-file {cap} <reason>    hold every session on the fleet\n  \
             aterm fabric halt --cap-file {cap} off         withdraw that halt\n"
        )
    } else {
        String::new()
    };
    let task = if human {
        format!("a `task` from {person} arrives as a task on every node")
    } else {
        format!(
            "{person} is not a human: it halts nothing, and its `task` arrives as a task only on \
             a node whose --accept-from lists {person}"
        )
    };
    format!(
        "with it, {person} uses the fleet from any host whose fabric is on (keep the cap \
         0600; it IS {person}):\n\
         {halt}  \
         aterm fabric inbox --cap-file {cap}            read what agents posted to={person}\n  \
         aterm fabric post --cap-file {cap} to=@<sid>@<node> kind=answer re=<off> <text>\n  \
         aterm link notify --fleet {fleet} --broker {broker} --cap-file {cap} --on \
         ask:{person} --exec <cmd>    be paged when an agent asks {person}\n\
         {task}"
    )
}

/// Make `path` 0600 if it is not: `Ok(None)` when it already was, `Ok(Some(
/// <old mode>))` when it was changed.
///
/// # Errors
///
/// The stat or the chmod.
fn private_mode(path: &Path) -> io::Result<Option<u32>> {
    use std::os::unix::fs::PermissionsExt;
    let was = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if was == 0o600 {
        return Ok(None);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(was))
}

// ---------------------------------------------------------------------------
// join
// ---------------------------------------------------------------------------

/// Everything `join` checked before it wrote anything.
struct Checked {
    broker: String,
    key_src: PathBuf,
    key_bytes: Vec<u8>,
    cap_src: PathBuf,
    cap_bytes: Vec<u8>,
    fleet: String,
    node: String,
}

/// The checks that come before anything is written — each a refusal, exit 2.
fn check_inputs(opts: &JoinOpts) -> Result<Checked, String> {
    if !crate::transport::SEALED {
        return Err(crate::transport::SEALED_UNAVAILABLE.to_string());
    }
    let Some(broker) = opts.broker.clone() else {
        return Err("--broker <host:port> is required: the first host's sealed broker".to_string());
    };
    if !opts.tcp {
        return Err("--tcp is required: `join` speaks only the sealed TCP wire".to_string());
    }
    let (Some(key), Some(cap)) = (opts.key_file.as_deref(), opts.cap_file.as_deref()) else {
        return Err(
            "--key-file <k> and --cap-file <c> are required (both copied from the first host)"
                .to_string(),
        );
    };
    match crate::transport::endpoint_port(&broker) {
        Some(0) | None => {
            return Err(format!(
                "--broker {}: a <host>:<port> with the broker's fixed port",
                safe(&broker, 128)
            ));
        }
        Some(_) => {}
    }
    if let Err(e) = crate::transport::is_loopback_endpoint(&broker) {
        return Err(format!("--broker {}: {e}", safe(&broker, 128)));
    }
    crate::transport::read_private_key_file(key)
        .map_err(|e| format!("--key-file {}", crate::transport::describe(key, &e)))?;
    let key_bytes = std::fs::read(key).map_err(|e| format!("--key-file {key}: {e}"))?;
    crate::transport::check_private(cap)
        .map_err(|e| format!("--cap-file {}", crate::transport::describe(cap, &e)))?;
    let caps = crate::bridge::read_cap_file(cap).map_err(|e| format!("--cap-file {e}"))?;
    let grants: Vec<String> = caps.iter().map(|c| c.grant.clone()).collect();
    let (fleet, node) = node_ring_of(&grants).map_err(|e| format!("--cap-file {cap}: {e}"))?;
    if let Some(asserted) = &opts.node {
        if asserted != &node {
            return Err(format!(
                "--node {} but the cap was minted for {node}: mint for this host's id on the first \
                 host (`aterm fabric mint-for {}`), or drop --node",
                safe(asserted, 64),
                safe(asserted, 64)
            ));
        }
    }
    for p in &opts.accept_from {
        if !crate::subject::is_principal(p) {
            return Err(format!("--accept-from {} is not a principal", safe(p, 64)));
        }
    }
    let cap_bytes = std::fs::read(cap).map_err(|e| format!("--cap-file {cap}: {e}"))?;
    Ok(Checked {
        broker,
        key_src: enable::absolute(key),
        key_bytes,
        cap_src: enable::absolute(cap),
        cap_bytes,
        fleet,
        node,
    })
}

/// Whether the caps in `cap_file` were minted under `secret` — with this
/// root's own secret, whether THIS root minted them, which makes it the
/// fleet's first host (and, for `on`, whether the broker it guards with that
/// secret will take them).
pub(crate) fn minted_under(secret: &[u8], cap_file: &Path) -> bool {
    let Ok(caps) = crate::bridge::read_cap_file(&cap_file.to_string_lossy()) else {
        return false;
    };
    !caps.is_empty()
        && caps.iter().all(|c| {
            astream_cap::mint(secret, &c.grant).is_ok_and(|m| m.filter == c.grant && c.tag == m.tag)
        })
}

/// Install `bytes` at `dest`, 0600: `already` when it holds exactly them AT
/// 0600 — the same bytes at another mode are made 0600, and the line says so.
fn install(name: &str, what: &str, src: &Path, dest: &Path, bytes: &[u8], out: &mut Out) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::read(dest) {
        Ok(old) if old == bytes => {
            let mode = std::fs::metadata(dest).map_or(0, |m| m.permissions().mode() & 0o777);
            if mode == 0o600 {
                out.already(name, &format!("{} (0600, {what})", dest.display()));
                return true;
            }
            out.done(
                name,
                &format!(
                    "chmod 600 {} — it holds this {what} at mode {mode:03o}",
                    dest.display()
                ),
                &format!(
                    "{} made 0600 (it held this {what} at mode {mode:03o})",
                    dest.display()
                ),
            );
            if out.dry_run {
                return true;
            }
            return match private_mode(dest) {
                Ok(_) => true,
                Err(e) => {
                    out.fail(name, &format!("{}: {e}", dest.display()));
                    false
                }
            };
        }
        Ok(_) => out.done(
            name,
            &format!(
                "replace {} with {} (0600) — it held a different {what}",
                dest.display(),
                src.display()
            ),
            &format!(
                "replaced {} with {} (0600; it held a different {what})",
                dest.display(),
                src.display()
            ),
        ),
        Err(_) => out.done(
            name,
            &format!("install {} as {} (0600)", src.display(), dest.display()),
            &format!("installed {} (0600, {what})", dest.display()),
        ),
    }
    if out.dry_run {
        return true;
    }
    match write_atomic(dest, bytes, 0o600) {
        Ok(()) => true,
        Err(e) => {
            out.fail(name, &format!("{}: {e}", dest.display()));
            false
        }
    }
}

/// Step 5: this root's own LOCAL broker job, if `on` installed one — stopped
/// and removed under launchd / systemd, untouched under `none`.
fn local_broker_step(p: &Paths, service: Service, remote: &str, out: &mut Out) -> bool {
    let label = p.label();
    match service {
        Service::None => {
            out.note(
                "broker",
                &format!(
                    "this host dials {}; a local broker, if one runs, is not managed here",
                    safe(remote, 128)
                ),
            );
            true
        }
        Service::Launchd => {
            let plist = p.plist();
            let loaded = launchd_loaded(&label);
            if !loaded && !plist.exists() {
                out.already(
                    "broker",
                    &format!(
                        "no local broker job for this root (launchd {label}): this host dials {}",
                        safe(remote, 128)
                    ),
                );
                return true;
            }
            out.done(
                "broker",
                &format!(
                    "stop launchd {label} and remove {} — this host dials {} now",
                    plist.display(),
                    safe(remote, 128)
                ),
                &format!("stopped launchd {label} and removed {}", plist.display()),
            );
            if out.dry_run {
                return true;
            }
            // By domain, as `off` does: no legacy `unload`, which acts on
            // the caller's domain and exits 0 whatever it did.
            if loaded {
                let domain = format!("gui/{}/{label}", uid());
                if let Err(e) = launchctl(&["bootout", &domain]) {
                    out.fail("broker", &e);
                    return false;
                }
            }
            if let Err(e) = std::fs::remove_file(&plist) {
                if e.kind() != io::ErrorKind::NotFound {
                    out.fail("broker", &format!("{}: {e}", plist.display()));
                    return false;
                }
            }
            true
        }
        Service::Systemd => {
            let unit = p.unit();
            if !unit.exists() {
                out.already(
                    "broker",
                    &format!(
                        "no local broker unit for this root (systemd --user {label}): this host \
                         dials {}",
                        safe(remote, 128)
                    ),
                );
                return true;
            }
            out.done(
                "broker",
                &format!(
                    "disable --now {label} and remove {} — this host dials {} now",
                    unit.display(),
                    safe(remote, 128)
                ),
                &format!(
                    "systemd --user {label} stopped and {} removed",
                    unit.display()
                ),
            );
            if out.dry_run {
                return true;
            }
            if let Err(e) = systemctl(&["disable", "--now", &label]) {
                out.fail("broker", &e);
                return false;
            }
            let _ = std::fs::remove_file(&unit);
            let _ = systemctl(&["daemon-reload"]);
            true
        }
    }
}

/// `aterm fabric join`.
#[must_use]
pub fn join(opts: &JoinOpts) -> ExitCode {
    let checked = match check_inputs(opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm fabric join: {e}");
            return ExitCode::from(2);
        }
    };
    let mut p = match Paths::resolve(Some(&checked.fleet)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric join: {e}");
            return ExitCode::from(2);
        }
    };
    p.wire = Wire::Sealed {
        bind: None,
        dial: checked.broker.clone(),
        key_file: p.installed_key(),
        allow_remote: false,
    };
    let service = opts.service.unwrap_or_else(Service::default_here);
    let node = checked.node.clone();
    let mut out = Out::new(opts.dry_run);
    println!(
        "aterm fabric join — {node} joins fleet {} at {} over the SEALED TCP wire · root {} · \
         service {}{}",
        safe(&checked.fleet, 64),
        safe(&checked.broker, 128),
        p.root.display(),
        match service {
            Service::Launchd => "launchd",
            Service::Systemd => "systemd",
            Service::None => "none",
        },
        if opts.dry_run {
            " · DRY RUN: every step printed, nothing touched"
        } else {
            ""
        }
    );

    // The refusals that come before anything is written. A secret here that
    // the one reader refuses (group-readable, short) is a refusal too: this
    // may be the fleet's first host, and `join` cannot tell without it.
    let own_secret = p.secret().to_string_lossy().into_owned();
    let own = match crate::transport::read_mint_secret(&own_secret) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            out.fail(
                "secret",
                &format!(
                    "{}: fix it (or move it aside if this host never served the fleet) and \
                     join again",
                    crate::transport::describe(&own_secret, &e)
                ),
            );
            return ExitCode::from(2);
        }
    };
    if own
        .as_deref()
        .is_some_and(|s| minted_under(s, &checked.cap_src))
    {
        out.fail(
            "cap",
            &format!(
                "it was minted under THIS root's own mint secret ({}): this is the fleet's first \
                 host; `join` is for a second one",
                p.secret().display()
            ),
        );
        return ExitCode::from(2);
    }
    let state = p.state();
    // A DIFFERENT node id already here is refused before anything is written:
    // identity is provisioned, never re-minted (a new id abandons this node's
    // mail lane).
    if let Some(have) = read_node(&state).filter(|have| *have != node) {
        out.fail(
            "node",
            &format!(
                "this host is {have} ({}/node) and the cap was minted for {node}. On the first \
                 host: `aterm fabric mint-for {have} --out <cap>`, and join with that cap",
                state.display()
            ),
        );
        return ExitCode::from(2);
    }
    for word in [
        p.aterm.as_str(),
        checked.broker.as_str(),
        &p.cap().to_string_lossy(),
        &p.installed_key().to_string_lossy(),
        &state.to_string_lossy(),
    ] {
        if let Err(e) = argv_safe(word) {
            out.fail("argv", &e);
            return ExitCode::from(2);
        }
    }
    match enable::binary_has_link(&p) {
        Ok(()) => out.ok(
            "binary",
            &format!(
                "{} ({}serve, sealed TCP compiled in)",
                p.aterm,
                if p.is_link_shim() { "" } else { "link " }
            ),
        ),
        Err(e) => {
            out.fail("binary", &e);
            return ExitCode::from(2);
        }
    }

    // 1. the remote broker, probed with the files as given — BEFORE anything
    // is written, so a wrong key or a foreign cap leaves this host untouched.
    // A dry run probes too (it writes nothing here) and keeps going past a
    // failure, to show every step.
    let seen = match probe_remote(&checked, &mut out) {
        Ok(seen) => seen,
        Err(()) if !opts.dry_run => return ExitCode::FAILURE,
        Err(()) => None,
    };
    // THE NODE IS NOBODY ELSE'S RIGHT NOW. A live presence row for a node
    // this root never recorded is ANOTHER root's bridge (the same cap joined
    // twice); a second would share its lane — mail answered `not-hosted`,
    // host= flipping, each one's will marking the node gone under the
    // other. Refused before anything is written. This root's OWN node — a
    // second `join` — is `already`.
    if let Some(row) = seen.filter(|r| r.state == "live") {
        if read_node(&state).as_deref() != Some(node.as_str()) {
            out.fail(
                "node",
                &format!(
                    "{node} is LIVE on the fleet (host={} fabric={}) and this root has \
                     never been it: another root answers as {node}. On the first host \
                     `aterm fabric mint-for {NEW_NODE} --out <cap>` and join with that cap, \
                     or run `aterm fabric off` on the other root and wait for state=gone",
                    safe(&row.host, 64),
                    safe(&row.fabric, 32)
                ),
            );
            if !opts.dry_run {
                return ExitCode::from(2);
            }
        }
    }

    // 2. the root
    if !enable::root_step(&p, &state, &mut out) {
        return ExitCode::from(2);
    }

    // 3. the node id: RECORDED from the cap, never re-minted.
    match read_node(&state) {
        Some(have) if have == node => {
            out.already("node", &format!("{node} ({}/node)", state.display()));
        }
        // A different id was refused before anything was written (above);
        // one that appeared since is the same refusal.
        Some(have) => {
            out.fail(
                "node",
                &format!(
                    "{have} appeared in {}/node during this run",
                    state.display()
                ),
            );
            return ExitCode::from(2);
        }
        None => {
            out.done(
                "node",
                &format!(
                    "record {node} (the node the cap was minted for) into {}/node",
                    state.display()
                ),
                &format!("recorded {node}"),
            );
            if !enable::record_node(&state, &node, &mut out) {
                return ExitCode::from(2);
            }
        }
    }

    // 4. the key and the cap, installed into the root.
    if !install(
        "key",
        "the fleet's pre-shared key",
        &checked.key_src,
        &p.installed_key(),
        &checked.key_bytes,
        &mut out,
    ) || !install(
        "cap",
        &format!("{node}'s 8 grants"),
        &checked.cap_src,
        &p.cap(),
        &checked.cap_bytes,
        &mut out,
    ) {
        return ExitCode::FAILURE;
    }

    // 5. this root's own local broker job.
    if !local_broker_step(&p, service, &checked.broker, &mut out) && !opts.dry_run {
        return ExitCode::FAILURE;
    }

    // The copies to delete — never the installed files themselves, which a
    // `join` re-applied from them (doctor's remedy) names as its sources.
    let copies: Vec<String> = [
        (&checked.key_src, p.installed_key()),
        (&checked.cap_src, p.cap()),
    ]
    .into_iter()
    .filter(|(src, dest)| !same_file(src, dest))
    .map(|(src, _)| src.display().to_string())
    .collect();
    if !opts.dry_run && !copies.is_empty() {
        out.note(
            "copies",
            &format!(
                "this host reads {} and {} now; delete the copies ({})",
                p.installed_key().display(),
                p.cap().display(),
                copies.join(" and ")
            ),
        );
    }

    // 6. `on`'s tail: config, rendezvous, instances, proof, undo, status.
    let argv = p.bridge_command_accepting(&node, &opts.accept_from);
    enable::finish(&p, &node, &argv, &mut out, opts.dry_run, Verb::Join)
}

/// Whether `a` and `b` name one file: the same path, or paths that resolve
/// to it.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .is_some_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

/// The joining node's own presence row on the bus, as the probe read it.
struct NodePresence {
    host: String,
    state: String,
    fabric: String,
}

/// Step 1: reach the remote broker FOR REAL with the given key and cap — the
/// sealed handshake, every grant attached, a read — and name what failed;
/// `Ok` carries the joining node's own presence row, when the bus holds one
/// (`Last{/f/<F>/pub/<node>/node/presence}`, which the node ring's
/// `ro:/f/<F>/pub/>` reads).
fn probe_remote(c: &Checked, out: &mut Out) -> Result<Option<NodePresence>, ()> {
    let key_src = c.key_src.to_string_lossy();
    let key = match crate::transport::read_key_file(&key_src) {
        Ok(k) => k,
        Err(e) => {
            out.fail("broker", &crate::transport::describe(&key_src, &e));
            return Err(());
        }
    };
    let cfg = crate::bridge::Config {
        fleet: c.fleet.clone(),
        broker: c.broker.clone(),
        transport: crate::transport::Transport::Sealed(Box::new(key)),
        cap_files: vec![c.cap_src.to_string_lossy().into_owned()],
        state_dir: String::new(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::Meta,
        receipts: false,
        log: crate::mirror::LogMode::Off,
    };
    let (view, conn) = crate::fabric::probe_broker(&cfg, &[]);
    if view.read_ok() {
        out.ok(
            "broker",
            &format!(
                "{} answers on the sealed wire: handshake, 8 grants attached and a read in {} ms \
                 (bus head @{})",
                safe(&c.broker, 128),
                view.rtt_ms.unwrap_or(0),
                view.head.unwrap_or(0)
            ),
        );
        let mut row = None;
        if let Some(mut conn) = conn {
            let filter = format!("/f/{}/pub/{}/node/presence", c.fleet, c.node);
            let read = crate::transport::walk_last(&mut conn, &filter, |(_, _, raw)| {
                let line = String::from_utf8_lossy(raw);
                let field = |k: &str| crate::fabric::kv(&line, k).unwrap_or("-").to_string();
                row = Some(NodePresence {
                    host: crate::pct::decode(&field("host")),
                    state: field("state"),
                    fabric: field("fabric"),
                });
                Ok(())
            });
            // A read that fails cannot say the node is someone else's; the
            // join goes on, and says it could not look.
            if let Err(e) = read {
                out.note(
                    "node",
                    &format!(
                        "{}'s presence row could not be read ({e}): whether another root \
                         answers as it now was not checked",
                        c.node
                    ),
                );
            }
        }
        return Ok(row);
    }
    let why = view.error.unwrap_or_else(|| "no answer".to_string());
    // TWO WAYS A SEALED CONNECT ENDS EARLY: a WRONG KEY fails the handshake's
    // key confirmation (`failed to authenticate`, definite); a connection
    // DROPPED before the broker said anything is astream's pre-authentication
    // bound (64 connections in the handshake, 5 s each) or not a broker.
    let hint = if why.contains("failed to authenticate") {
        "the sealed handshake failed to authenticate: this key is not the broker's — copy the \
         first host's key file again"
    } else if why.contains("reset")
        || why.contains("peer closed")
        || why.contains("failed to fill whole buffer")
        || why.contains("Broken pipe")
    {
        "the broker dropped the connection before the sealed handshake finished: its \
         pre-authentication slots were full (docs/FABRIC-SECOND-HOST.md), or it is not a sealed \
         broker. Retry in a few seconds"
    } else if view.protocol.is_some() {
        // THE HANDSHAKE PASSED and a broker answered — in another astream
        // protocol: the sealed layer versions itself apart from the bus, so
        // the port is open and the key is right. No retry on this pair works.
        "the first host's broker speaks another astream protocol: the two hosts run different \
         aterm builds — build both from the same commit (docs/FABRIC-SECOND-HOST.md)"
    } else if !view.reachable {
        "nothing answered: `aterm fabric` on the first host shows the endpoint, and its TCP port \
         must be open to this host"
    } else if !view.attached {
        "the broker refused the cap: it was not minted under that broker's mint secret — run \
         `mint-for` on the host that serves it"
    } else {
        "the cap attached and the read was refused: the fleet it names is not the broker's"
    };
    out.fail(
        "broker",
        &format!("{}: {} — {hint}", safe(&c.broker, 128), safe(&why, 256)),
    );
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CAP STDOUT DID NOT TAKE WHOLE IS REFUSED, a reader that went away included: the cap
    /// is the whole answer, and a host cannot join with part of one.
    #[test]
    fn a_cap_stdout_did_not_take_whole_is_refused() {
        assert_eq!(cap_on_stdout(Ok(())), Ok(()));
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::StorageFull] {
            let refused = cap_on_stdout(Err(kind.into())).expect_err("no cap was handed over");
            assert!(
                refused.starts_with("aterm fabric mint-for: the cap was not written whole"),
                "{kind:?}: {refused}"
            );
        }
    }

    /// A NODE'S RING IS READ OFF THE CAP, and nothing else passes for one: the
    /// eight grants in any order name their node and fleet; seven, nine, a
    /// grant for another node, or a human's ring are refused.
    #[test]
    fn a_cap_names_its_node_and_fleet_and_nothing_else_passes() {
        let ring = node_grants("lab", "n-0123456789abcdef");
        assert_eq!(
            node_ring_of(&ring),
            Ok(("lab".to_string(), "n-0123456789abcdef".to_string()))
        );
        let mut shuffled = ring.clone();
        shuffled.reverse();
        assert!(node_ring_of(&shuffled).is_ok(), "order does not matter");
        let mut short = ring.clone();
        short.pop();
        assert!(node_ring_of(&short).is_err(), "seven grants");
        let mut long = ring.clone();
        long.push("ro:/f/lab/>".to_string());
        assert!(node_ring_of(&long).is_err(), "a ninth, wider grant");
        let mut mixed = ring.clone();
        mixed[4] = "ro:/f/lab/in/n-other/>".to_string();
        assert!(node_ring_of(&mixed).is_err(), "another node's lane");
        assert!(node_ring_of(&["ro:/f/lab/pub/>".to_string()]).is_err());
        assert!(node_ring_of(&[]).is_err());
    }

    /// A RING'S LIFETIME IS FREE, ITS AUTHORITY IS NOT: the eight grants each
    /// stamped `exp=` — in either field order — are the same ring, while one
    /// that has lapsed is refused, and an `exp=` ring with
    /// a grant too many, or one that is not a grant at all, is still refused.
    #[test]
    fn a_ring_stamped_with_an_expiry_is_the_same_ring_until_it_lapses() {
        let node = "n-0123456789abcdef";
        let exp = 1_800_000_000_000u64;
        let stamped: Vec<String> = node_grants("lab", node)
            .iter()
            .map(|g| with_expiry(g, exp))
            .collect();
        assert!(
            stamped[0].starts_with(&format!("rw,p={node},exp={exp}:")),
            "{stamped:?}"
        );
        assert!(
            stamped[1].starts_with(&format!("ro,exp={exp}:")),
            "{stamped:?}"
        );
        for g in &stamped {
            assert_eq!(astream_cap::expires_at(g), Some(exp), "{g}");
        }
        let ok = Ok(("lab".to_string(), node.to_string()));
        assert_eq!(node_ring_at(&stamped, exp - 1), ok);
        let swapped: Vec<String> = stamped
            .iter()
            .map(|g| {
                g.replace(
                    &format!("p={node},exp={exp}"),
                    &format!("exp={exp},p={node}"),
                )
            })
            .collect();
        assert_ne!(swapped, stamped);
        assert_eq!(node_ring_at(&swapped, exp - 1), ok, "either field order");
        node_ring_at(&stamped, exp).expect_err("lapsed at exp");
        let mut long = stamped.clone();
        long.push(format!("ro,exp={exp}:/f/lab/>"));
        assert!(
            node_ring_at(&long, exp - 1).is_err(),
            "a ninth, wider grant"
        );
        let mut junk = stamped;
        junk[3] = "rw,p=n-x,ttl=9:/f/lab/>".to_string();
        assert!(
            node_ring_at(&junk, exp - 1).is_err(),
            "an unknown field is not a grant"
        );
    }

    /// `--expires` is a whole number of days or hours, never zero, never
    /// past ten years.
    #[test]
    fn a_lifetime_is_days_or_hours() {
        assert_eq!(parse_lifetime("30d"), Ok(30 * 24 * 3_600_000));
        assert_eq!(parse_lifetime("12h"), Ok(12 * 3_600_000));
        for bad in [
            "0d", "0h", "d", "30", "30m", "-1d", "+3h", "1.5d", "99999d", "",
        ] {
            assert!(parse_lifetime(bad).is_err(), "{bad}");
        }
    }

    /// JOINING ITSELF IS REFUSED: a cap minted under this root's own secret is
    /// recognised (every tag re-minted and compared), one minted under another
    /// secret is not.
    #[test]
    fn a_cap_minted_under_this_roots_secret_is_recognised() {
        let dir = std::env::temp_dir().join(format!("atjoin-self-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let cap_under = |key: &[u8], name: &str| {
            let lines: String = node_grants("lab", "n-0123456789abcdef")
                .iter()
                .map(|g| {
                    let c = astream_cap::mint(key, g).expect("mint");
                    astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
                })
                .collect();
            let path = dir.join(name);
            std::fs::write(&path, lines).expect("cap");
            path
        };
        assert!(minted_under(&[4u8; 32], &cap_under(&[4u8; 32], "own.cap")));
        assert!(!minted_under(
            &[4u8; 32],
            &cap_under(&[5u8; 32], "foreign.cap")
        ));
        assert!(!minted_under(&[4u8; 32], &dir.join("absent.cap")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A NODE ID is `n-` and a principal; `new` is the keyword, not an id.
    #[test]
    fn a_node_id_is_n_dash_and_a_principal() {
        assert!(is_node_id("n-0123456789abcdef"));
        assert!(is_node_id("n-m7"));
        for bad in [
            "new", "n-", "s-0123", "h-andrew", "n-UPPER", "n-a b", "n-a/b", "",
        ] {
            assert!(!is_node_id(bad), "{bad}");
        }
    }
}
