// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! PROVISIONING: everything `atpkg-keys setup` and `atpkg-keys join` do, minus the one
//! thing that cannot be done without a human — reading the paper phrase off `/dev/tty`.
//!
//! # What this module is for
//!
//! Arming the roster tier used to be six manual steps, three of which were "copy 44 base64
//! characters into `pins.rs` by hand". Every one of those was a chance to paste the wrong
//! 44 characters into the file that decides what the whole fleet trusts. The two verbs
//! built on this module reduce the human's job to writing one phrase on paper:
//!
//! * `setup` — the FIRST machine. Generates the master, prints the phrase once, arms
//!   `pins::PAPER_MASTER_PUBKEYS`, mints this machine's keypair, and creates the
//!   master-signed roster.
//! * `join` — EVERY later machine. Reads the phrase back, PROVES it against the committed
//!   anchor and the existing roster, then does the same remaining work. It edits NO trust
//!   anchor at all.
//!
//! # NEITHER VERB PUTS A MACHINE KEY IN `pins::UPDATE_CHANNEL_PUBKEYS`
//!
//! They used to, as a bridge. It was removed, and the reasoning is worth keeping because
//! the append looks helpful:
//!
//! * It buys a ROSTER-AWARE client nothing. With the master armed, the roster alone
//!   authorizes (`aterm_update::github::fetch_authoritative_release`); the keyset is not
//!   consulted and cannot grant.
//! * It buys a PRE-ROSTER client nothing either — not from here. Such a client can only
//!   learn a key from a release it already accepts, so an entry in this working tree
//!   reaches it only if a release is cut and adopted, and that release must itself be
//!   signed by a key it already holds. Minting a key on a laptop cannot change what a
//!   shipped binary trusts.
//! * And it COSTS something real, permanently. A keyset member is irrevocable for every
//!   client that ships with it: `machine-revoke` withdraws a machine from the roster, and
//!   a pre-roster client would go on accepting that same key forever. It also spends one
//!   of four slots in what is a rotation window, not a machine registry.
//!
//! Extending the pre-roster allowance is therefore a reviewed edit to `pins.rs` made as
//! part of a release, not a side effect of minting a key. The producer names the same fact
//! from the other end: `aterm_release::publish::PreRosterClients`.
//!
//! # The split, and why the secret is not in this module's signatures
//!
//! Every function here takes a [`MasterSeed`] — never a phrase, never bytes, never a
//! `String`. `MasterSeed` has no accessor returning its bytes, no `Clone`, and a `Debug`
//! that prints `<redacted>`, so there is no shape in which provisioning logic can write,
//! log or return the master. The phrase itself exists only inside `main.rs`, between the
//! `/dev/tty` read and the `seed()` call, and is printed in exactly one place: `setup`, at
//! generation, because the owner cannot write down what they cannot see.
//!
//! That split is also what makes this testable. The success path — derive, arm, mint,
//! roster, verify — runs against a synthetic seed with no terminal in sight, which matters
//! because a test harness has no controlling terminal and a prompt that could be satisfied
//! without one would defeat its own purpose (`master.rs`, leak vector 5).
//!
//! # THREE PHASES, and the order is the safety property
//!
//! 1. [`preflight`] — every refusal that can be made before a secret exists. It reads, it
//!    validates, it writes nothing. `setup` meeting an already-armed anchor stops here.
//! 2. [`plan`] — compute the ENTIRE result in memory: the new `pins.rs` text (both edits),
//!    the machine keypair, the roster document and its master signature. Still no writes.
//!    Everything that can fail on bad input fails here, with the working tree untouched.
//! 3. [`write_pins`] then [`write_rest`] — two writes of pre-computed bytes. The anchor
//!    file goes first and is verified by re-reading it, because `setup`'s caller prints the
//!    phrase between the two: a master that is armed but never printed is unrecoverable,
//!    while a phrase printed against a durable anchor can always be finished with `join`.
//!
//! # It does not commit, and it does not push
//!
//! Deliberately. Arming a trust anchor is a reviewed act, so the tool edits the working
//! tree and tells the operator to read the diff. [`render_report`] says so in the output.

use crate::fsio::{
    concat, create_secret_file, ensure_parent_dir, read_bytes, write_all_to, write_bytes,
    write_bytes_atomic,
};
use crate::master::MasterSeed;
use crate::pins_edit::{
    CHANNEL_ANCHOR, Edit, MASTER_ANCHOR, MAX_MASTER_MEMBERS, append_member, read_anchor,
    verify_members,
};
use aterm_update_core::roster::{Roster, RosterReject};

/// Where the roster and its master signature live by default — the release staging dir,
/// so a cut picks them up as assets alongside the appcast.
pub const DEFAULT_ROSTER: &str = "dist/aterm-machines.toml";

/// The machine's own secret key, relative to `$HOME`. This file NEVER leaves the machine
/// that minted it.
pub const MACHINE_KEY_REL: &str = ".aterm/machine.key";

/// The machine's PUBLIC record, beside its key: id, pubkey, mint time.
pub const MACHINE_PUB_REL: &str = ".aterm/machine.toml";

/// The anchor file, relative to the workspace root.
pub const PINS_REL: &str = "crates/aterm-update-core/src/pins.rs";

/// The roster id `setup` gives the INCUMBENT keyset head when no better name is supplied.
///
/// See [`plan`] for why the first roster must name that key at all. The default is a
/// description rather than a guess at a hostname, because this tool genuinely does not know
/// which machine holds that key — `--head-id` exists for the operator who does.
pub const DEFAULT_HEAD_ID: &str = "incumbent-head";

/// Which verb is running. The two share almost all of their work; what differs is who
/// supplies the master and what the anchor is allowed to look like beforehand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// The first machine: generate the master and arm the anchor.
    Setup,
    /// A later machine (or a re-run on the first): prove the master, then mint.
    Join,
}

impl Verb {
    /// The verb as it is spelled on the command line, for messages and provenance
    /// comments.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Join => "join",
        }
    }
}

/// Every path a provisioning run touches. Explicit rather than derived inside, so a test
/// can run the whole path inside a temporary directory without a `$HOME`, a repository or
/// a terminal.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `crates/aterm-update-core/src/pins.rs`.
    pub pins: String,
    /// The roster document; its detached master signature is `<roster>.sig`.
    pub roster: String,
    /// This machine's SECRET key file, written 0600 and never copied off the machine.
    pub key: String,
    /// This machine's public record.
    pub machine_pub: String,
    /// Whether `pins` was named by an explicit `--pins` rather than discovered by
    /// walking up from the cwd. Discovery means "this tree" — the tree this binary was
    /// built from — so the compiled-in second-master refusal applies; an explicit path
    /// names a DIFFERENT tree, whose own file anchors govern.
    pub pins_explicit: bool,
}

/// `$HOME/<rel>`, or an error naming what was missing.
pub fn home_path(rel: &str) -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let mut p = home;
    p.push('/');
    p.push_str(rel);
    Ok(p)
}

/// Find `crates/aterm-update-core/src/pins.rs` by walking up from the current directory.
///
/// The anchor file is found rather than configured because the operator's job is to type
/// one phrase, not to know a path — and because a `--pins` value typed wrong is another
/// 44-character transcription risk in a different costume. The flag still exists for
/// tests and for the operator who really does mean a different tree; this is the default.
pub fn discover_pins_path() -> Result<String, String> {
    let mut dir = std::env::current_dir()
        .map_err(|e| concat(&["cannot read the current directory: ", &e.to_string()]))?;
    // Bounded: a repository is not 24 levels below where anyone runs a CLI, and an
    // unbounded walk to `/` is how a tool ends up editing something it never meant to.
    for _ in 0..24 {
        let candidate = dir.join(PINS_REL);
        if candidate.is_file() {
            return candidate.to_str().map(str::to_string).ok_or_else(|| {
                "the path to pins.rs is not valid UTF-8; pass --pins explicitly".to_string()
            });
        }
        if !dir.pop() {
            break;
        }
    }
    Err(concat(&[
        "could not find ",
        PINS_REL,
        " above the current directory — run this from inside the aterm checkout, or pass \
         --pins <path>",
    ]))
}

/// Machine ids appear in signed documents and in a deny-list that must be typed correctly
/// under pressure after a laptop is stolen. So they are restricted to characters that
/// cannot be confused, cannot need quoting, and cannot be homoglyph-substituted: ASCII
/// letters, digits, `-` and `_`.
pub fn vet_machine_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 32 {
        return Err(
            "a machine id must be 1-32 characters (it is typed under pressure \
                    during a revocation; keep it short and memorable)"
                .to_string(),
        );
    }
    if !id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(
            "a machine id may contain only ASCII letters, digits, '-' and '_' — \
                    anything else invites a homoglyph in a deny-list entry"
                .to_string(),
        );
    }
    Ok(())
}

/// A stable, printable name for a roster rejection. Written out rather than `{:?}` so the
/// tool keeps its no-`format!` discipline.
#[must_use]
pub fn reject_name(r: &RosterReject) -> String {
    use RosterReject as R;
    String::from(match r {
        R::Disabled => "no master pinned",
        R::BadKey => "unusable master key",
        R::BadSig => "malformed signature",
        R::Verify => "signature did not verify",
        R::Malformed => "malformed roster",
        R::Schema => "roster schema is newer than this build",
        R::Rollback => "roster_seq below the recorded floor",
        R::Stale => "roster freshness window has lapsed",
        R::SeqMismatch => "roster_seq does not match the release",
        R::Unattributed => "release names no machine",
        R::UnknownMachine => "machine not on the roster",
        R::Revoked => "machine is revoked",
        R::Expired => "machine key has expired",
    })
}

/// Whether the caller may legitimately be the FIRST mint when no roster is on disk.
///
/// This exists because the "may a fresh roster be started?" decision used to live only in
/// [`preflight`], while the code that actually STARTS one ([`load_roster`]'s fresh branch)
/// keyed on nothing but file absence. `join`'s refusal was therefore a check at one point
/// in time, not an invariant: a roster deleted in the window between preflight and
/// [`plan`] (the operator is typing 64 characters; `dist/` is gitignored and a concurrent
/// cleanup can sweep both files) sailed straight into the fresh branch and minted the
/// same-sequence fork the refusal exists to prevent. The mode makes the invariant hold at
/// the point of USE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterExpectation {
    /// The roster must already be on disk — `join`'s position, always: a committed master
    /// implies a roster that master signed exists SOMEWHERE, and starting a second one
    /// forks it.
    MustExist,
    /// An absent roster is the legitimate first-mint case — `setup`, whose preflight has
    /// proved no master (and therefore no roster lineage) exists yet.
    MayCreateFresh,
}

/// The exact on-disk bytes of the roster pair a run planned against, carried so the write
/// phase can prove it is replacing the pair it read — the same premise-check discipline
/// [`write_pins`] applies to the anchor file.
#[derive(Debug, Clone)]
pub struct RosterSnapshot {
    pub raw: Vec<u8>,
    pub sig: Vec<u8>,
}

/// Read the roster and its signature, verify under `master_pubkey`, and parse. Returns
/// the roster and the exact pair bytes it was read from — `None` meaning it was created
/// fresh (no roster on disk, permitted only under
/// [`RosterExpectation::MayCreateFresh`]).
///
/// There is deliberately NO unverified parse path in this tool. Reading a roster means
/// verifying it first, which is also the automatic transcription check: a mistyped phrase
/// derives a different master and simply cannot verify a roster the real master signed.
///
/// An absent SIGNATURE beside a present roster is never fresh-mintable: that is a
/// half-published state, and silently starting over would discard every machine already
/// listed.
pub fn load_roster(
    path: &str,
    master_pubkey: &str,
    now: u64,
    expect: RosterExpectation,
) -> Result<(Roster, Option<RosterSnapshot>), String> {
    let sig_path = concat(&[path, ".sig"]);
    if std::fs::metadata(path).is_err() {
        if std::fs::metadata(&sig_path).is_ok() {
            return Err(concat(&[
                sig_path.as_str(),
                " exists but ",
                path,
                " does not — refusing to start a fresh roster over a half-published one",
            ]));
        }
        if expect == RosterExpectation::MustExist {
            // `join`'s refusal, enforced at the point of use and not only at preflight:
            // the same message, because it is the same fact — see [`RosterExpectation`].
            return Err(concat(&[
                "no roster at ",
                path,
                " (it may have existed when this run started and been removed since — \
                 dist/ is gitignored, so a cleanup can sweep it). COPY \
                 `aterm-machines.toml` AND `aterm-machines.toml.sig` back from a machine \
                 that has them, or from the latest release's assets, and run this again. \
                 A second roster will not be started: two rosters signed by the same \
                 master at the same sequence de-authorize each other's machines, and \
                 clients have no fallback from that. Nothing has been written.",
            ]));
        }
        return Ok((crate::roster_ops::empty(now), None));
    }
    let raw = read_bytes(path).map_err(|e| concat(&["read ", path, ": ", &e.to_string()]))?;
    let sig = read_bytes(&sig_path).map_err(|e| {
        concat(&[
            "read ",
            &sig_path,
            ": ",
            &e.to_string(),
            " (a roster without its master signature cannot be verified, and this tool \
             never parses an unverified roster)",
        ])
    })?;
    let verified = aterm_update_core::roster::verify_roster(&[master_pubkey], raw.clone(), &sig)
        .map_err(|e| {
            concat(&[
                "the existing roster at ",
                path,
                " does not verify under the master you typed (",
                &reject_name(&e),
                "). Either a character of the phrase is wrong — check the fingerprint \
                 above against your paper — or that roster was signed by a different \
                 master. Nothing has been written.",
            ])
        })?;
    let roster = Roster::parse(&verified).map_err(|e| {
        concat(&[
            "the roster verified but will not parse (",
            &reject_name(&e),
            ")",
        ])
    })?;
    Ok((roster, Some(RosterSnapshot { raw, sig })))
}

/// Emit an already-signed roster and its detached signature, failing AS A PAIR: on any
/// error return, the pair on disk is a consistent one — the previous roster with its
/// previous signature (or nothing at all, for a first publish) — never a new document
/// beside a stale signature.
///
/// Per-file atomicity (temp + fsync + rename) cannot deliver that by itself: it makes
/// each FILE whole, not the PAIR consistent. So both files are staged completely before
/// either is promoted, the document is promoted first, and a failure promoting the
/// signature ROLLS THE DOCUMENT BACK to its previous bytes (or removes it, when there
/// were none). The residual this honestly leaves is a CRASH between the two renames —
/// error paths restore, a process death cannot — and the recovery for that torn state is
/// to copy `aterm-machines.toml` + `.sig` back from another machine or from the latest
/// release's assets (git cannot restore them: `dist/` is gitignored).
pub fn publish_roster(path: &str, bytes: &[u8], sig: &[u8]) -> Result<(), String> {
    let _ = ensure_parent_dir(path);
    let sig_path = concat(&[path, ".sig"]);
    // The previous document, held for the rollback. `None` = first publish.
    let previous = read_bytes(path).ok();

    // Stage BOTH files to fsync'd siblings before promoting EITHER: a staging failure
    // (full disk, permissions) leaves the published pair untouched.
    let body_tmp = crate::fsio::stage_sibling_temp(path, bytes)
        .map_err(|e| concat(&["write ", path, ": ", &e.to_string()]))?;
    let sig_tmp = match crate::fsio::stage_sibling_temp(&sig_path, sig) {
        Ok(tmp) => tmp,
        Err(e) => {
            let _ = std::fs::remove_file(&body_tmp);
            return Err(concat(&["write ", &sig_path, ": ", &e.to_string()]));
        }
    };

    // Promote the document first...
    if let Err(e) = crate::fsio::promote_staged(&body_tmp, path) {
        let _ = std::fs::remove_file(&sig_tmp);
        return Err(concat(&["write ", path, ": ", &e.to_string()]));
    }
    // ...then its signature — and if THAT fails, the disk holds the new document beside
    // the old signature, which every verifier refuses as "signature did not verify" and
    // misdiagnoses as a mistyped phrase. Roll the document back so the pair stays the
    // one that was published before this run.
    if let Err(e) = crate::fsio::promote_staged(&sig_tmp, &sig_path) {
        let restored = match &previous {
            Some(old) => write_bytes_atomic(path, old).is_ok(),
            None => std::fs::remove_file(path).is_ok(),
        };
        return Err(concat(&[
            "write ",
            &sig_path,
            ": ",
            &e.to_string(),
            if restored {
                " — the roster document was ROLLED BACK to its previous bytes, so the \
                 published pair is the one from before this run"
            } else {
                " — AND the roster document could not be rolled back: the pair on disk \
                 is now a new document beside the previous signature, which no client \
                 will verify. Restore both files by copying `aterm-machines.toml` and \
                 `aterm-machines.toml.sig` from another machine or from the latest \
                 release's assets (git cannot restore them; dist/ is gitignored)"
            },
        ]));
    }
    Ok(())
}

/// The state a verb has proved BEFORE any secret exists. Produced by [`preflight`],
/// consumed by [`plan`].
pub struct Preflight {
    verb: Verb,
    id: String,
    /// The roster id `setup` will give the incumbent keyset head. Unused by `join`, whose
    /// roster already names it.
    head_id: String,
    paths: Paths,
    /// The anchor file's exact current bytes, as text.
    pins_src: String,
    /// What `PAPER_MASTER_PUBKEYS` holds in the working tree right now.
    master_members: Vec<String>,
    /// What `UPDATE_CHANNEL_PUBKEYS` holds in the working tree right now.
    channel_members: Vec<String>,
}

impl std::fmt::Debug for Preflight {
    /// Hand-written and terse. Nothing here is secret today, but this value travels beside
    /// a `MasterSeed` through the whole provisioning path, and a derived `Debug` that
    /// dumped an entire `pins.rs` into a panic message is noise no failure needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Preflight(")?;
        f.write_str(self.verb.name())?;
        f.write_str(", ")?;
        f.write_str(&self.id)?;
        f.write_str(")")
    }
}

impl Preflight {
    /// The channel keyset's head — the key this build signs with today.
    #[must_use]
    pub fn channel_head(&self) -> Option<&str> {
        self.channel_members.first().map(String::as_str)
    }
}

/// EVERY REFUSAL THAT CAN BE MADE BEFORE A SECRET EXISTS.
///
/// Nothing here writes a FILE, and nothing here needs the master. That ordering is the
/// whole point: an operator who is going to be refused should be refused before they type
/// 64 characters, and `setup` meeting an already-armed anchor must stop before it generates
/// a second master — generating one silently would strand the first, and the paper holding
/// the first is the only copy that exists.
///
/// It does create missing DIRECTORIES, which is the one deliberate exception and is here
/// rather than later for exactly the reason above. `$HOME/.aterm` does not exist on a fresh
/// machine — the first machine, which is what `setup` is for — and creating the machine key
/// inside it used to fail ENOENT at the last step of a run that had already armed the
/// anchor, published a master-signed roster naming a machine whose key was never written,
/// and burned one of four keyset slots on a public key whose private half existed only in
/// the memory of a process that was exiting. An `mkdir -p` before any of that is the whole
/// fix, and a failure here costs an error message.
pub fn preflight(verb: Verb, id: &str, head_id: &str, paths: &Paths) -> Result<Preflight, String> {
    vet_machine_id(id)?;
    vet_machine_id(head_id).map_err(|e| concat(&["--head-id: ", &e]))?;
    if verb == Verb::Setup && head_id == id {
        return Err(concat(&[
            "--head-id and --id are both '",
            id,
            "', but they name two DIFFERENT machines: --id is this machine, and --head-id \
             is the machine that already holds the incumbent channel key (whose private \
             half is not on this one). Give the incumbent its own id.",
        ]));
    }

    let raw = read_bytes(&paths.pins)
        .map_err(|e| concat(&["read ", &paths.pins, ": ", &e.to_string()]))?;
    let pins_src = String::from_utf8(raw).map_err(|_| {
        concat(&[
            &paths.pins,
            " is not valid UTF-8 — refusing to edit a trust anchor file this tool cannot \
             read",
        ])
    })?;

    // Reading BOTH anchors here is a shape check as much as a data read: if either is
    // spelled in a way the writer does not recognise, the run stops now rather than
    // halfway through.
    let master = read_anchor(&pins_src, MASTER_ANCHOR)?;
    let channel = read_anchor(&pins_src, CHANNEL_ANCHOR)?;

    match verb {
        Verb::Setup => {
            // THE REFUSAL THAT MATTERS MOST. A second master is not an additional
            // authority, it is a replacement for one that exists only on paper.
            if !master.members.is_empty() {
                return Err(concat(&[
                    "a paper master is ALREADY committed in ",
                    &paths.pins,
                    " (pins::PAPER_MASTER_PUBKEYS names ",
                    &master.members.len().to_string(),
                    " key(s)). `setup` would generate a SECOND master and strand the first \
                     — whose secret half is on paper and exists nowhere else. Use \
                     `atpkg-keys join --id <id>` instead: it proves the phrase you already \
                     wrote down and mints this machine under the master you already have.",
                ]));
            }
            // The same refusal against the COMPILED-IN anchor, because a working tree
            // can be reverted while a shipped build cannot: if this binary was built
            // from a tree that armed the tier, a master exists in the world regardless
            // of what the file currently says. ONLY for the discovered tree: an explicit
            // --pins names a different tree (a fixture, another checkout), and the
            // binary's own provenance says nothing about what that tree has armed —
            // its file anchor, checked above, is the authority there.
            if !paths.pins_explicit && !aterm_update_core::pins::PAPER_MASTER_PUBKEYS.is_empty() {
                return Err(
                    "this build was compiled with a paper master already pinned \
                     (pins::PAPER_MASTER_PUBKEYS), so a master exists even though the \
                     working tree's anchor is empty. Restore the anchor and use \
                     `atpkg-keys join --id <id>`, or work out which tree is authoritative \
                     before generating a second master."
                        .to_string(),
                );
            }
            // An existing roster means an existing master signed it. `setup` has no master
            // to verify it with, and overwriting it would discard whatever it names.
            if std::fs::metadata(&paths.roster).is_ok() {
                return Err(concat(&[
                    "a roster already exists at ",
                    &paths.roster,
                    ", so some master already signed it — but no master is committed. \
                     `setup` will not write over it. Either restore the anchor and use \
                     `join`, or move that roster aside if you are genuinely starting over.",
                ]));
            }
        }
        Verb::Join => {
            if master.members.is_empty() {
                return Err(concat(&[
                    "no paper master is committed in ",
                    &paths.pins,
                    " (pins::PAPER_MASTER_PUBKEYS is empty), so there is nothing for \
                     `join` to prove your phrase against. Run `atpkg-keys setup --id <id>` \
                     on the first machine, commit the anchor it writes, and join from \
                     there.",
                ]));
            }
            // THE ROSTER MUST ALREADY BE HERE, and this refusal is the whole reason the
            // check exists rather than the tool "helpfully" starting a new one.
            //
            // `join` runs where a master is already committed, which means a roster that
            // master signed already exists SOMEWHERE. It just does not exist HERE: the
            // roster lives under `dist/`, `dist/` is gitignored, and pins.rs says in as
            // many words that the roster does not travel with the repository. So a freshly
            // cloned second machine has an armed anchor and no roster, which is precisely
            // the state this branch is reached in — and treating it as "first mint" started
            // a BRAND-NEW roster at roster_seq 1 naming only this machine, and then signed
            // it with the real master. Two divergent, both-valid, same-sequence rosters:
            // whichever is published de-authorizes every machine on the other, the
            // monotonic ratchet cannot see a same-sequence fork, and a client that meets a
            // release it cannot attribute is wedged with no fallback.
            //
            // There is no safe way to guess which side is authoritative, so the tool does
            // not guess. It asks for the file.
            if std::fs::metadata(&paths.roster).is_err() {
                return Err(concat(&[
                    "no roster at ",
                    &paths.roster,
                    ". A master IS committed, so a roster the master signed already exists \
                     — but it does not live in the repository (dist/ is gitignored and the \
                     roster is published as a release asset), so a fresh checkout does not \
                     have it. COPY `aterm-machines.toml` AND `aterm-machines.toml.sig` from \
                     a machine that already has them, or from the latest release's assets, \
                     and run `join` again. `join` will not start a second roster: two \
                     rosters signed by the same master at the same sequence de-authorize \
                     each other's machines, and clients have no fallback from that.",
                ]));
            }
        }
    }

    // The directories this run will write into, created before anything secret exists.
    // See this function's doc for the ENOENT-at-the-last-step failure this closes.
    for path in [&paths.key, &paths.machine_pub, &paths.roster] {
        ensure_parent_dir(path)
            .map_err(|e| concat(&["cannot create the directory for ", path, ": ", &e.to_string()]))?;
    }

    // The key-file check comes AFTER the verb checks, deliberately. An operator who runs
    // `setup` a second time on the machine that ran it first trips both — and of the two,
    // "a master is already committed, use `join`" is the one that tells them what is
    // actually going on. Reporting "machine.key already exists" first would send them to
    // fix the wrong thing. Both are still before any secret exists, which is the property
    // that matters. (`create_new` at write time remains the real guard against a race;
    // this is the courteous one.)
    if std::fs::metadata(&paths.key).is_ok() {
        return Err(concat(&[
            &paths.key,
            " already exists. This machine already has a key — minting a second one under \
             a new id is fine, but do it with --key pointing somewhere else, and never \
             overwrite a key the roster still names.",
        ]));
    }

    // There is deliberately NO "is there room in the keyset" check here any more: this
    // machine does not need a slot. See the module doc for why the append was removed
    // rather than made conditional.

    Ok(Preflight {
        verb,
        id: id.to_string(),
        head_id: head_id.to_string(),
        paths: paths.clone(),
        pins_src,
        master_members: master.members,
        channel_members: channel.members,
    })
}

/// `join`'s TRANSCRIPTION PROOF, part one: the master the operator just typed must be one
/// the committed anchor already names.
///
/// Part two is [`plan`]'s roster verification, which proves the same phrase against a
/// document that master actually signed. Both run before anything is written, so a
/// mistyped phrase costs an error message and nothing else.
///
/// Calling this from the orchestrator is COURTESY, not the enforcement: [`plan`] re-runs
/// this exact check for `join` itself, so a caller that skips it (and substitutes an
/// attacker-signed `--roster`, which the roster proof alone would accept) is still
/// refused at the point of use.
pub fn verify_master(pre: &Preflight, seed: &MasterSeed) -> Result<(), String> {
    let derived = seed.pubkey_b64()?;
    if pre.master_members.contains(&derived) {
        return Ok(());
    }
    Err(concat(&[
        "the phrase you typed derives the master ",
        &derived,
        ", which is NOT the master committed in ",
        &pre.paths.pins,
        ". Either a character of the phrase is wrong — compare the fingerprint printed \
         above against the one on your paper — or this is a different master entirely. \
         Nothing has been written.",
    ]))
}

/// The whole result, computed in memory. Nothing here has touched the filesystem.
pub struct Planned {
    verb: Verb,
    id: String,
    paths: Paths,
    /// The complete new text of `pins.rs`. `None` when neither anchor needed changing —
    /// the idempotent re-run.
    pins_text: Option<String>,
    /// The anchor file's bytes as they were when this run read them. Carried so the write
    /// can prove it is replacing the file it planned against, and not one somebody else
    /// changed in the meantime.
    pins_src: String,
    /// The incumbent keyset head, seeded onto a FRESH roster as `(id, pubkey)`. `None` when
    /// there was no incumbent (a fork with no channel pin) or when the roster already
    /// existed.
    seeded_head: Option<(String, String)>,
    /// The exact roster-pair bytes this plan was computed against (`None` = planned
    /// fresh), so [`write_rest`] can prove it is extending the pair it read — the same
    /// premise check [`write_pins`] runs over the anchor file.
    roster_snapshot: Option<RosterSnapshot>,
    master_after: Vec<String>,
    channel_after: Vec<String>,
    master_pubkey: String,
    master_fingerprint: String,
    machine_pkcs8: Vec<u8>,
    machine_pubkey: String,
    roster_bytes: Vec<u8>,
    roster_sig: Vec<u8>,
    roster_seq: u64,
    roster_valid_until: String,
    roster_machines: Vec<String>,
    roster_was_fresh: bool,
    now: u64,
}

impl std::fmt::Debug for Planned {
    /// Hand-written, because this value holds `machine_pkcs8` — the machine's SECRET
    /// signing key. A derived `Debug` would print it into any panic message that formatted
    /// a `Result` containing one. Same discipline as `MasterSeed`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Planned(")?;
        f.write_str(self.verb.name())?;
        f.write_str(", ")?;
        f.write_str(&self.id)?;
        f.write_str(", secret=<redacted>)")
    }
}

/// The ISO date, for the provenance comment written beside a key.
fn today(now: u64) -> String {
    let stamp = aterm_types::rfc3339::format_rfc3339(now);
    stamp.get(..10).unwrap_or("").to_string()
}

/// COMPUTE EVERYTHING, WRITE NOTHING.
///
/// By the time this returns, the exact bytes of the new `pins.rs`, the roster and its
/// master signature all exist in memory and have been checked. What remains is two file
/// writes of already-decided content, which is as close to atomic as a tool that must edit
/// a source file and a signed document can honestly get.
pub fn plan(pre: Preflight, seed: &MasterSeed, now: u64) -> Result<Planned, String> {
    let master_pubkey = seed.pubkey_b64()?;
    let master_fingerprint = seed.fingerprint()?;

    // `join`'s ANCHOR PROOF, re-run here so it cannot be skipped. `verify_master` is a
    // free function the orchestrator calls for the courteous early error, but nothing in
    // the types forced it to have run before this point — and the roster verification
    // below proves the phrase only against the OPERATOR-SUPPLIED `--roster` file, not
    // against the committed anchor. Repeating the containment check inside `plan` closes
    // that: a join planned under a master the anchor does not name refuses here, however
    // this function was reached.
    if pre.verb == Verb::Join {
        verify_master(&pre, seed)?;
    }

    // The machine keypair. Generated here, on this machine, and never copied off it.
    let (machine_pkcs8, machine_pubkey) = crate::generate()?;

    // The hierarchy must not collapse: `pins::tests::the_master_is_never_also_a_channel_
    // signing_key` refuses a master that is also a channel key, and the reason is the
    // whole premise of the tier — a master that signs releases lives on the release
    // machine, which is exactly the arrangement the previous one-key design was retired
    // for. A fresh Ed25519 collision is not a realistic accident, but a re-run against a
    // hand-edited file is, so it is checked rather than assumed.
    if pre.channel_members.contains(&master_pubkey) {
        return Err(concat(&[
            "the paper master ",
            &master_pubkey,
            " is already listed as a CHANNEL SIGNING key. That collapses the hierarchy \
             back into one key while looking like a hierarchy; refusing.",
        ]));
    }
    if machine_pubkey == master_pubkey {
        return Err("the freshly minted machine key is the paper master; refusing".to_string());
    }

    // --- the anchor edits, in one pass over the file text -------------------------
    let mut text = pre.pins_src.clone();
    let mut changed = false;
    let mut master_after = pre.master_members.clone();

    if pre.verb == Verb::Setup {
        let date = today(now);
        let comment = [
            "The PAPER MASTER — the offline root of the machine roster, armed by",
            concat(&["`atpkg-keys setup --id ", &pre.id, "` on ", &date, "."]).as_str(),
            "Its secret half is 52 base32 characters ON PAPER and exists on no",
            "computer. It signs aterm-machines.toml and nothing else.",
        ]
        .map(str::to_string);
        let refs: Vec<&str> = comment.iter().map(String::as_str).collect();
        match append_member(&text, MASTER_ANCHOR, &master_pubkey, &refs, MAX_MASTER_MEMBERS)? {
            Edit::AlreadyPresent { members } => master_after = members,
            Edit::Changed {
                text: next,
                members,
            } => {
                text = next;
                master_after = members;
                changed = true;
            }
        }
    }

    // THE CHANNEL KEYSET IS NOT TOUCHED, by either verb. This machine's key goes on the
    // ROSTER and nowhere else — the roster is what authorizes it, and a keyset entry
    // would be an irrevocable grant to clients this tool cannot reach anyway. See the
    // module doc. Carrying the members forward unchanged is not a formality: `write_pins`
    // verifies the anchor file against this list afterwards, so what used to prove "the
    // append landed" now proves the stronger property that the keyset is UNTOUCHED.
    let channel_after = pre.channel_members.clone();

    // --- the roster ---------------------------------------------------------------
    // Loading it VERIFIES it under the master just derived. For `join` that is the second
    // half of the transcription proof; for `setup` there is nothing on disk to verify, so
    // it starts at sequence 0. `join` passes `MustExist`, which is what actually keeps
    // the fresh path `setup`'s alone: preflight refuses an absent roster too, but that is
    // a check at an earlier point in time — the roster can vanish in the window while
    // the operator types the phrase, and treating THAT as first mint would fork a second
    // same-sequence roster (see [`RosterExpectation`]).
    let expectation = match pre.verb {
        Verb::Join => RosterExpectation::MustExist,
        Verb::Setup => RosterExpectation::MayCreateFresh,
    };
    let (mut roster, roster_snapshot) =
        load_roster(&pre.paths.roster, &master_pubkey, now, expectation)?;
    let roster_was_fresh = roster_snapshot.is_none();

    // THE FIRST ROSTER MUST NAME THE INCUMBENT CHANNEL HEAD, and this is where that
    // happens.
    //
    // Arming the master changes what the CUTTER checks. With the anchor empty a cut is
    // authorized by head equality — the signing key must BE `UPDATE_CHANNEL_PUBKEYS[0]`.
    // Arm it and that widens to keyset membership AND a roster lookup by public key
    // (`aterm_release::machines::authorize_cut`). So a first roster that named only the
    // machine running `setup` would, the moment the anchor was committed, make the machine
    // holding the head key unable to cut at all — and that machine is the only one whose
    // key every shipped client already accepts. The channel would be bricked in both
    // directions: the one machine that can be verified may not sign, and the one machine
    // that may sign cannot be verified. `pins.rs` states this as step 5 of its activation
    // sequence; the tool now satisfies it instead of printing it.
    //
    // ONLY the head is seeded, deliberately. The other keyset members are accept-only keys
    // inside a rotation window: today head equality is what stops them cutting, and putting
    // them on the roster would hand them a cutting authority the shipped fleet cannot
    // verify — the exact permanent wedge the keyset ordering exists to prevent. They join
    // the roster when their machine runs `join`, which is also when the operator finds out
    // (from the closing report) that it cannot cut yet.
    let mut seeded_head = None;
    if roster_was_fresh
        && let Some(head) = pre.channel_members.first()
        && *head != machine_pubkey
    {
        roster = crate::roster_ops::add(roster, &pre.head_id, head, now)?;
        seeded_head = Some((pre.head_id.clone(), head.clone()));
    }
    let roster = crate::roster_ops::add(roster, &pre.id, &machine_pubkey, now)?;
    let roster_bytes = roster
        .to_toml()
        .map_err(|e| {
            concat(&[
                "refusing to emit an invalid roster (",
                &reject_name(&e),
                ")",
            ])
        })?
        .into_bytes();
    let roster_sig = seed.sign(&roster_bytes)?;

    Ok(Planned {
        verb: pre.verb,
        id: pre.id,
        paths: pre.paths,
        pins_text: if changed { Some(text) } else { None },
        pins_src: pre.pins_src,
        seeded_head,
        roster_snapshot,
        master_after,
        channel_after,
        master_pubkey,
        master_fingerprint,
        machine_pkcs8,
        machine_pubkey,
        roster_bytes,
        roster_sig,
        roster_seq: roster.roster_seq,
        roster_valid_until: roster.valid_until.clone(),
        roster_machines: roster.machines.iter().map(|m| m.id.clone()).collect(),
        roster_was_fresh,
        now,
    })
}

/// WRITE THE ANCHOR FILE, THEN PROVE IT LANDED.
///
/// Separated from [`write_rest`] because `setup` prints the paper phrase between the two.
/// A master armed in the file but never shown to the owner is unrecoverable; a phrase
/// shown against a durable anchor can always be finished later with `join`. So the anchor
/// is durable first, and only then is the secret put on the screen.
///
/// The verification re-reads the FILE, not the string that was just built. Building
/// correct text and getting it onto a disk are two different claims, and a short write, a
/// full filesystem or a concurrent editor all break only the second.
///
/// # The file it replaces must be the file it planned against
///
/// The new text is a whole-file image built from a snapshot [`preflight`] took, and
/// preflight runs BEFORE the human types the phrase. `join`'s window is therefore as long
/// as it takes to type 64 characters, and this repository has other agents and other
/// sessions writing to it. Writing the snapshot back would silently REVERT anything that
/// landed in between — including, in the worst case, a reviewed commit retiring a
/// compromised channel key, which would be resurrected into the keyset by a run that then
/// reported success. So the current bytes are re-read and compared first, and a file that
/// moved is a refusal, not a merge: this writer's whole premise is that it refuses rather
/// than guesses.
pub fn write_pins(planned: &Planned) -> Result<(), String> {
    if let Some(text) = &planned.pins_text {
        let current = read_bytes(&planned.paths.pins)
            .map_err(|e| concat(&["re-read ", &planned.paths.pins, ": ", &e.to_string()]))?;
        if current != planned.pins_src.as_bytes() {
            return Err(concat(&[
                &planned.paths.pins,
                " CHANGED ON DISK since this run read it (something else edited it — a \
                 `git checkout`, a rebase, an editor, another agent). This run's edit was \
                 computed against the older bytes, so writing it now would silently revert \
                 that change. NOTHING HAS BEEN WRITTEN. Read `git diff` over the anchor \
                 file, then run this command again against what the file says now.",
            ]));
        }
        // Atomic: a sibling temporary, flushed, then renamed over the target. A truncating
        // write here could leave the trust anchor half-written, and the operator would be
        // told the run wrote nothing.
        write_bytes_atomic(&planned.paths.pins, text.as_bytes())
            .map_err(|e| concat(&["write ", &planned.paths.pins, ": ", &e.to_string()]))?;
    }
    let raw = read_bytes(&planned.paths.pins).map_err(|e| {
        concat(&[
            "re-read ",
            &planned.paths.pins,
            " to verify the write: ",
            &e.to_string(),
        ])
    })?;
    let back = String::from_utf8(raw)
        .map_err(|_| concat(&[&planned.paths.pins, " is not valid UTF-8 after writing"]))?;
    if let Some(text) = &planned.pins_text
        && back != *text
    {
        return Err(concat(&[
            &planned.paths.pins,
            " does not hold the bytes that were just written — `git diff` it before doing \
             anything else",
        ]));
    }
    let damaged = |e: String| {
        concat(&[
            "after writing, ",
            &e,
            " — the anchor file may be damaged; `git diff` ",
            &planned.paths.pins,
            " before doing anything else",
        ])
    };
    verify_members(&back, MASTER_ANCHOR, &planned.master_after).map_err(damaged)?;
    verify_members(&back, CHANNEL_ANCHOR, &planned.channel_after).map_err(damaged)
}

/// What a completed run produced, for [`render_report`] to speak.
pub struct Report {
    pub verb: Verb,
    pub id: String,
    pub paths: Paths,
    pub master_pubkey: String,
    pub master_fingerprint: String,
    pub master_after: Vec<String>,
    pub machine_pubkey: String,
    /// Whether this machine's public key IS the committed keyset's HEAD.
    ///
    /// Normally FALSE, and that is not a defect: this tool does not touch the keyset.
    /// It is true only when a reviewed commit already put this key at index 0 — i.e.
    /// when this machine IS the incumbent whose releases pre-roster clients can verify —
    /// and that single fact decides everything the closing report has to say about the
    /// installed base.
    ///
    /// HEAD, not membership, and the distinction is the difference between a correct
    /// report and one that talks an operator into bricking the fleet. A non-head keyset
    /// member is in this tree and in no shipped build (step 1 of the rotation appends it
    /// so a FUTURE build can carry it), so "your key is in the keyset, therefore old
    /// clients can verify you" is false for exactly the member most likely to be there.
    /// `publish::channel_signature_policy` tests the same thing the same way.
    pub machine_is_committed_head: bool,
    pub channel_after: Vec<String>,
    pub roster_seq: u64,
    pub roster_valid_until: String,
    pub roster_machines: Vec<String>,
    /// Whether this run CREATED the roster rather than extending one. Reported out loud:
    /// a roster that appeared from nowhere is either the first one ever (fine, and only
    /// `setup` can be in that position) or a fork of one that exists elsewhere (fatal), and
    /// the operator is the only one who can tell which.
    pub roster_was_fresh: bool,
    /// The incumbent keyset head this run put on the fresh roster, `(id, pubkey)`.
    pub seeded_head: Option<(String, String)>,
    pub pins_changed: bool,
}

impl std::fmt::Debug for Report {
    /// Hand-written and terse, matching [`Preflight`] and [`Planned`].
    ///
    /// Everything in a `Report` is a public identity, so there is nothing here to redact —
    /// but a derived impl on a value that travels beside a `MasterSeed` is the kind of thing
    /// that stays derived while the fields change underneath it. The convention is cheaper
    /// to keep than to re-audit.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Report(")?;
        f.write_str(self.verb.name())?;
        f.write_str(", ")?;
        f.write_str(&self.id)?;
        f.write_str(", roster_seq ")?;
        f.write_str(&self.roster_seq.to_string())?;
        f.write_str(")")
    }
}

/// Write the machine's own two files, then the roster and its master signature.
///
/// # The secret key goes FIRST, and that order is a correction
///
/// It used to go last, on the reasoning that a failure would then leave "either the roster
/// names this machine and its key exists, or neither". The reasoning was right and the
/// order implemented the opposite of it: creating the key is the step most likely to fail
/// (a missing directory, a permission, an existing file), and when it failed the roster had
/// already been signed and published naming a machine whose key had just been discarded —
/// a signed document asserting something false, plus a keyset slot spent on a public key
/// with no private half.
///
/// Reversed, the worst case is a machine that holds a key nothing has authorized yet. No
/// signed document says anything untrue, and the recovery is two commands the error message
/// names. `create_new` still guarantees an existing key is never clobbered.
pub fn write_rest(planned: Planned) -> Result<Report, String> {
    // THE ROSTER'S PREMISE CHECK, first and before ANY write — the same rule `write_pins`
    // applies to the anchor file, for the same reason: the signed roster this run built
    // extends the pair `plan` read, and this repository has other agents and other
    // sessions. Two runs that both read seq N and both publish seq N+1 produce a
    // same-sequence fork the monotonic ratchet cannot see, and the last writer silently
    // de-authorizes the machine the first one added. A pair that moved (or appeared,
    // when this run planned fresh) is a refusal, not a merge.
    let sig_path = concat(&[&planned.paths.roster, ".sig"]);
    match &planned.roster_snapshot {
        Some(snapshot) => {
            let body_now = read_bytes(&planned.paths.roster);
            let sig_now = read_bytes(&sig_path);
            let unchanged = body_now.as_deref().ok() == Some(snapshot.raw.as_slice())
                && sig_now.as_deref().ok() == Some(snapshot.sig.as_slice());
            if !unchanged {
                return Err(concat(&[
                    &planned.paths.roster,
                    " (or its .sig) CHANGED ON DISK since this run read it — another \
                     join/mint/revoke, or a copy from another machine, landed in between. \
                     This run's roster was signed over the OLDER pair, so publishing it \
                     now would fork the lineage: two rosters at the same sequence \
                     de-authorize each other's machines silently. NOTHING HAS BEEN \
                     WRITTEN. Re-run this command against what the file says now; if \
                     several machines are being provisioned from one copy, serialize the \
                     joins through one machine at a time.",
                ]));
            }
        }
        None => {
            // Planned fresh (`setup`): the pair must STILL be absent, or some other run
            // started the lineage first and this one would fork it at sequence parity.
            if std::fs::metadata(&planned.paths.roster).is_ok()
                || std::fs::metadata(&sig_path).is_ok()
            {
                return Err(concat(&[
                    "a roster APPEARED at ",
                    &planned.paths.roster,
                    " while this run was in flight — this run planned the FIRST roster \
                     for a fresh master, and writing it over one that now exists would \
                     fork the lineage. NOTHING HAS BEEN WRITTEN. Work out where that \
                     roster came from before re-running.",
                ]));
            }
        }
    }
    // Defensive: preflight created this directory before any secret existed. Doing it again
    // costs a syscall and covers the caller who assembled a `Planned` without going through
    // preflight — which the library's own tests do.
    let _ = ensure_parent_dir(&planned.paths.key);
    let mut f = create_secret_file(&planned.paths.key).map_err(|e| {
        concat(&[
            "create ",
            &planned.paths.key,
            " (refusing to overwrite an existing key): ",
            &e.to_string(),
        ])
    })?;
    write_all_to(&mut f, &planned.machine_pkcs8)
        .map_err(|e| concat(&["write ", &planned.paths.key, ": ", &e.to_string()]))?;

    let mut record = String::from("id = \"");
    record.push_str(&planned.id);
    record.push_str("\"\npubkey = \"");
    record.push_str(&planned.machine_pubkey);
    record.push_str("\"\nminted_at = \"");
    record.push_str(&aterm_types::rfc3339::format_rfc3339(planned.now));
    record.push_str("\"\n");
    let _ = ensure_parent_dir(&planned.paths.machine_pub);
    let _ = write_bytes(&planned.paths.machine_pub, record.as_bytes());

    // The roster last. If it fails, the machine holds a key that no roster names —
    // recoverable, but only if the operator is told exactly what to undo. NOTE the
    // wording: `publish_roster` fails as a pair (its own error says whether the on-disk
    // roster was rolled back or is torn), so this wrapper must not claim the roster
    // "was NOT updated" — it may have been replaced and restored — and `git` can never
    // recover it, because the roster lives under gitignored `dist/`.
    publish_roster(
        &planned.paths.roster,
        &planned.roster_bytes,
        &planned.roster_sig,
    )
    .map_err(|e| {
        concat(&[
            &e,
            "\nNothing authorizes this machine yet (the message above says what state \
             the roster pair is in; if it needs restoring, copy `aterm-machines.toml` \
             and `aterm-machines.toml.sig` from another machine or from the latest \
             release's assets — git cannot restore them). This machine's key file DOES \
             exist; undo it before retrying, or the retry will mint a second key: `rm ",
            &planned.paths.key,
            "` (and `git checkout -- ",
            &planned.paths.pins,
            "` if this run armed the master anchor).",
        ])
    })?;

    let machine_is_committed_head =
        planned.channel_after.first() == Some(&planned.machine_pubkey);

    Ok(Report {
        verb: planned.verb,
        id: planned.id,
        paths: planned.paths,
        master_pubkey: planned.master_pubkey,
        master_fingerprint: planned.master_fingerprint,
        master_after: planned.master_after,
        machine_pubkey: planned.machine_pubkey,
        machine_is_committed_head,
        channel_after: planned.channel_after,
        roster_seq: planned.roster_seq,
        roster_valid_until: planned.roster_valid_until,
        roster_machines: planned.roster_machines,
        roster_was_fresh: planned.roster_was_fresh,
        seeded_head: planned.seeded_head,
        pins_changed: planned.pins_text.is_some(),
    })
}

/// The closing output: DONE, then NEXT. Owner rulings 2026-08-14/15: facts and next
/// actions only — what happened and what to do, one line each; every load-bearing
/// caveat rides as a clause on the step it guards, never as its own section. Pure so
/// tests read what the operator reads.
#[must_use]
pub fn render_report(r: &Report) -> Vec<String> {
    let mut out = Vec::new();

    out.push(String::new());
    // The commit caveat is a fact about SETUP, which is the only verb that edits the
    // working tree (the MASTER_ANCHOR append in `plan()`). A join leaves `pins_text`
    // None, writes nothing git tracks, and has nothing to commit — so it does not
    // carry the caveat, and does not send the operator to an empty `git diff`.
    out.push(if r.pins_changed {
        "=== DONE (working tree only — a commit makes it durable) ===".to_string()
    } else {
        "=== DONE ===".to_string()
    });
    if r.verb == Verb::Setup {
        out.push(concat(&[
            "  anchor  pins::PAPER_MASTER_PUBKEYS = ",
            &r.master_pubkey,
            "  fingerprint ",
            &r.master_fingerprint,
        ]));
    } else {
        out.push(concat(&[
            "  anchor  phrase verified against the committed master (",
            &r.master_fingerprint,
            ")",
        ]));
    }
    out.push(concat(&[
        "  key     ",
        &r.paths.key,
        "  0600, stays on this machine  (pub ",
        &r.machine_pubkey,
        ")",
    ]));
    let mut roster = concat(&[
        "  roster  ",
        &r.paths.roster,
        " + .sig  seq ",
        &r.roster_seq.to_string(),
        "  (",
        &r.roster_machines.join(", "),
        ")",
    ]);
    // A sentinel expiry is not news. A real one is, so it still gets its clause.
    if r.roster_valid_until != crate::roster_ops::VALID_UNTIL_FOREVER {
        roster.push_str(&concat(&["  valid until ", &r.roster_valid_until]));
    }
    if r.roster_was_fresh {
        roster.push_str(
            " — the ONLY roster this master signs; a second at the same seq forks it \
             and de-authorizes machines silently",
        );
    }
    out.push(roster);
    if let Some((head_id, head_key)) = &r.seeded_head {
        out.push(concat(&[
            "          '",
            head_id,
            "' = the incumbent keyset head (",
            head_key,
            "); rename only now, via --head-id — roster ids are revoke-only later",
        ]));
    }
    out.push(concat(&[
        "  keyset  pins::UPDATE_CHANNEL_PUBKEYS unchanged (",
        &r.channel_after.len().to_string(),
        ") — the roster authorizes '",
        &r.id,
        "'",
    ]));

    out.push(String::new());
    out.push("=== NEXT ===".to_string());
    let mut step = 0usize;
    let mut numbered = move |s: String| {
        step += 1;
        concat(&["  ", &step.to_string(), ". ", &s])
    };
    // Steps 1 and 2 are about a working-tree edit, so they exist only when there IS one.
    // Printing them on a join sends the operator to an empty diff and a no-op commit.
    if r.pins_changed {
        out.push(numbered(concat(&["review: git diff -- ", &r.paths.pins])));
    }
    if r.verb == Verb::Setup {
        out.push(numbered(
            "delete the tripwire tests that assert an empty anchor:".to_string(),
        ));
        out.push(
            "       crates/aterm-update-core/src/pins.rs::tests::\
             the_paper_master_is_unset_so_the_roster_tier_is_inert"
                .to_string(),
        );
        out.push(
            "       crates/atpkg-keys/tests/paper_master_to_client.rs::\
             the_shipped_master_anchor_is_still_empty"
                .to_string(),
        );
    }
    if r.pins_changed {
        out.push(numbered("commit — durable from here".to_string()));
    }
    if r.machine_is_committed_head {
        out.push(numbered(concat(&[
            "cut from this machine — '",
            &r.id,
            "' holds the committed keyset head, the one key pre-roster clients verify",
        ])));
    } else if let Some(head) = r.channel_after.first() {
        let mut line = concat(&["cut — from the machine holding the head key ", head]);
        if let Some((head_id, _)) = &r.seeded_head {
            line.push_str(&concat(&[
                " (roster id '",
                head_id,
                "'; its profile sets machine_id = \"",
                head_id,
                "\" — a declared id that contradicts the roster refuses the cut)",
            ]));
        }
        line.push_str(&concat(&[
            ", or from '",
            &r.id,
            "' with --strand-pre-roster-clients (asserts no pre-roster client is left \
             to strand)",
        ]));
        out.push(numbered(line));
    }
    out.push(numbered(concat(&[
        "copy ",
        &r.paths.roster,
        " + .sig to every other publishing machine — a cut from an older roster is refused",
    ])));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::master::parse_master;

    /// Obviously synthetic masters, used nowhere outside tests.
    const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";
    const OTHER_PAPER: &str = "zyxwvtsrqpnmkjhgfedcba9876543210zyxwvtsrqpnmkjhgfed0";

    /// 2026-08-04T00:00:00Z.
    const NOW: u64 = 1_785_801_600;

    /// A private scratch directory for one test, removed if it already exists.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("atpkg-keys-provision").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// A `pins.rs` fixture with the real two shapes. Deliberately a COPY: the tests write
    /// to it, and writing to the tree's own anchor file from a test would be exactly the
    /// accident this whole module is trying to make impossible.
    const PINS_FIXTURE: &str = "// Copyright 2026 Andrew Yates\n\
        // SPDX-License-Identifier: Apache-2.0\n\
        \n\
        /// The paper master. Empty here, and therefore INERT.\n\
        pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n\
        \n\
        /// The channel keyset. ORDER IS A CONTRACT: index 0 is the head.\n\
        pub const UPDATE_CHANNEL_PUBKEYS: &[&str] = &[\n\
        \x20   // K1 — HEAD, the key this build signs with.\n\
        \x20   \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
        ];\n";

    const HEAD_KEY: &str = "cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=";

    fn paths_in(dir: &std::path::Path) -> Paths {
        let s = |n: &str| dir.join(n).to_str().expect("utf-8 scratch path").to_string();
        Paths {
            pins: s("pins.rs"),
            roster: s("aterm-machines.toml"),
            key: s("machine.key"),
            machine_pub: s("machine.toml"),
            pins_explicit: true,
        }
    }

    fn write_fixture(paths: &Paths) {
        std::fs::write(&paths.pins, PINS_FIXTURE).expect("fixture pins.rs");
    }

    fn seed_of(paper: &str) -> MasterSeed {
        parse_master(paper).expect("synthetic phrase").seed()
    }

    /// Run `setup` end to end against a scratch tree, returning its report.
    fn run_setup(paths: &Paths, id: &str) -> Result<Report, String> {
        let pre = preflight(Verb::Setup, id, HEAD_ID, paths)?;
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW)?;
        write_pins(&planned)?;
        write_rest(planned)
    }

    /// Run `join` end to end, with the phrase already in hand.
    fn run_join(paths: &Paths, id: &str, paper: &str) -> Result<Report, String> {
        let pre = preflight(Verb::Join, id, HEAD_ID, paths)?;
        let seed = seed_of(paper);
        verify_master(&pre, &seed)?;
        let planned = plan(pre, &seed, NOW)?;
        write_pins(&planned)?;
        write_rest(planned)
    }

    /// The id the tests give the incumbent head.
    const HEAD_ID: &str = "incumbent-head";

    /// THE WHOLE OF `setup`, on a fresh tree: the anchor is armed, the CHANNEL KEYSET IS
    /// NOT TOUCHED, the roster names the machine and verifies under the master, and the
    /// secret key lands 0600.
    ///
    /// Kills the mutation "append the machine key to the keyset as a bridge": the keyset
    /// assertion below then fails. A keyset entry is an irrevocable grant to every client
    /// that ships with it, and it authorizes nothing that the roster does not already.
    #[test]
    fn setup_arms_the_anchor_mints_the_machine_and_signs_the_roster() {
        let dir = scratch("setup-happy");
        let paths = paths_in(&dir);
        write_fixture(&paths);

        let report = run_setup(&paths, "m3").expect("setup completes");

        // The anchor file, read back from disk.
        let src = std::fs::read_to_string(&paths.pins).unwrap();
        let master = read_anchor(&src, MASTER_ANCHOR).unwrap();
        assert_eq!(master.members, vec![report.master_pubkey.clone()]);
        assert_eq!(
            master.members[0],
            seed_of(PAPER).pubkey_b64().unwrap(),
            "the armed anchor is the master the phrase derives"
        );
        let channel = read_anchor(&src, CHANNEL_ANCHOR).unwrap();
        assert_eq!(
            channel.members,
            vec![HEAD_KEY.to_string()],
            "UNTOUCHED: the machine key belongs on the roster, never in the keyset"
        );
        assert_eq!(channel.head(), Some(HEAD_KEY), "the head is not reordered");
        assert!(
            !report.machine_is_committed_head,
            "this tool never grants a machine the pre-roster allowance"
        );
        assert!(
            !src.contains(report.machine_pubkey.as_str()),
            "the minted key must appear nowhere in the anchor file"
        );

        // The roster verifies under the master and names the machine.
        let bytes = std::fs::read(&paths.roster).unwrap();
        let sig = std::fs::read(concat(&[&paths.roster, ".sig"])).unwrap();
        let verified = aterm_update_core::roster::verify_roster(
            &[report.master_pubkey.as_str()],
            bytes,
            &sig,
        )
        .expect("the roster verifies under the armed master");
        let roster = Roster::parse(&verified).unwrap();
        // THE FIRST ROSTER NAMES THE INCUMBENT HEAD FIRST. Without this entry, committing
        // the anchor would leave the one machine every shipped client can verify unable to
        // cut — `authorize_cut` looks the signing key up in the roster by public key.
        assert_eq!(roster.machines.len(), 2);
        assert_eq!(roster.machines[0].id, HEAD_ID);
        assert_eq!(
            roster.machines[0].pubkey, HEAD_KEY,
            "the incumbent channel head is the roster's first machine"
        );
        assert_eq!(roster.machines[1].id, "m3");
        assert_eq!(roster.machines[1].pubkey, report.machine_pubkey);
        assert_eq!(roster.roster_seq, 2, "one bump per machine added");
        assert_eq!(
            report.seeded_head,
            Some((HEAD_ID.to_string(), HEAD_KEY.to_string())),
            "the report carries the seeding so it can be spoken out loud"
        );

        // The secret key is 0600 and holds a usable pkcs8 key.
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&paths.key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the machine key must be owner-only");
        let pkcs8 = std::fs::read(&paths.key).unwrap();
        assert_eq!(crate::pubkey_b64(&pkcs8).unwrap(), report.machine_pubkey);
    }

    /// `setup` REFUSES when an anchor is already committed, and it refuses BEFORE
    /// generating anything — the point being that a second master would strand the first,
    /// which exists only on paper.
    #[test]
    fn setup_refuses_when_a_master_is_already_committed() {
        let dir = scratch("setup-refuses");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("the first setup succeeds");

        // Second run: the anchor is armed now.
        let before = std::fs::read_to_string(&paths.pins).unwrap();
        let err = preflight(Verb::Setup, "m11", "incumbent-head", &paths).unwrap_err();
        assert!(err.contains("ALREADY committed"), "{err}");
        assert!(err.contains("join"), "{err}");
        assert!(err.contains("strand"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&paths.pins).unwrap(),
            before,
            "a refused setup writes nothing"
        );

        // NEGATIVE CONTROL: the same call against an unarmed tree is accepted, so the
        // refusal is about the armed anchor and not about the arguments.
        let fresh = scratch("setup-refuses-control");
        let fresh_paths = paths_in(&fresh);
        write_fixture(&fresh_paths);
        assert!(preflight(Verb::Setup, "m11", "incumbent-head", &fresh_paths).is_ok());
    }

    /// `join` on a later machine: the master is proved, the machine is added to the
    /// ROSTER, and NEITHER anchor is touched — `join` edits no trust anchor at all.
    #[test]
    fn join_proves_the_master_and_adds_a_second_machine() {
        let dir = scratch("join-happy");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let first = run_setup(&paths, "m3").expect("setup");

        // A second machine, with its own key file, in the same tree.
        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

        let pre = preflight(Verb::Join, "m11", "incumbent-head", &second).expect("join preflight");
        let seed = seed_of(PAPER);
        verify_master(&pre, &seed).expect("the phrase matches the committed anchor");
        let planned = plan(pre, &seed, NOW).expect("join plans");
        write_pins(&planned).expect("pins written");
        let report = write_rest(planned).expect("join completes");

        let src = std::fs::read_to_string(&paths.pins).unwrap();
        assert_eq!(
            read_anchor(&src, MASTER_ANCHOR).unwrap().members,
            vec![first.master_pubkey.clone()],
            "join never touches the master anchor"
        );
        assert_eq!(
            read_anchor(&src, CHANNEL_ANCHOR).unwrap().members,
            vec![HEAD_KEY.to_string()],
            "join grants no pre-roster allowance: the keyset is exactly as committed"
        );
        assert!(
            !src.contains(first.machine_pubkey.as_str())
                && !src.contains(report.machine_pubkey.as_str()),
            "neither minted key appears in the anchor file"
        );
        assert!(!report.machine_is_committed_head);
        assert_eq!(report.roster_seq, 3, "the roster advanced");
        assert_eq!(report.roster_machines, vec![HEAD_ID, "m3", "m11"]);
        assert!(!report.roster_was_fresh, "join edited the existing roster");
        assert_eq!(
            report.seeded_head, None,
            "only a FRESH roster is seeded; join extends the one it was given"
        );
    }

    /// `join` WITH NO ROSTER ON DISK REFUSES RATHER THAN STARTING A SECOND ONE.
    ///
    /// This is the default state of the second machine, not an edge case: the roster lives
    /// under `dist/`, which is gitignored, so a fresh checkout has the committed anchor and
    /// no roster. Treating that as the first mint produced a BRAND-NEW roster at sequence 1
    /// naming only the joining machine — master-signed, therefore valid — beside the real
    /// one somewhere else. Same sequence, disjoint machine lists: publishing either
    /// de-authorizes every machine on the other, and a same-sequence fork is invisible to
    /// the monotonic ratchet, so nothing downstream catches it.
    #[test]
    fn join_refuses_a_missing_roster_instead_of_forking_a_second_one() {
        let dir = scratch("join-no-roster");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup, on the first machine");

        // Machine #2: the same committed anchor, and no roster — exactly what `git clone`
        // produces.
        let dir2 = scratch("join-no-roster-second");
        let second = paths_in(&dir2);
        std::fs::copy(&paths.pins, &second.pins).expect("the anchor travels with the repo");
        assert!(
            !std::path::Path::new(&second.roster).exists(),
            "the roster does NOT travel with the repo; that is the premise"
        );

        let err = preflight(Verb::Join, "m11", HEAD_ID, &second).unwrap_err();
        assert!(err.contains("no roster at"), "{err}");
        assert!(err.contains("COPY"), "{err}");
        assert!(err.contains("will not start a second roster"), "{err}");
        assert!(
            !std::path::Path::new(&second.roster).exists(),
            "a refused join creates nothing"
        );
        assert!(!std::path::Path::new(&second.key).exists());

        // NEGATIVE CONTROL: copy the roster across — the real one, with its signature — and
        // the identical call is accepted. So the refusal is about the missing file.
        std::fs::copy(&paths.roster, &second.roster).unwrap();
        std::fs::copy(
            concat(&[&paths.roster, ".sig"]),
            concat(&[&second.roster, ".sig"]),
        )
        .unwrap();
        let report = run_join(&second, "m11", PAPER).expect("join, with the roster in hand");
        assert_eq!(
            report.roster_machines,
            vec![HEAD_ID, "m3", "m11"],
            "the roster was EXTENDED: every machine already on it survives"
        );
        assert!(!report.roster_was_fresh);
    }

    /// THE PREFLIGHT REFUSAL IS NOT THE ENFORCEMENT: a roster that vanishes BETWEEN
    /// preflight and plan — the operator is typing 64 characters, and the pair lives
    /// under gitignored `dist/`, where a concurrent cleanup can sweep both files — must
    /// be refused at the point of use, not sailed into the fresh-mint branch. Before
    /// this held, exactly that window minted a second same-sequence master-signed
    /// roster: a fork that de-authorizes every machine on the other copy.
    ///
    /// MUTATION: pass `MayCreateFresh` for `join` in `plan` (the pre-fix behavior) and
    /// this fails at `unwrap_err` — the plan happily forks.
    #[test]
    fn join_refuses_a_roster_that_vanished_between_preflight_and_plan() {
        let dir = scratch("join-vanished-roster");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

        // Preflight sees the roster and passes — the courteous check.
        let pre = preflight(Verb::Join, "m11", HEAD_ID, &second).expect("roster is present");
        // THE WINDOW: both halves of the pair disappear while the phrase is typed.
        // (Held in memory first, so the negative control below can put them back.)
        let saved_body = std::fs::read(&second.roster).unwrap();
        let saved_sig = std::fs::read(concat(&[&second.roster, ".sig"])).unwrap();
        std::fs::remove_file(&second.roster).unwrap();
        std::fs::remove_file(concat(&[&second.roster, ".sig"])).unwrap();

        let seed = seed_of(PAPER);
        verify_master(&pre, &seed).expect("the phrase itself is right");
        let err = plan(pre, &seed, NOW).unwrap_err();
        assert!(err.contains("no roster at"), "{err}");
        assert!(err.contains("second roster"), "{err}");
        assert!(
            !std::path::Path::new(&second.roster).exists()
                && !std::path::Path::new(&second.key).exists(),
            "a refused plan writes nothing and forks nothing"
        );

        // NEGATIVE CONTROL: with the pair restored, the identical sequence completes —
        // so the refusal above is about the vanished roster, not the fixture.
        std::fs::write(&second.roster, &saved_body).unwrap();
        std::fs::write(concat(&[&second.roster, ".sig"]), &saved_sig).unwrap();
        let report = run_join(&second, "m11", PAPER).expect("join with the pair in place");
        assert_eq!(report.roster_machines, vec![HEAD_ID, "m3", "m11"]);
    }

    /// THE ROSTER'S OWN PREMISE CHECK: `write_rest` refuses to publish a roster signed
    /// over a pair that has since MOVED on disk — a concurrent join/mint/revoke or a
    /// hand-copy landing between `plan` and the write would otherwise be silently
    /// overwritten by a same-sequence sibling, de-authorizing whatever the other run
    /// added. The refusal happens before ANY write: no key, no roster.
    ///
    /// MUTATION: delete the snapshot comparison at the top of `write_rest` and the
    /// `unwrap_err` below becomes a successful (forking) publish.
    #[test]
    fn write_rest_refuses_a_roster_that_changed_since_the_plan_read_it() {
        let dir = scratch("roster-premise");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

        let pre = preflight(Verb::Join, "m11", HEAD_ID, &second).expect("preflight");
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW).expect("plan against the current pair");

        // A concurrent actor replaces the pair while this run holds its plan. (Another
        // join from the same base sequence is the realistic shape; any byte change is
        // the same premise violation.)
        let mut moved = std::fs::read(&paths.roster).unwrap();
        moved.extend_from_slice(b"\n# concurrently rewritten\n");
        std::fs::write(&paths.roster, &moved).unwrap();

        let err = write_rest(planned).unwrap_err();
        assert!(err.contains("CHANGED ON DISK"), "{err}");
        assert!(err.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
        assert!(
            !std::path::Path::new(&second.key).exists(),
            "refused before the key write — the premise check must run FIRST"
        );
        assert_eq!(
            std::fs::read(&paths.roster).unwrap(),
            moved,
            "the concurrent actor's pair survives untouched"
        );

        // And the FRESH-plan premise: `setup` planned the first roster ever, so a pair
        // that APPEARS mid-run is a lineage this run must not overwrite.
        let fresh_dir = scratch("roster-premise-fresh");
        let fresh_paths = paths_in(&fresh_dir);
        write_fixture(&fresh_paths);
        let pre = preflight(Verb::Setup, "m3", HEAD_ID, &fresh_paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        write_pins(&planned).unwrap();
        std::fs::write(&fresh_paths.roster, b"someone else's first roster").unwrap();
        let err = write_rest(planned).unwrap_err();
        assert!(err.contains("APPEARED"), "{err}");
        assert!(
            !std::path::Path::new(&fresh_paths.key).exists(),
            "nothing written on the fresh-collision refusal either"
        );

        // NEGATIVE CONTROL: an untouched pair publishes — so both refusals above are
        // the premise checks and not a broken pipeline.
        let control_dir = scratch("roster-premise-control");
        let control = paths_in(&control_dir);
        write_fixture(&control);
        run_setup(&control, "m3").expect("the same pipeline, unraced, completes");
    }

    /// A SIGNATURE THAT CANNOT PUBLISH ROLLS THE DOCUMENT BACK. Per-file atomicity
    /// cannot make the PAIR consistent: before this held, a failure between the two
    /// renames left the NEW document beside the OLD signature — refused by every
    /// verifier as "signature did not verify" and misdiagnosed as a mistyped phrase,
    /// on a file `git checkout` cannot restore (dist/ is gitignored).
    ///
    /// MUTATION: revert `publish_roster` to two sequential `write_bytes_atomic` calls
    /// and the roll-back assertion fails — the new body survives beside the stale sig.
    #[test]
    fn a_failed_signature_publish_rolls_the_roster_document_back() {
        let dir = scratch("pair-rollback");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup publishes the first pair");
        let sig_path = concat(&[&paths.roster, ".sig"]);
        let old_body = std::fs::read(&paths.roster).unwrap();
        let old_sig = std::fs::read(&sig_path).unwrap();

        // Block the SIGNATURE promotion and nothing else: a directory squats on the
        // sig path, so its staging sibling writes fine and the rename fails.
        std::fs::remove_file(&sig_path).unwrap();
        std::fs::create_dir(&sig_path).unwrap();
        let err = publish_roster(&paths.roster, b"NEW BODY", b"NEW SIG").unwrap_err();
        assert!(err.contains("ROLLED BACK"), "{err}");
        assert_eq!(
            std::fs::read(&paths.roster).unwrap(),
            old_body,
            "the document must be rolled back to the pair's previous bytes"
        );
        std::fs::remove_dir(&sig_path).unwrap();
        std::fs::write(&sig_path, &old_sig).unwrap();

        // The restored pair still verifies under the master — the operator is NOT sent
        // chasing a "wrong phrase" diagnosis.
        let master_pub = seed_of(PAPER).pubkey_b64().unwrap();
        assert!(
            load_roster(&paths.roster, &master_pub, NOW, RosterExpectation::MustExist)
                .is_ok(),
            "the pair left behind by a failed publish must be the consistent old one"
        );

        // A FIRST publish that fails the same way retracts the document instead: no
        // half-pair is left to misdiagnose.
        let fresh = scratch("pair-rollback-fresh");
        let fresh_roster = fresh.join("aterm-machines.toml").to_str().unwrap().to_string();
        let fresh_sig = concat(&[&fresh_roster, ".sig"]);
        std::fs::create_dir(&fresh_sig).unwrap();
        let err = publish_roster(&fresh_roster, b"BODY", b"SIG").unwrap_err();
        assert!(err.contains("ROLLED BACK"), "{err}");
        assert!(
            !std::path::Path::new(&fresh_roster).exists(),
            "a first publish that failed leaves nothing, not a signatureless document"
        );

        // NEGATIVE CONTROL: unblocked, the same publish replaces the pair whole.
        std::fs::remove_dir(&fresh_sig).unwrap();
        publish_roster(&fresh_roster, b"BODY", b"SIG").expect("publish with nothing in the way");
        assert_eq!(std::fs::read(&fresh_roster).unwrap(), b"BODY");
        assert_eq!(std::fs::read(&fresh_sig).unwrap(), b"SIG");
    }

    /// `plan` RE-PROVES THE ANCHOR FOR `join` — the orchestrator's `verify_master` call
    /// is courtesy, not the enforcement. The attack this closes: skip `verify_master`
    /// and hand `plan` a substituted `--roster` signed by the WRONG master; the roster
    /// proof alone would pass (the document does verify under the phrase that was
    /// typed), and the run would publish under a master the committed anchor never
    /// named.
    ///
    /// MUTATION: remove the `Verb::Join` `verify_master` call inside `plan` and the
    /// `unwrap_err` below becomes a successful plan.
    #[test]
    fn plan_reproves_the_anchor_even_when_the_orchestrator_skips_verify_master() {
        // The victim tree: armed with PAPER's master.
        let dir = scratch("plan-anchor-reproof");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        // The attacker's parallel lineage: a different tree, set up under OTHER_PAPER,
        // yielding a roster that VERIFIES under the wrong master.
        let attacker_dir = scratch("plan-anchor-reproof-attacker");
        let attacker_paths = paths_in(&attacker_dir);
        write_fixture(&attacker_paths);
        let pre = preflight(Verb::Setup, "mx", HEAD_ID, &attacker_paths).unwrap();
        let wrong = seed_of(OTHER_PAPER);
        let planned = plan(pre, &wrong, NOW).unwrap();
        write_pins(&planned).unwrap();
        write_rest(planned).expect("the attacker's own lineage sets up fine");

        // The join, orchestrated WITHOUT verify_master and with the substituted roster.
        let mut second = paths.clone();
        second.roster = attacker_paths.roster.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();
        let pre = preflight(Verb::Join, "m11", HEAD_ID, &second).expect("roster file exists");
        let err = plan(pre, &wrong, NOW).unwrap_err();
        assert!(
            err.contains("is NOT the master committed"),
            "the anchor containment must refuse inside plan itself: {err}"
        );
        assert!(!std::path::Path::new(&second.key).exists());

        // NEGATIVE CONTROL: the committed master with its own roster plans fine without
        // any prior verify_master call — the re-proof refuses wrong masters, not joins.
        let mut honest = paths.clone();
        honest.key = dir.join("m11b.key").to_str().unwrap().to_string();
        honest.machine_pub = dir.join("m11b.toml").to_str().unwrap().to_string();
        let pre = preflight(Verb::Join, "m11", HEAD_ID, &honest).unwrap();
        assert!(plan(pre, &seed_of(PAPER), NOW).is_ok());
    }

    /// THE MACHINE KEY'S DIRECTORY IS CREATED BEFORE ANY SECRET EXISTS.
    ///
    /// `$HOME/.aterm` does not exist on a fresh machine — the first machine, the one
    /// `setup` is for. Creating the key there used to fail ENOENT at the very last step,
    /// after the anchor had been armed, the phrase shown and a master-signed roster
    /// published naming a machine whose private key was discarded with the process.
    #[test]
    fn a_run_creates_the_directories_its_key_and_roster_need() {
        let dir = scratch("missing-dirs");
        let paths = Paths {
            pins: dir.join("pins.rs").to_str().unwrap().to_string(),
            // Every one of these sits under a directory that does not exist yet.
            roster: dir.join("dist/aterm-machines.toml").to_str().unwrap().to_string(),
            key: dir.join("home/.aterm/machine.key").to_str().unwrap().to_string(),
            machine_pub: dir.join("home/.aterm/machine.toml").to_str().unwrap().to_string(),
            pins_explicit: true,
        };
        write_fixture(&paths);
        assert!(!dir.join("home/.aterm").exists(), "the premise: no $HOME/.aterm");

        let report = run_setup(&paths, "m3").expect("setup completes on a fresh machine");

        // The key really is there, 0600, and really is the key the roster names.
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&paths.key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            crate::pubkey_b64(&std::fs::read(&paths.key).unwrap()).unwrap(),
            report.machine_pubkey,
            "the roster names a key whose private half exists"
        );
        assert!(std::path::Path::new(&paths.roster).exists());
        assert!(std::path::Path::new(&paths.machine_pub).exists());
    }

    /// THE SECRET KEY EXISTS BEFORE ANY SIGNED DOCUMENT NAMES IT.
    ///
    /// Driven by making the roster's directory unwritable AFTER the run has passed
    /// preflight, which is the only way to fail that one step in isolation. The point is
    /// the STATE afterwards — the machine holds a key nothing has authorized, which is
    /// recoverable, rather than a master-signed roster naming a machine whose key was never
    /// written, which asserts something false.
    #[test]
    fn the_key_is_written_before_the_roster_that_names_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("key-before-roster");
        let mut paths = paths_in(&dir);
        let roster_dir = dir.join("dist");
        paths.roster = roster_dir.join("r.toml").to_str().unwrap().to_string();
        write_fixture(&paths);

        let pre = preflight(Verb::Setup, "m3", HEAD_ID, &paths).expect("preflight");
        let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
        write_pins(&planned).expect("the anchor is written");
        // Preflight created this directory; take the write permission away now, so the
        // roster publish is the one step that fails.
        std::fs::set_permissions(&roster_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_rest(planned);
        std::fs::set_permissions(&roster_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.unwrap_err();
        assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
        assert!(err.contains("git checkout --"), "the recovery names both files: {err}");
        assert!(err.contains("rm "), "{err}");
        assert!(
            std::path::Path::new(&paths.key).exists(),
            "the key was written first, which is what makes the recovery two commands"
        );
        assert!(
            !std::path::Path::new(&paths.roster).exists()
                && !std::path::Path::new(&concat(&[&paths.roster, ".sig"])).exists(),
            "neither half of a pair that could not publish exists on disk"
        );
    }

    /// THE ANCHOR FILE IS NOT REPLACED BY A SNAPSHOT SOMEBODY ELSE HAS SINCE EDITED.
    ///
    /// The plan is computed from bytes read in `preflight`, and for `join` that read happens
    /// before the human types 64 characters. A `git checkout`, a rebase, an editor or
    /// another agent landing in that window used to be silently reverted by the write —
    /// including, in the worst case, a reviewed commit retiring a compromised key, which
    /// would be resurrected into the keyset by a run that reported success.
    #[test]
    fn write_pins_refuses_a_file_that_changed_under_it() {
        let dir = scratch("stale-snapshot");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", HEAD_ID, &paths).unwrap();

        // A reviewed commit lands while the run is in flight: the head key is retired and
        // replaced.
        const REPLACEMENT: &str = "bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=";
        let concurrent = PINS_FIXTURE.replace(HEAD_KEY, REPLACEMENT);
        assert_ne!(concurrent, PINS_FIXTURE, "the fixture must actually differ");
        std::fs::write(&paths.pins, &concurrent).unwrap();

        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        let err = write_pins(&planned).unwrap_err();
        assert!(err.contains("CHANGED ON DISK"), "{err}");
        assert!(err.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&paths.pins).unwrap(),
            concurrent,
            "the concurrent edit survives, and the retired key stays retired"
        );

        // NEGATIVE CONTROL: plan against what the file says NOW, and the write is accepted.
        let pre = preflight(Verb::Setup, "m3", HEAD_ID, &paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        write_pins(&planned).expect("a plan built from the current bytes writes");
        let after = std::fs::read_to_string(&paths.pins).unwrap();
        assert!(after.contains(REPLACEMENT), "{after}");
        assert!(!after.contains(HEAD_KEY), "the retired key was not resurrected");
    }

    /// JOIN REFUSES THE WRONG PAPER — twice over, and before writing anything. The anchor
    /// check catches it first; the roster verification would catch it too.
    #[test]
    fn join_refuses_a_phrase_that_does_not_match_the_committed_anchor() {
        let dir = scratch("join-wrong-paper");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

        let before = std::fs::read_to_string(&paths.pins).unwrap();
        let roster_before = std::fs::read(&paths.roster).unwrap();

        let pre = preflight(Verb::Join, "m11", "incumbent-head", &second).expect("preflight is fine");
        let wrong = seed_of(OTHER_PAPER);
        let err = verify_master(&pre, &wrong).unwrap_err();
        assert!(err.contains("is NOT the master committed"), "{err}");
        assert!(err.contains("Nothing has been written"), "{err}");

        // `plan` re-runs the anchor proof ITSELF — the enforcement, not the courtesy —
        // so an orchestrator that skipped `verify_master` is refused all the same.
        let err = plan(pre, &wrong, NOW).unwrap_err();
        assert!(err.contains("is NOT the master committed"), "{err}");

        // The second, independent check is still independently there: a caller that
        // somehow reached the roster with the wrong master cannot verify the document
        // the real master signed.
        let err = load_roster(
            &paths.roster,
            &wrong.pubkey_b64().unwrap(),
            NOW,
            RosterExpectation::MustExist,
            )
        .unwrap_err();
        assert!(err.contains("does not verify under the master you typed"), "{err}");

        assert_eq!(std::fs::read_to_string(&paths.pins).unwrap(), before);
        assert_eq!(std::fs::read(&paths.roster).unwrap(), roster_before);
        assert!(!std::path::Path::new(&second.key).exists(), "no key was minted");

        // NEGATIVE CONTROL: the RIGHT paper is accepted on the same inputs.
        let pre = preflight(Verb::Join, "m11", "incumbent-head", &second).unwrap();
        assert!(verify_master(&pre, &seed_of(PAPER)).is_ok());
    }

    /// `join` refuses when nothing is committed to prove a phrase against — the
    /// counterpart of `setup`'s refusal, so neither verb can be used where the other
    /// belongs.
    #[test]
    fn join_refuses_when_no_master_is_committed() {
        let dir = scratch("join-unarmed");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let err = preflight(Verb::Join, "m3", "incumbent-head", &paths).unwrap_err();
        assert!(err.contains("no paper master is committed"), "{err}");
        assert!(err.contains("setup"), "{err}");
        // Negative control: setup IS allowed here.
        assert!(preflight(Verb::Setup, "m3", "incumbent-head", &paths).is_ok());
    }

    /// IDEMPOTENCE AT THE VERB LEVEL: re-arming the SAME master is a no-op rather than a
    /// second entry. (The channel keyset is no longer written by either verb, so the
    /// append that used to be exercised here is the master's.)
    #[test]
    fn a_second_pass_over_an_already_listed_key_adds_no_second_entry() {
        let dir = scratch("idempotent-verb");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let report = run_setup(&paths, "m3").expect("setup");

        let src = std::fs::read_to_string(&paths.pins).unwrap();
        let edit = append_member(
            &src,
            MASTER_ANCHOR,
            &report.master_pubkey,
            &["a re-run"],
            MAX_MASTER_MEMBERS,
        )
        .unwrap();
        assert!(
            matches!(edit, Edit::AlreadyPresent { .. }),
            "the master is already there; a re-run must be a no-op"
        );
        assert_eq!(
            src.matches(report.master_pubkey.as_str()).count(),
            1,
            "exactly one entry for this master"
        );
        // And the machine key is in the anchor file NOWHERE, under either verb.
        assert!(!src.contains(report.machine_pubkey.as_str()));
    }

    /// A re-run of `setup` with the SAME id is refused for a second reason too: the key
    /// file already exists, which is checked before anything is generated.
    #[test]
    fn an_existing_machine_key_is_refused_before_anything_is_generated() {
        let dir = scratch("existing-key");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");
        let err = preflight(Verb::Join, "m3", "incumbent-head", &paths).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(err.contains("never overwrite a key the roster still names"), "{err}");
    }

    /// `setup` will not write over a roster some other master signed.
    #[test]
    fn setup_refuses_to_start_over_an_existing_roster() {
        let dir = scratch("setup-roster-exists");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        std::fs::write(&paths.roster, b"schema = 1\n").unwrap();
        let err = preflight(Verb::Setup, "m3", "incumbent-head", &paths).unwrap_err();
        assert!(err.contains("roster already exists"), "{err}");
    }

    /// A FULL KEYSET IS NO LONGER AN OBSTACLE, because provisioning does not need a slot
    /// in it. This used to refuse at preflight; refusing now would block a machine from
    /// being minted over a resource it never consumes.
    ///
    /// Kills the mutation "put the keyset room check back": a fleet whose rotation window
    /// happens to be full could not add a publishing machine at all, which is exactly the
    /// coupling this change removes.
    #[test]
    fn a_full_keyset_is_no_longer_an_obstacle_to_provisioning() {
        let dir = scratch("full-keyset");
        let paths = paths_in(&dir);
        let full = PINS_FIXTURE.replace(
            "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n",
            "    \"cw5gIGYQzX6xrhTXjXU9nYfLWeoIkiZ1yUX7d1wmdz8=\",\n\
             \x20   \"bsuawZEJq6qhEpcUovJCFFfMXgp7AgLZHjPvd14qNdc=\",\n\
             \x20   \"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\",\n\
             \x20   \"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=\",\n",
        );
        std::fs::write(&paths.pins, &full).unwrap();
        preflight(Verb::Setup, "m3", "incumbent-head", &paths)
            .expect("a full rotation window says nothing about minting a machine key");
        let report = run_setup(&paths, "m3").expect("setup completes over a full keyset");
        let src = std::fs::read_to_string(&paths.pins).unwrap();
        assert_eq!(
            read_anchor(&src, CHANNEL_ANCHOR).unwrap().members.len(),
            4,
            "and the keyset is still exactly what was committed"
        );
        assert!(!src.contains(report.machine_pubkey.as_str()));
    }

    /// A pins file this writer does not recognise stops the run at preflight rather than
    /// being edited on a guess.
    #[test]
    fn an_unrecognised_pins_file_stops_the_run_at_preflight() {
        let dir = scratch("bad-pins");
        let paths = paths_in(&dir);
        std::fs::write(&paths.pins, "// nothing resembling an anchor\n").unwrap();
        let err = preflight(Verb::Setup, "m3", "incumbent-head", &paths).unwrap_err();
        assert!(err.contains("will not guess"), "{err}");
    }

    /// WRITE VERIFICATION IS REAL: if the anchor file is replaced behind the tool's back
    /// between planning and verifying, `write_pins` reports failure instead of returning
    /// success over a file that does not hold the value.
    #[test]
    fn write_pins_reports_a_write_that_did_not_land() {
        let dir = scratch("verify-write");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", "incumbent-head", &paths).unwrap();
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW).unwrap();

        // Honest first: the ordinary path succeeds.
        write_pins(&planned).expect("the ordinary write verifies");

        // MUTATION: put the original, unarmed file back and verify again. Nothing about
        // the plan changed, so a passing result here would prove the check is decorative.
        std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();
        let planned2 = {
            let pre = preflight(Verb::Setup, "m3-b", "incumbent-head", &Paths {
                key: dir.join("m3b.key").to_str().unwrap().to_string(),
                ..paths.clone()
            })
            .unwrap();
            plan(pre, &seed, NOW).unwrap()
        };
        // Write the planned text, then clobber it with the unarmed original and re-verify
        // through the same function: it must refuse.
        write_pins(&planned2).expect("write");
        std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();
        let mut no_write = planned2;
        no_write.pins_text = None; // "already correct" — the idempotent path
        let err = write_pins(&no_write).unwrap_err();
        assert!(
            err.contains("holds 0 keys but 1 were intended")
                || err.contains("holds 1 keys but 2 were intended"),
            "{err}"
        );
    }

    /// THE CLOSING OUTPUT SAYS THE THREE THINGS IT MUST SAY: what is true, what is not,
    /// and the next action — including the bridge order and the no-commit rule.
    #[test]
    fn the_report_states_the_bridge_order_and_that_nothing_is_committed() {
        let dir = scratch("report");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let report = run_setup(&paths, "m3").expect("setup");
        let text = render_report(&report).join("\n");

        assert!(text.contains("=== DONE (working tree only — a commit makes it durable) ==="), "{text}");
        
        assert!(text.contains("=== NEXT ==="), "{text}");
        assert!(text.contains("commit — durable from here"), "{text}");
        // The two halves of the truth, both required: who this machine reaches, and who
        // it can never reach. A report that printed only one of them would send an
        // operator either to an unnecessary release or into a wedged fleet.
        assert!(text.contains("the ONLY roster this master signs"), "{text}");
        assert!(text.contains("no pre-roster client is left"), "{text}");
        assert!(text.contains("--strand-pre-roster-clients"), "{text}");
        assert!(
            text.contains("UPDATE_CHANNEL_PUBKEYS unchanged"),
            "the report must say the keyset was not touched: {text}"
        );
        assert!(text.contains(HEAD_KEY), "the head key is named: {text}");
        assert!(text.contains(&report.machine_pubkey), "{text}");
        assert!(text.contains("git diff -- "), "{text}");
        assert!(
            text.contains("the_paper_master_is_unset_so_the_roster_tier_is_inert"),
            "setup must name the tripwires it just broke: {text}"
        );

        // THE SAFE PATH IS SPELLED OUT IN FULL, not left one refusal short: the cut from
        // the incumbent must name the profile line it needs, because a declared id that
        // contradicts the roster refuses the cut — and an operator who finds that out
        // from the refusal is one step from reaching for --strand-pre-roster-clients.
        assert!(
            text.contains("machine_id = \"incumbent-head\""),
            "the safe path must name the profile line it needs: {text}"
        );
        assert!(
            text.contains("contradicts the roster refuses the cut"),
            "and say why it is needed, or it reads as boilerplate: {text}"
        );

        // THE SECRET IS NOT IN THE OUTPUT. The report is built from public identities
        // only, and this asserts it rather than trusting the type system to have.
        assert!(!text.contains(PAPER), "{text}");
        assert!(!text.contains(&PAPER[..16]), "{text}");
    }

    /// NO SECRET REACHES A `Debug` RENDERING. `Planned` carries the machine's pkcs8 key
    /// and travels beside a `MasterSeed`; a derived `Debug` would print both into any
    /// panic message that formatted them.
    #[test]
    fn debug_renderings_carry_no_secret_material() {
        let dir = scratch("debug-redaction");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", "incumbent-head", &paths).unwrap();
        assert_eq!(format!("{pre:?}"), "Preflight(setup, m3)");
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW).unwrap();
        let rendered = format!("{planned:?}");
        assert_eq!(rendered, "Planned(setup, m3, secret=<redacted>)");
        assert!(!rendered.contains(PAPER), "{rendered}");
        assert_eq!(format!("{seed:?}"), "MasterSeed(<redacted>)");
    }

    /// THE PHRASE NEVER REACHES A FILE. Every byte this run wrote is scanned for the
    /// phrase, for its raw 32-byte seed, and for any 16-character run of either.
    #[test]
    fn no_file_written_by_a_run_contains_the_phrase_or_its_seed() {
        let dir = scratch("no-phrase-on-disk");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        // The raw seed bytes the phrase decodes to, built here by an independent base32
        // decode rather than read out of the (deliberately accessor-less) MasterSeed.
        const ALPHABET: &[u8] = b"0123456789abcdefghjkmnpqrstvwxyz";
        let mut raw = Vec::new();
        let mut acc: u32 = 0;
        let mut nbits = 0u32;
        for &c in PAPER.as_bytes() {
            let v = ALPHABET
                .iter()
                .position(|a| *a == c)
                .expect("fixture in alphabet") as u32;
            acc = (acc << 5) | v;
            nbits += 5;
            if nbits >= 8 {
                nbits -= 8;
                raw.push(((acc >> nbits) & 0xff) as u8);
            }
        }
        assert_eq!(raw.len(), 32);

        let mut checked = 0usize;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() {
                continue;
            }
            checked += 1;
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(PAPER), "{path:?} contains the phrase");
            for start in 0..(PAPER.len() - 16) {
                assert!(
                    !text.contains(&PAPER[start..start + 16]),
                    "{path:?} contains a 16-character run of the phrase"
                );
            }
            assert!(
                !bytes.windows(32).any(|w| w == raw.as_slice()),
                "{path:?} contains the raw master seed"
            );
        }
        assert!(
            checked >= 4,
            "the run must have written pins.rs, the roster, its .sig and the key; saw {checked}"
        );
    }
}
