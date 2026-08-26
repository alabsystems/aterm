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
//! # THE ROSTER IS A PAIR, AND A CRASH USED TO BE ABLE TO TEAR IT
//!
//! Phase 3's roster half is not one write but two — `aterm-machines.toml` and its
//! detached `.sig`, meaningful only together — so it gets its own machinery: one
//! crash-released advisory lock every writer takes ([`lock_roster`]) and a durable redo
//! transaction under it ([`publish_roster_locked`]). The section comment above that layer
//! is where its reasoning lives.
//!
//! # It does not commit, and it does not push
//!
//! Deliberately. Arming a trust anchor is a reviewed act, so the tool edits the working
//! tree and tells the operator to read the diff. [`render_report`] says so in the output.

use crate::fsio::{
    concat, ensure_parent_dir, read_bytes, sync_parent, write_bytes, write_bytes_atomic,
    write_owner_file_create_new,
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
    // BEFORE the pair is read at all: while a transaction is pending, the two files on
    // disk may be a torn pair, and the failure that surfaces from reading one is
    // "signature did not verify" — the mistyped-phrase diagnosis this whole layer exists
    // to stop an operator chasing.
    refuse_pending_roster_transaction(path)?;
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

// ---------------------------------------------------------------------------
// THE ROSTER PAIR'S CRASH-SAFE TRANSACTION LAYER
// ---------------------------------------------------------------------------
//
// The roster is TWO files — `aterm-machines.toml` and its detached
// `aterm-machines.toml.sig` — that are only meaningful together. Per-file atomicity
// (stage + fsync + rename, see [`crate::fsio::write_bytes_atomic`]) makes each FILE
// whole; it cannot make the PAIR consistent, because there is no syscall that renames
// two names at once. Staging both before promoting either narrows the window to the two
// renames, and an error path between them can roll the document back — but a PROCESS
// DEATH or a power loss cannot run an error path. The state it leaves is the new
// document beside the old signature: refused by every client as "signature did not
// verify", misdiagnosed by every operator as a mistyped phrase, on a file `git checkout`
// cannot restore because `dist/` is gitignored.
//
// So the pair is published through a REDO LOG instead. The complete new pair — and the
// exact predecessor it replaces — is written into a staging directory, fsynced, and
// published under one fixed name by a single `rename(2)`. That rename is the commit
// point: before it, nothing canonical has moved and a crash loses only litter; after
// it, the exact signed pair is on disk and every later run replays it forward. Replay
// happens on LOCK ACQUISITION, before any reader may look at the pair, so no caller can
// observe a torn pair at all.
//
// Recording the PREDECESSOR is what makes replay safe rather than reckless. A recovering
// run compares each canonical half against that predecessor and against the target: if a
// half is neither, something else authoritative landed while this machine was down (a
// copy from another machine, a newer join), and replaying would silently destroy it. In
// that case the transaction is left in place and the operator reconciles deliberately.

/// The ONE fixed name a committed roster transaction takes. Fixed because it is a
/// rendezvous: recovery must be able to FIND it without knowing which run wrote it.
fn roster_txn_path(path: &str) -> String {
    concat(&[path, ".atpkg-keys.txn"])
}

/// Refuse to READ a roster while a committed transaction is still pending.
///
/// Recovery is a WRITE, performed only by [`lock_roster`], so a reader cannot fix this —
/// but it must not paper over it either. Between a crash and the next writer, the two
/// canonical files may be a torn pair, and a reader that proceeds reports "does not
/// verify under the master you typed": an operator sent to re-check a paper phrase that
/// was never wrong.
///
/// Public because the readers are not all in this crate: `cargo ship provision --check`
/// audits the same pair and promises to write nothing, so it may not take the writer
/// lock (acquiring it REPAIRS) — it applies this refusal instead, and reports.
pub fn refuse_pending_roster_transaction(path: &str) -> Result<(), String> {
    let transaction = roster_txn_path(path);
    match std::fs::symlink_metadata(&transaction) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(concat(&[
            "a durable interrupted roster transaction exists at ",
            &transaction,
            ". Reading never replays it — recovery is a write, and only the roster writer \
             lock may perform it. Run any roster command (`setup`, `join`, \
             `machine-revoke`) to complete the exact signed pair forward, then retry. The \
             pair on disk is NOT authoritative until that happens, and your master phrase \
             is not what is wrong.",
        ])),
        Err(e) => Err(concat(&[
            "inspect roster transaction path ",
            &transaction,
            ": ",
            &e.to_string(),
        ])),
    }
}

/// Write one member of a staged transaction: owner-only, never over an existing name,
/// and flushed to the device before the caller may fsync the directory that will publish
/// it.
fn write_txn_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    // `append(true)` rather than `write(true)` for the FFI-summary reason spelled out in
    // [`crate::fsio`]; byte-identical for a `create_new` file written once.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Commit a complete roster transition to one durable redo directory, returning its path.
///
/// The fixed name appears only after the expected predecessor, both target halves, and
/// the staging directory itself have been fsynced — so the commit `rename` publishes a
/// directory that is already whole on the device. Persisting the predecessor is what
/// lets [`complete_roster_transaction`] tell an interrupted transition apart from a
/// newer pair that landed while this process was down.
fn begin_roster_transaction(
    path: &str,
    expected: Option<&RosterSnapshot>,
    bytes: &[u8],
    sig: &[u8],
) -> Result<String, String> {
    use std::os::unix::fs::DirBuilderExt as _;

    let committed = roster_txn_path(path);
    match std::fs::symlink_metadata(&committed) {
        Ok(_) => {
            return Err(concat(&[
                "a prior roster transaction still exists at ",
                &committed,
                "; acquire the roster lock to recover it before starting another",
            ]));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(concat(&[
                "inspect roster transaction path ",
                &committed,
                ": ",
                &e.to_string(),
            ]));
        }
    }
    // The STAGING name is unique (pid + clock + attempt), never the fixed one: a run that
    // dies mid-staging must leave litter, not something recovery would replay.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    let mut attempt = 0u64;
    let staged = loop {
        let candidate = concat(&[
            path,
            ".atpkg-keys.",
            &std::process::id().to_string(),
            ".",
            &now,
            ".",
            &attempt.to_string(),
            ".txn",
        ]);
        match std::fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => break candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt = attempt.wrapping_add(1);
            }
            Err(e) => {
                return Err(concat(&[
                    "stage roster transaction beside ",
                    path,
                    ": ",
                    &e.to_string(),
                ]));
            }
        }
    };
    let body_path = std::path::Path::new(&staged).join("body");
    let sig_path = std::path::Path::new(&staged).join("sig");
    let predecessor_state = std::path::Path::new(&staged).join("predecessor-state");
    let predecessor_body = std::path::Path::new(&staged).join("predecessor-body");
    let predecessor_sig = std::path::Path::new(&staged).join("predecessor-sig");
    let staged_result = write_txn_file(
        &predecessor_state,
        if expected.is_some() {
            b"present\n"
        } else {
            b"missing\n"
        },
    )
    .and_then(|()| match expected {
        Some(snapshot) => write_txn_file(&predecessor_body, &snapshot.raw)
            .and_then(|()| write_txn_file(&predecessor_sig, &snapshot.sig)),
        None => Ok(()),
    })
    .and_then(|()| write_txn_file(&body_path, bytes))
    .and_then(|()| write_txn_file(&sig_path, sig))
    .and_then(|()| std::fs::File::open(&staged)?.sync_all());
    if let Err(e) = staged_result {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(concat(&[
            "write staged roster transaction ",
            &staged,
            ": ",
            &e.to_string(),
        ]));
    }
    // THE COMMIT POINT. One rename of a whole directory, then the parent fsync that makes
    // the new name itself survive a power loss.
    if let Err(e) = std::fs::rename(&staged, &committed).and_then(|()| sync_parent(&committed)) {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(concat(&[
            "commit roster redo transaction ",
            &committed,
            ": ",
            &e.to_string(),
        ]));
    }
    Ok(committed)
}

/// `Some(bytes)` if the file is there, `None` if it is genuinely absent — and an ERROR
/// for anything else, because "unreadable" must never be scored as "absent" by the
/// predecessor comparison below.
fn read_optional_bytes(path: &str) -> Result<Option<Vec<u8>>, String> {
    match read_bytes(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(concat(&[
            "inspect roster transaction premise ",
            path,
            ": ",
            &e.to_string(),
        ])),
    }
}

/// The pair a committed transaction expected to replace: `None` when it was published
/// over nothing (a first roster), `Some` for its exact previous bytes.
fn transaction_predecessor(transaction: &str) -> Result<Option<RosterSnapshot>, String> {
    let state_path = std::path::Path::new(transaction).join("predecessor-state");
    let state_path = state_path
        .to_str()
        .ok_or("roster transaction predecessor-state path is not UTF-8")?;
    let state = read_bytes(state_path).map_err(|e| {
        concat(&[
            "recover roster transaction ",
            transaction,
            ": read predecessor state: ",
            &e.to_string(),
        ])
    })?;
    match state.as_slice() {
        b"missing\n" => Ok(None),
        b"present\n" => {
            let body_path = std::path::Path::new(transaction).join("predecessor-body");
            let sig_path = std::path::Path::new(transaction).join("predecessor-sig");
            let body_path = body_path
                .to_str()
                .ok_or("roster transaction predecessor-body path is not UTF-8")?;
            let sig_path = sig_path
                .to_str()
                .ok_or("roster transaction predecessor-signature path is not UTF-8")?;
            let raw = read_bytes(body_path).map_err(|e| {
                concat(&[
                    "recover roster transaction ",
                    transaction,
                    ": read predecessor body: ",
                    &e.to_string(),
                ])
            })?;
            let sig = read_bytes(sig_path).map_err(|e| {
                concat(&[
                    "recover roster transaction ",
                    transaction,
                    ": read predecessor signature: ",
                    &e.to_string(),
                ])
            })?;
            Ok(Some(RosterSnapshot { raw, sig }))
        }
        _ => Err(concat(&[
            "roster transaction ",
            transaction,
            " has an invalid predecessor-state marker; leave it in place and investigate",
        ])),
    }
}

/// Is this canonical half one the transaction is entitled to overwrite — either the
/// predecessor it recorded, or the target it is trying to install (the crash landed
/// after this half's rename)? Anything else is a THIRD state nobody in this transaction
/// wrote, and replaying over it would destroy it.
fn transaction_half_is_known(
    current: Option<&[u8]>,
    predecessor: Option<&[u8]>,
    target: &[u8],
) -> bool {
    current == predecessor || current == Some(target)
}

/// Drop a completed transaction's fixed name, so nothing replays it a second time.
///
/// The rename-then-fsync-then-delete order is deliberate: once the FIXED name is durably
/// gone, the leftover directory is inert, and its best-effort removal cannot leave a
/// half-deleted transaction under a name recovery still trusts.
fn retire_roster_transaction(transaction: &str) -> Result<(), String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let retired = concat(&[
        transaction,
        ".retired.",
        &std::process::id().to_string(),
        ".",
        &now.to_string(),
    ]);
    std::fs::rename(transaction, &retired).map_err(|e| {
        concat(&[
            "retire completed roster transaction ",
            transaction,
            ": ",
            &e.to_string(),
        ])
    })?;
    sync_parent(transaction).map_err(|e| {
        concat(&[
            "persist retirement of completed roster transaction ",
            transaction,
            ": ",
            &e.to_string(),
            "; the canonical pair is complete, but leave the retired directory in place",
        ])
    })?;
    let _ = std::fs::remove_dir_all(&retired);
    let _ = sync_parent(&retired);
    Ok(())
}

/// Complete a committed pair FORWARD, if one is pending.
///
/// This runs on every write-lock acquisition before the caller may read the roster, so a
/// process death after either canonical rename converges on the exact signed pair the
/// redo directory preserved. It is idempotent: replaying a pair that is already fully
/// installed rewrites the same bytes and retires the transaction.
fn complete_roster_transaction(path: &str) -> Result<(), String> {
    let transaction = roster_txn_path(path);
    match std::fs::symlink_metadata(&transaction) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(concat(&[
                "inspect roster transaction path ",
                &transaction,
                ": ",
                &e.to_string(),
            ]));
        }
    }
    let body_source = std::path::Path::new(&transaction).join("body");
    let sig_source = std::path::Path::new(&transaction).join("sig");
    let body_source = body_source
        .to_str()
        .ok_or("roster transaction body path is not UTF-8")?;
    let sig_source = sig_source
        .to_str()
        .ok_or("roster transaction signature path is not UTF-8")?;
    let bytes = read_bytes(body_source).map_err(|e| {
        concat(&[
            "recover roster transaction ",
            &transaction,
            ": read body: ",
            &e.to_string(),
        ])
    })?;
    let sig = read_bytes(sig_source).map_err(|e| {
        concat(&[
            "recover roster transaction ",
            &transaction,
            ": read signature: ",
            &e.to_string(),
        ])
    })?;
    let predecessor = transaction_predecessor(&transaction)?;
    let sig_path = concat(&[path, ".sig"]);
    let current_body = read_optional_bytes(path)?;
    let current_sig = read_optional_bytes(&sig_path)?;
    let predecessor_body = predecessor.as_ref().map(|snapshot| snapshot.raw.as_slice());
    let predecessor_sig = predecessor.as_ref().map(|snapshot| snapshot.sig.as_slice());
    if !transaction_half_is_known(current_body.as_deref(), predecessor_body, &bytes)
        || !transaction_half_is_known(current_sig.as_deref(), predecessor_sig, &sig)
    {
        return Err(concat(&[
            "roster transaction ",
            &transaction,
            " cannot be replayed: the canonical pair is neither its exact predecessor, a \
             partial target, nor its exact target. A newer or unrelated authoritative pair \
             may have landed while the transaction was interrupted. Nothing was overwritten; \
             preserve all files and reconcile deliberately.",
        ]));
    }
    write_bytes_atomic(path, &bytes).map_err(|e| {
        concat(&[
            "complete roster body from durable transaction ",
            &transaction,
            ": ",
            &e.to_string(),
        ])
    })?;
    write_bytes_atomic(&sig_path, &sig).map_err(|e| {
        concat(&[
            "complete roster signature from durable transaction ",
            &transaction,
            ": ",
            &e.to_string(),
        ])
    })?;
    // Retirement is only legitimate once the pair is PROVED installed: retiring on the
    // strength of two Ok returns would drop the redo log on the word of the same
    // filesystem that just lost the pair.
    if read_bytes(path).ok().as_deref() != Some(bytes.as_slice())
        || read_bytes(&sig_path).ok().as_deref() != Some(sig.as_slice())
    {
        return Err(concat(&[
            "roster transaction ",
            &transaction,
            " did not reproduce its exact pair; leave it in place and investigate",
        ]));
    }
    retire_roster_transaction(&transaction)
}

/// How a [`RosterLock`] holds its `flock`: a writer's managed rendezvous (created if
/// absent, transaction replayed on acquisition) or a read-only claim on one that must
/// already exist.
enum RosterGuard {
    Managed { _guard: aterm_update_core::FileLock },
    Existing { _file: std::fs::File },
}

/// The advisory lock a roster writer holds across its whole read → edit → sign →
/// publish sequence.
///
/// Without it, two runs could each read sequence N, each sign N+1, and each pass a
/// compare-then-publish premise check in the gap before the other's rename — a
/// same-sequence fork the monotonic ratchet cannot see, in which the second publish
/// silently de-authorizes the machine the first one added. `flock` is released by the
/// kernel when the file closes, INCLUDING on a crash, so serializing here does not buy a
/// stale-lock recovery ceremony.
///
/// # Which writers take it
///
/// Named, not asserted, because "every writer" is a claim about code this crate cannot
/// see, and a doc that states an invariant the workspace does not hold is worse than one
/// that states the boundary. As of this commit, every writer of a roster PAIR that ships
/// in this workspace's binaries goes through this lock and the redo transaction behind it
/// (test fixtures elsewhere write pair bytes directly, and are not writers of anybody's
/// roster):
///
/// * `atpkg-keys setup` / `join` — [`write_rest`], which takes the lock before its
///   premise check and holds it through publication;
/// * `atpkg-keys machine-revoke` — one lock across read, revoke and publish;
/// * `cargo ship provision` — its PHASE 1 roster seeding (the kept-copy restore, the
///   channel/dist install, and the kept copy written after `authorize_cut`) takes it in
///   `aterm-release`'s `provision::lock_roster_pair`, and drops it before the in-process
///   mint re-takes it in `write_rest`; and
/// * `cargo ship recover` — the `reconstruct_roster_assets` rewrite of `dist/` from an
///   already-published release.
///
/// It is `flock`, so it serializes those writers across PROCESSES; it is per open file
/// description, so a single process that takes it twice blocks on itself. Callers that
/// only observe take [`lock_roster_read_only`], or apply
/// [`refuse_pending_roster_transaction`] alone when no rendezvous need exist.
pub struct RosterLock {
    path: String,
    _guard: RosterGuard,
}

impl RosterLock {
    /// Compare the guarded pair with the exact bytes the caller read (`None` = the
    /// caller planned a FIRST roster, so both halves must still be absent).
    ///
    /// Deliberately a method on the live guard: it is impossible to run this
    /// compare-and-swap without first holding the serialization that makes it mean
    /// something.
    pub fn assert_snapshot(&self, expected: Option<&RosterSnapshot>) -> Result<(), String> {
        let sig_path = concat(&[&self.path, ".sig"]);
        match expected {
            Some(snapshot) => {
                let body_now = read_bytes(&self.path);
                let sig_now = read_bytes(&sig_path);
                if body_now.as_deref().ok() != Some(snapshot.raw.as_slice())
                    || sig_now.as_deref().ok() != Some(snapshot.sig.as_slice())
                {
                    return Err(concat(&[
                        &self.path,
                        " (or its .sig) CHANGED ON DISK since this run read it — another \
                         join/mint/revoke, or a copy from another machine, landed in \
                         between. The document this run signed extends the OLDER pair, so \
                         publishing it now would fork the lineage: two rosters at the same \
                         sequence de-authorize each other's machines silently. Re-run this \
                         command against what the file says now; if several machines are \
                         being provisioned from one copy, serialize the joins through one \
                         machine at a time.",
                    ]));
                }
            }
            None => {
                let body_absent = match std::fs::symlink_metadata(&self.path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Ok(_) => false,
                    Err(e) => {
                        return Err(concat(&[
                            "inspect fresh-roster premise ",
                            &self.path,
                            ": ",
                            &e.to_string(),
                        ]));
                    }
                };
                let sig_absent = match std::fs::symlink_metadata(&sig_path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                    Ok(_) => false,
                    Err(e) => {
                        return Err(concat(&[
                            "inspect fresh-roster premise ",
                            &sig_path,
                            ": ",
                            &e.to_string(),
                        ]));
                    }
                };
                if !body_absent || !sig_absent {
                    return Err(concat(&[
                        "a roster pair APPEARED at ",
                        &self.path,
                        " while this run was in flight — this run planned the FIRST roster \
                         for a fresh master, and writing it over one that now exists would \
                         fork the lineage. Work out where that roster came from before \
                         re-running.",
                    ]));
                }
            }
        }
        Ok(())
    }
}

/// Take the roster's writer lock, replaying any committed transaction FORWARD before
/// returning — so the caller's very first read already sees a consistent pair.
pub fn lock_roster(path: &str) -> Result<RosterLock, String> {
    let lock_path = concat(&[path, ".lock"]);
    ensure_parent_dir(&lock_path).map_err(|e| {
        concat(&[
            "create the roster-lock directory for ",
            &lock_path,
            ": ",
            &e.to_string(),
        ])
    })?;
    let guard = aterm_update_core::FileLock::acquire(std::path::Path::new(&lock_path))
        .map_err(|e| concat(&["lock roster ", path, ": ", &e.to_string()]))?;
    complete_roster_transaction(path)?;
    Ok(RosterLock {
        path: path.to_string(),
        _guard: RosterGuard::Managed { _guard: guard },
    })
}

/// Claim an ALREADY-ESTABLISHED roster rendezvous without creating or repairing
/// anything — for callers that only observe.
///
/// An observer must REPORT an interrupted redo transaction, never complete one as a side
/// effect of looking: recovery is a write, and a caller that promised to write nothing
/// may not perform it.
pub fn lock_roster_read_only(path: &str) -> Result<RosterLock, String> {
    use std::os::unix::fs::MetadataExt as _;

    let lock_path = concat(&[path, ".lock"]);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(&lock_path)
        .map_err(|e| {
            concat(&[
                "open existing roster lock ",
                &lock_path,
                " without creating it: ",
                &e.to_string(),
            ])
        })?;
    file.lock()
        .map_err(|e| concat(&["lock roster ", path, ": ", &e.to_string()]))?;

    // Refuse a raced unlink/replacement after open. Normal roster tooling never deletes
    // this rendezvous, but a read-only claim must not sit locking an orphaned inode while
    // a writer creates and locks a new path under the same name.
    let opened = file.metadata().map_err(|e| {
        concat(&[
            "inspect opened roster lock ",
            &lock_path,
            ": ",
            &e.to_string(),
        ])
    })?;
    let current = std::fs::symlink_metadata(&lock_path).map_err(|e| {
        concat(&[
            "re-check existing roster lock ",
            &lock_path,
            ": ",
            &e.to_string(),
        ])
    })?;
    if opened.dev() != current.dev() || opened.ino() != current.ino() {
        return Err(concat(&[
            "the roster lock ",
            &lock_path,
            " was replaced while a read-only claim acquired it; nothing was written, retry",
        ]));
    }

    refuse_pending_roster_transaction(path)?;
    Ok(RosterLock {
        path: path.to_string(),
        _guard: RosterGuard::Existing { _file: file },
    })
}

/// Lock, read the pair, and publish — the whole sequence in one call.
///
/// Only the tests use this: a real mutation must hold ONE lock across its own read,
/// edit, sign and publish, which is [`publish_roster_locked`]. Kept because the
/// transaction layer's properties are stated over a publish, and stating them through
/// the full provisioning pipeline would test four other things at once.
#[cfg(test)]
fn publish_roster(path: &str, bytes: &[u8], sig: &[u8]) -> Result<(), String> {
    let lock = lock_roster(path)?;
    let sig_path = concat(&[path, ".sig"]);
    let expected = match (read_bytes(path), read_bytes(&sig_path)) {
        (Ok(raw), Ok(sig)) => Some(RosterSnapshot { raw, sig }),
        (Err(body), Err(signature))
            if body.kind() == std::io::ErrorKind::NotFound
                && signature.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        _ => {
            return Err(concat(&[
                "the roster pair at ",
                path,
                " is missing one half or unreadable; restore both files before publishing",
            ]));
        }
    };
    publish_roster_locked(&lock, path, expected.as_ref(), bytes, sig)
}

/// A roster transition that is DURABLY COMMITTED but not yet installed under the two
/// canonical names — the state a process death between the commit and the renames
/// leaves on disk, as a value.
///
/// Reaching it costs nothing: [`publish_roster_locked`] is exactly
/// [`commit_roster_pair`] followed by [`CommittedRoster::complete`]. Naming the commit
/// point is what lets the property be STATED — every recovery message in this layer
/// turns on which side of it a failure landed — and it is the only honest way for a
/// crate that publishes rosters through this layer (`aterm-release`, whose `cargo ship
/// provision` seeds the same pair) to prove its own crash recovery: dropping this value
/// instead of completing it is what a killed process leaves, byte for byte, produced by
/// the real commit path rather than a hand-built replica of the on-disk layout.
#[must_use = "a committed roster transaction that is never completed leaves the pair for \
              the next writer to recover; drop it deliberately or call complete()"]
pub struct CommittedRoster {
    path: String,
    transaction: String,
}

impl CommittedRoster {
    /// The redo directory holding the exact signed pair — the path recovery advice names.
    #[must_use]
    pub fn transaction_path(&self) -> &str {
        &self.transaction
    }

    /// Install both canonical halves from the commit and retire it.
    pub fn complete(self) -> Result<(), String> {
        complete_roster_transaction(&self.path).map_err(|e| {
            concat(&[
                &e,
                "\nThe exact new roster pair remains durably recoverable at ",
                &self.transaction,
                ". Remove any filesystem obstruction and rerun any roster command; taking \
                 the roster lock completes it forward before reading.",
            ])
        })
    }
}

/// Check the caller's premise and COMMIT the new pair to the redo log, without touching
/// either canonical name yet.
///
/// `expected` is the pair the caller read under this same lock; it is both the CAS
/// premise and the predecessor the redo transaction records for crash recovery.
pub fn commit_roster_pair(
    lock: &RosterLock,
    path: &str,
    expected: Option<&RosterSnapshot>,
    bytes: &[u8],
    sig: &[u8],
) -> Result<CommittedRoster, String> {
    if lock.path != path {
        return Err("the roster lock does not guard the roster being published".to_string());
    }
    lock.assert_snapshot(expected)?;
    let _ = ensure_parent_dir(path);
    let transaction = begin_roster_transaction(path, expected, bytes, sig)?;
    Ok(CommittedRoster {
        path: path.to_string(),
        transaction,
    })
}

/// Publish an already-signed pair while the caller holds the roster lock across its own
/// read → edit → CAS sequence. This is the form every roster mutation uses.
pub fn publish_roster_locked(
    lock: &RosterLock,
    path: &str,
    expected: Option<&RosterSnapshot>,
    bytes: &[u8],
    sig: &[u8],
) -> Result<(), String> {
    commit_roster_pair(lock, path, expected, bytes, sig)?.complete()
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
        ensure_parent_dir(path).map_err(|e| {
            concat(&[
                "cannot create the directory for ",
                path,
                ": ",
                &e.to_string(),
            ])
        })?;
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
        match append_member(
            &text,
            MASTER_ANCHOR,
            &master_pubkey,
            &refs,
            MAX_MASTER_MEMBERS,
        )? {
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
///
/// The roster half now runs under [`lock_roster`], so the recovery advice branches on the
/// redo transaction's commit point: before it, undo the key; after it, KEEP the key,
/// because the pair that authorizes it is durable and the next run installs it.
pub fn write_rest(planned: Planned) -> Result<Report, String> {
    // ONE crash-released advisory lock, taken BEFORE the premise check and held through
    // both roster promotions. Every roster writer takes this same lock, which is what
    // closes the compare-then-publish race the old inline check could not: two runs that
    // both read sequence N could both pass their own check in the gap before the other's
    // rename, and produce two seq N+1 lineages. Taking it also completes any redo
    // transaction a previous run's death left committed, so the premise this run checks
    // is a consistent pair and not a torn one.
    let roster_lock = lock_roster(&planned.paths.roster)?;

    // THE ROSTER'S PREMISE CHECK — the same rule `write_pins` applies to the anchor file,
    // for the same reason: the signed roster this run built extends the pair `plan` read,
    // and this repository has other agents and other sessions. The guard owns the
    // compare-and-swap, so a pair that moved (or appeared, when this run planned fresh)
    // refuses before either local identity file is written.
    roster_lock
        .assert_snapshot(planned.roster_snapshot.as_ref())
        .map_err(|e| {
            concat(&[
                &e,
                " NOTHING HAS BEEN WRITTEN: this machine's key does not exist yet.",
            ])
        })?;
    // Defensive: preflight created this directory before any secret existed. Doing it again
    // costs a syscall and covers the caller who assembled a `Planned` without going through
    // preflight — which the library's own tests do.
    let _ = ensure_parent_dir(&planned.paths.key);
    // Whole, 0600, fsynced, and never over an existing key — all four at once, because a
    // key that exists under its final name but is empty after a power loss is
    // indistinguishable from one the roster is about to authorize.
    write_owner_file_create_new(&planned.paths.key, &planned.machine_pkcs8).map_err(|e| {
        concat(&[
            "atomically create and sync ",
            &planned.paths.key,
            " (refusing to overwrite an existing key): ",
            &e.to_string(),
            ". No roster was written, so nothing authorizes this machine; if a complete \
             key now exists, move it aside before retrying",
        ])
    })?;

    let mut record = String::from("id = \"");
    record.push_str(&planned.id);
    record.push_str("\"\npubkey = \"");
    record.push_str(&planned.machine_pubkey);
    record.push_str("\"\nminted_at = \"");
    record.push_str(&aterm_types::rfc3339::format_rfc3339(planned.now));
    record.push_str("\"\n");
    let _ = ensure_parent_dir(&planned.paths.machine_pub);
    let _ = write_bytes(&planned.paths.machine_pub, record.as_bytes());

    // The roster last, under the lock this function already holds. The redo transaction's
    // commit is the line the recovery advice turns on: BEFORE it, nothing canonical moved
    // and this machine simply holds an unauthorized key; AFTER it, the exact signed pair
    // is durable and the operator must KEEP the key rather than undo it. The old wording
    // could not make that distinction and told every failure to `rm` the key — which,
    // after a committed transaction, deletes the private half of a machine the recovered
    // roster authorizes.
    publish_roster_locked(
        &roster_lock,
        &planned.paths.roster,
        planned.roster_snapshot.as_ref(),
        &planned.roster_bytes,
        &planned.roster_sig,
    )
    .map_err(|e| {
        let transaction = roster_txn_path(&planned.paths.roster);
        let roster_sig_path = concat(&[&planned.paths.roster, ".sig"]);
        let pair_is_target = read_bytes(&planned.paths.roster).ok().as_deref()
            == Some(planned.roster_bytes.as_slice())
            && read_bytes(&roster_sig_path).ok().as_deref() == Some(planned.roster_sig.as_slice());
        match std::fs::symlink_metadata(&transaction) {
            Ok(_) => concat(&[
                &e,
                "\nThe roster update HAS a durable redo transaction, so the exact signed \
                 pair is recoverable: KEEP this machine's key file. Clear the filesystem \
                 obstruction named above, then run any roster command — taking the roster \
                 lock completes the pair forward before reading it.",
            ]),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && pair_is_target => concat(&[
                &e,
                "\nThe exact target roster pair IS installed, even though retiring the \
                 transaction could not be confirmed: KEEP this machine's key file and do \
                 NOT mint again. Rerun once the durability error above is fixed.",
            ]),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => concat(&[
                &e,
                "\nNothing authorizes this machine yet: no redo transaction was committed, \
                 so the roster pair is still the one from before this run. This machine's \
                 key file DOES exist; undo it before retrying, or the retry will mint a \
                 second key: `rm ",
                &planned.paths.key,
                "` (and `git checkout -- ",
                &planned.paths.pins,
                "` if this run armed the master anchor).",
            ]),
            Err(err) => concat(&[
                &e,
                "\nCould not inspect the possible redo transaction at ",
                &transaction,
                ": ",
                &err.to_string(),
                ". KEEP this machine's key file and investigate; do not mint again until \
                 the transaction state is known.",
            ]),
        }
    })?;

    let machine_is_committed_head = planned.channel_after.first() == Some(&planned.machine_pubkey);

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
        let s = |n: &str| {
            dir.join(n)
                .to_str()
                .expect("utf-8 scratch path")
                .to_string()
        };
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
        let verified =
            aterm_update_core::roster::verify_roster(&[report.master_pubkey.as_str()], bytes, &sig)
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

    /// A BROKEN SIGNATURE TARGET LEAVES THE DOCUMENT WHERE IT WAS. Per-file atomicity
    /// cannot make the PAIR consistent: before the transaction layer, a failure between
    /// the two renames left the NEW document beside the OLD signature — refused by every
    /// verifier as "signature did not verify" and misdiagnosed as a mistyped phrase, on a
    /// file `git checkout` cannot restore (dist/ is gitignored). The guarded publisher
    /// reads BOTH halves under its lock first, so a directory squatting on one target is
    /// a premise failure and the document never moves at all — there is nothing to roll
    /// back.
    ///
    /// MUTATION: let a publisher proceed without the pair it claims to extend — drop the
    /// premise read and `assert_snapshot`, and promote the two halves in sequence — and
    /// "NEW BODY" lands beside a signature no client can verify.
    #[test]
    fn a_broken_signature_target_leaves_the_roster_document_unchanged() {
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
        assert!(err.contains("missing one half or unreadable"), "{err}");
        assert_eq!(
            std::fs::read(&paths.roster).unwrap(),
            old_body,
            "the document must still be the pair's previous bytes"
        );
        std::fs::remove_dir(&sig_path).unwrap();
        std::fs::write(&sig_path, &old_sig).unwrap();

        // The restored pair still verifies under the master — the operator is NOT sent
        // chasing a "wrong phrase" diagnosis.
        let master_pub = seed_of(PAPER).pubkey_b64().unwrap();
        assert!(
            load_roster(
                &paths.roster,
                &master_pub,
                NOW,
                RosterExpectation::MustExist
            )
            .is_ok(),
            "the pair left behind by a failed publish must be the consistent old one"
        );

        // A FIRST publish with the same obstruction likewise writes neither half: no
        // half-pair is left to misdiagnose.
        let fresh = scratch("pair-rollback-fresh");
        let fresh_roster = fresh
            .join("aterm-machines.toml")
            .to_str()
            .unwrap()
            .to_string();
        let fresh_sig = concat(&[&fresh_roster, ".sig"]);
        std::fs::create_dir(&fresh_sig).unwrap();
        let err = publish_roster(&fresh_roster, b"BODY", b"SIG").unwrap_err();
        assert!(err.contains("missing one half or unreadable"), "{err}");
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

    /// A CRASH BETWEEN THE TWO RENAMES CONVERGES FORWARD, NOT SIDEWAYS. This is the
    /// exact state per-file atomicity could not prevent and no error path could repair:
    /// the new document on disk beside the old signature, with the process gone. The
    /// committed redo directory holds the whole pair, and the next lock acquisition
    /// installs it — before the caller is allowed to read anything.
    ///
    /// The crash is simulated by doing what the publisher does (commit the transaction,
    /// promote the body) and then simply not finishing.
    ///
    /// MUTATION: drop the `complete_roster_transaction` call from `lock_roster` and the
    /// signature stays at "old sig" — a pair no client will verify.
    #[test]
    fn a_committed_roster_transaction_recovers_forward_after_a_body_only_crash() {
        let dir = scratch("pair-redo-recovery");
        let roster = dir.join("aterm-machines.toml");
        let roster = roster.to_str().unwrap();
        let sig_path = concat(&[roster, ".sig"]);
        std::fs::write(roster, b"old body").unwrap();
        std::fs::write(&sig_path, b"old sig").unwrap();

        let predecessor = RosterSnapshot {
            raw: b"old body".to_vec(),
            sig: b"old sig".to_vec(),
        };
        let transaction =
            begin_roster_transaction(roster, Some(&predecessor), b"new body", b"new sig").unwrap();
        write_bytes_atomic(roster, b"new body").unwrap();
        assert_eq!(
            std::fs::read(&sig_path).unwrap(),
            b"old sig",
            "the premise: a torn pair, which is what a verifier refuses"
        );

        let _lock = lock_roster(roster).expect("lock acquisition replays the committed redo");
        assert_eq!(std::fs::read(roster).unwrap(), b"new body");
        assert_eq!(std::fs::read(&sig_path).unwrap(), b"new sig");
        assert!(
            !std::path::Path::new(&transaction).exists(),
            "a completed transaction is retired, so it cannot replay a second time"
        );
    }

    /// RECOVERY NEVER OVERWRITES A PAIR IT DID NOT WRITE. A redo log that replayed
    /// unconditionally would be worse than the torn pair it fixes: the machine comes back
    /// up days later, the operator has already copied a NEWER roster across from another
    /// machine, and replay silently destroys it — de-authorizing every machine joined
    /// since. The recorded predecessor is what makes that distinguishable: a canonical
    /// half that is neither the predecessor nor the target is a third state nobody in
    /// this transaction wrote.
    ///
    /// MUTATION: make `transaction_half_is_known` return `true` and the newer pair is
    /// overwritten by the interrupted one.
    #[test]
    fn redo_recovery_never_overwrites_a_newer_or_unrelated_pair() {
        let dir = scratch("pair-redo-cas");
        let roster = dir.join("aterm-machines.toml");
        let roster = roster.to_str().unwrap();
        let sig_path = concat(&[roster, ".sig"]);
        std::fs::write(roster, b"old body").unwrap();
        std::fs::write(&sig_path, b"old sig").unwrap();
        drop(lock_roster(roster).expect("establish the writer rendezvous"));

        let predecessor = RosterSnapshot {
            raw: b"old body".to_vec(),
            sig: b"old sig".to_vec(),
        };
        let transaction = begin_roster_transaction(
            roster,
            Some(&predecessor),
            b"interrupted body",
            b"interrupted sig",
        )
        .unwrap();
        // While this machine was down, an authoritative pair arrived from elsewhere.
        std::fs::write(roster, b"newer body").unwrap();
        std::fs::write(&sig_path, b"newer sig").unwrap();

        let err = lock_roster(roster)
            .err()
            .expect("replay over an unrelated pair is refused");
        assert!(err.contains("newer or unrelated"), "{err}");
        assert_eq!(std::fs::read(roster).unwrap(), b"newer body");
        assert_eq!(std::fs::read(&sig_path).unwrap(), b"newer sig");
        assert!(
            std::path::Path::new(&transaction).exists(),
            "the evidence stays on disk for the operator to reconcile"
        );
    }

    /// AN OBSERVER OBSERVES. Recovery is a WRITE, and completing a redo transaction as a
    /// side effect of looking would make a read-only caller install a roster nobody asked
    /// it to install. The read-only claim therefore refuses to create the rendezvous file
    /// and refuses to replay a pending transaction — it reports it instead.
    ///
    /// MUTATION: give `lock_roster_read_only` `create(true)`, or let it call
    /// `complete_roster_transaction`, and one of the two refusals below becomes a success
    /// that mutated the tree.
    #[test]
    fn check_only_lock_never_creates_or_recovers_roster_state() {
        let dir = scratch("read-only-roster-lock");
        let roster = dir.join("aterm-machines.toml");
        let roster = roster.to_str().unwrap();
        let lock_path = concat(&[roster, ".lock"]);
        let sig_path = concat(&[roster, ".sig"]);

        let err = lock_roster_read_only(roster)
            .err()
            .expect("an absent rendezvous is refused, not created");
        assert!(err.contains("without creating it"), "{err}");
        assert!(
            !std::path::Path::new(&lock_path).exists(),
            "an observer left no rendezvous file behind"
        );

        std::fs::write(roster, b"old body").unwrap();
        std::fs::write(&sig_path, b"old sig").unwrap();
        drop(lock_roster(roster).expect("establish the writer rendezvous"));
        // NEGATIVE CONTROL: with a rendezvous and no pending transaction, it succeeds —
        // so the refusals here are the policy and not a broken lock.
        drop(lock_roster_read_only(roster).expect("an existing clean rendezvous is readable"));

        let predecessor = RosterSnapshot {
            raw: b"old body".to_vec(),
            sig: b"old sig".to_vec(),
        };
        let transaction =
            begin_roster_transaction(roster, Some(&predecessor), b"new body", b"new sig").unwrap();
        let err = lock_roster_read_only(roster)
            .err()
            .expect("a pending transaction is reported, not replayed");
        assert!(err.contains("never replays"), "{err}");
        assert_eq!(std::fs::read(roster).unwrap(), b"old body");
        assert_eq!(std::fs::read(&sig_path).unwrap(), b"old sig");
        assert!(std::path::Path::new(&transaction).exists());
    }

    /// A PENDING TRANSACTION IS NAMED, NOT MISDIAGNOSED AS A MISTYPED PHRASE. Recovery
    /// runs on the WRITER lock, and `join` reads the roster (in `plan`) before it ever
    /// takes that lock — so without this refusal the first thing an operator sees after a
    /// crashed publish is "does not verify under the master you typed", against a torn
    /// pair and a paper phrase that was never wrong.
    ///
    /// MUTATION: drop the `refuse_pending_roster_transaction` call from `load_roster` and
    /// the signature diagnosis comes back.
    #[test]
    fn a_pending_roster_transaction_is_named_instead_of_a_mistyped_phrase() {
        let dir = scratch("pending-txn-read");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup publishes the first pair");
        let master_pub = seed_of(PAPER).pubkey_b64().unwrap();
        let sig_path = concat(&[&paths.roster, ".sig"]);

        // The crash: a committed transaction, its body promoted, its signature not.
        let predecessor = RosterSnapshot {
            raw: std::fs::read(&paths.roster).unwrap(),
            sig: std::fs::read(&sig_path).unwrap(),
        };
        begin_roster_transaction(&paths.roster, Some(&predecessor), b"new body", b"new sig")
            .unwrap();
        write_bytes_atomic(&paths.roster, b"new body").unwrap();

        let err = load_roster(
            &paths.roster,
            &master_pub,
            NOW,
            RosterExpectation::MustExist,
        )
        .unwrap_err();
        assert!(err.contains("interrupted roster transaction"), "{err}");
        assert!(err.contains("is not what is wrong"), "{err}");
        assert!(
            !err.contains("does not verify under the master you typed"),
            "the phrase must not be blamed for a torn pair: {err}"
        );

        // NEGATIVE CONTROL: once a writer completes it forward, the same read succeeds.
        drop(lock_roster(&paths.roster).expect("the writer lock recovers the pair"));
        assert_eq!(std::fs::read(&paths.roster).unwrap(), b"new body");
        assert_eq!(std::fs::read(&sig_path).unwrap(), b"new sig");
        let err = load_roster(
            &paths.roster,
            &master_pub,
            NOW,
            RosterExpectation::MustExist,
        )
        .unwrap_err();
        assert!(
            err.contains("does not verify under the master you typed"),
            "the recovered pair is read normally — and this fixture's bytes are not a \
             real roster, so the ordinary signature refusal is what should surface: {err}"
        );
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
            roster: dir
                .join("dist/aterm-machines.toml")
                .to_str()
                .unwrap()
                .to_string(),
            key: dir
                .join("home/.aterm/machine.key")
                .to_str()
                .unwrap()
                .to_string(),
            machine_pub: dir
                .join("home/.aterm/machine.toml")
                .to_str()
                .unwrap()
                .to_string(),
            pins_explicit: true,
        };
        write_fixture(&paths);
        assert!(
            !dir.join("home/.aterm").exists(),
            "the premise: no $HOME/.aterm"
        );

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
        // Preflight created this directory; pre-create the persistent advisory-lock inode
        // (opening an EXISTING file needs no directory write permission), then take the
        // write permission away so staging the roster transaction is the one step that
        // fails.
        std::fs::write(concat(&[&paths.roster, ".lock"]), b"").unwrap();
        std::fs::set_permissions(&roster_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_rest(planned);
        std::fs::set_permissions(&roster_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.unwrap_err();
        assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
        assert!(
            err.contains("git checkout --"),
            "the recovery names both files: {err}"
        );
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
        assert!(
            !after.contains(HEAD_KEY),
            "the retired key was not resurrected"
        );
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

        let pre =
            preflight(Verb::Join, "m11", "incumbent-head", &second).expect("preflight is fine");
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
        assert!(
            err.contains("does not verify under the master you typed"),
            "{err}"
        );

        assert_eq!(std::fs::read_to_string(&paths.pins).unwrap(), before);
        assert_eq!(std::fs::read(&paths.roster).unwrap(), roster_before);
        assert!(
            !std::path::Path::new(&second.key).exists(),
            "no key was minted"
        );

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
        assert!(
            err.contains("never overwrite a key the roster still names"),
            "{err}"
        );
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
            let pre = preflight(
                Verb::Setup,
                "m3-b",
                "incumbent-head",
                &Paths {
                    key: dir.join("m3b.key").to_str().unwrap().to_string(),
                    ..paths.clone()
                },
            )
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

        assert!(
            text.contains("=== DONE (working tree only — a commit makes it durable) ==="),
            "{text}"
        );

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
