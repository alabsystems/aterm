// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Provisioning: everything `atpkg-keys setup` and `join` do except reading the paper phrase
//! off `/dev/tty`.
//!
//! * `setup`, the first machine: generates the master, prints the phrase once, arms
//!   `pins::PAPER_MASTER_PUBKEYS`, mints this machine's keypair, and creates the
//!   master-signed roster.
//! * `join`, every later machine: proves the phrase against the committed anchor and the
//!   existing roster, then mints and extends the roster. It edits no trust anchor.
//!
//! The roster is the only grant: there is no compiled-in channel keyset, so every grant is
//! one `machine-revoke` can take back.
//!
//! Every function here takes a [`MasterSeed`], never a phrase or bytes; it has no byte
//! accessor, no `Clone` and a redacted `Debug`, so provisioning logic cannot write, log or
//! return the master. The split also lets the success path run against a synthetic seed
//! with no terminal.
//!
//! Three phases, and the order is the safety property:
//!
//! 1. [`preflight`]: every refusal possible before a secret exists. Writes no file.
//! 2. [`plan`]: compute the whole result in memory (the new `pins.rs` text, the machine
//!    keypair, the roster and its master signature). Bad input fails here, with the tree
//!    untouched.
//! 3. [`write_pins`] then [`write_rest`]: write the pre-computed bytes. The anchor goes first
//!    and is re-read, because the key and roster depend on it. `setup` shows the phrase and
//!    proves its transcription before either write (`main.rs`'s `provision`).
//!
//! The roster is a pair (`aterm-machines.toml` and its detached `.sig`), written under one
//! crash-released lock ([`lock_roster`]) through a durable redo transaction
//! ([`publish_roster_locked`]); the comment above that layer explains it. Nothing here
//! commits or pushes: [`render_report`] tells the operator to review the diff.

use crate::fsio::{
    concat, ensure_parent_dir, read_bytes, sync_parent, sync_parent_ordered, write_bytes_atomic,
    write_owner_file_create_new,
};
use crate::master::MasterSeed;
use crate::pins_edit::{
    Edit, MASTER_ANCHOR, MAX_MASTER_MEMBERS, append_member, read_anchor, verify_members,
};
use aterm_update_core::roster::{Roster, RosterReject};

/// Where the roster and its master signature live by default — the release staging dir,
/// so a cut picks them up as assets alongside the appcast.
pub const DEFAULT_ROSTER: &str = "dist/aterm-machines.toml";

/// The machine's secret key, relative to `$HOME`; it never leaves the machine that minted it.
pub const MACHINE_KEY_REL: &str = ".aterm/machine.key";

/// The machine's public record, beside its key: id, pubkey, mint time.
pub const MACHINE_PUB_REL: &str = ".aterm/machine.toml";

/// The anchor file, relative to the workspace root.
pub const PINS_REL: &str = "crates/aterm-update-core/src/pins.rs";

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

/// Every path a provisioning run touches, explicit so a test can run the whole path in a
/// temporary directory without `$HOME`, a repository or a terminal.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `crates/aterm-update-core/src/pins.rs`.
    pub pins: String,
    /// The roster document; its detached master signature is `<roster>.sig`.
    pub roster: String,
    /// This machine's secret key file, written 0600 and never copied off the machine.
    pub key: String,
    /// This machine's public record.
    pub machine_pub: String,
    /// Whether `pins` came from an explicit `--pins` rather than discovery. Discovery means
    /// the tree this binary was built from, so the compiled-in second-master refusal applies;
    /// an explicit path names another tree, whose own file anchor governs.
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
/// Found, not configured: a mistyped `--pins` is another transcription risk. The flag remains
/// for tests and for an operator who means a different tree.
pub fn discover_pins_path() -> Result<String, String> {
    let dir = std::env::current_dir()
        .map_err(|e| concat(&["cannot read the current directory: ", &e.to_string()]))?;
    discover_pins_path_from(dir)
}

/// [`discover_pins_path`]'s walk, from `dir` rather than the current directory.
fn discover_pins_path_from(mut dir: std::path::PathBuf) -> Result<String, String> {
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

/// Machine ids appear in signed documents and in a deny-list typed under pressure after a
/// theft, so they are 1-32 ASCII letters, digits, `-` and `_`: nothing confusable, nothing
/// that needs quoting, no homoglyphs.
pub fn vet_machine_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 32 {
        return Err("a machine id must be 1-32 characters".to_string());
    }
    if !id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("a machine id may contain only ASCII letters, digits, '-' and '_'".to_string());
    }
    Ok(())
}

/// A stable, printable name for a roster rejection, without `format!`.
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

/// Whether an absent roster may be the first mint. A check in [`preflight`] alone holds at
/// one moment: a roster removed before [`plan`] (`dist/` is gitignored, so a cleanup can
/// sweep it) would reach [`load_roster`]'s fresh branch and fork the lineage at the same
/// sequence. The mode enforces the rule at the point of use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterExpectation {
    /// The roster must already exist: `join`'s case, since a committed master implies a
    /// roster it signed exists somewhere, and starting another forks it.
    MustExist,
    /// An absent roster is the first mint: `setup`, whose preflight proved no master exists.
    MayCreateFresh,
}

/// The exact on-disk bytes of the roster pair a run planned against, so the write phase can
/// prove it replaces the pair it read (as [`write_pins`] does for the anchor).
#[derive(Debug, Clone)]
pub struct RosterSnapshot {
    pub raw: Vec<u8>,
    pub sig: Vec<u8>,
}

/// Read the roster and its signature, verify under `master_pubkey`, and parse. Returns the
/// roster and the exact pair bytes, or `None` for a fresh roster (no pair on disk, allowed
/// only under [`RosterExpectation::MayCreateFresh`]).
///
/// There is no unverified parse path, and verifying is also the transcription check: a
/// mistyped phrase derives a master that cannot verify the real one's roster. A pair with
/// one half missing is half-published and never starts a fresh roster.
pub fn load_roster(
    path: &str,
    master_pubkey: &str,
    now: u64,
    expect: RosterExpectation,
) -> Result<(Roster, Option<RosterSnapshot>), String> {
    // Before reading the pair: while a transaction is pending the files may be torn, and
    // reading one fails as "signature did not verify", the mistyped-phrase diagnosis this
    // layer exists to prevent.
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
            // `join`'s refusal, enforced at the point of use as well as at preflight (see
            // [`RosterExpectation`]).
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

// The roster pair's crash-safe transaction layer.
//
// The roster is two files, `aterm-machines.toml` and `aterm-machines.toml.sig`, meaningful
// only together. Per-file atomicity (`crate::fsio::write_bytes_atomic`) makes each file whole
// but cannot make the pair consistent: no syscall renames two names at once, and a process
// death or power loss between the renames runs no error path. It would leave the new
// document beside the old signature, refused by every client as "signature did not verify",
// misread by the operator as a mistyped phrase, in a gitignored `dist/` that `git checkout`
// cannot restore.
//
// So the pair is published through a redo log. The new pair and the exact predecessor it
// replaces are written into a staging directory, fsynced, and published under one fixed name
// by a single `rename(2)`, the commit point. Before it a crash leaves only litter; after it
// every later run replays the pair forward on lock acquisition, before any reader looks.
//
// The recorded predecessor makes replay safe: recovery compares each canonical half against
// it and the target, and a half that is neither means something authoritative landed
// meanwhile (a copy from another machine, a newer join). Then the transaction is left in
// place for the operator to reconcile.

/// The one fixed name a committed roster transaction takes: a rendezvous recovery can find
/// without knowing which run wrote it.
fn roster_txn_path(path: &str) -> String {
    concat(&[path, ".atpkg-keys.txn"])
}

/// Refuse to read a roster while a committed transaction is pending. Recovery is a write
/// that only [`lock_roster`] performs; a reader proceeding over a torn pair would report
/// "does not verify under the master you typed" and send the operator to re-check a correct
/// phrase.
///
/// Public for readers outside this crate: `targo --unverified ship provision --check` writes
/// nothing, so it may not take the writer lock (acquiring it repairs) and applies this
/// refusal instead.
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

/// Write one member of a staged transaction: `0600`, never over an existing name, and synced
/// before the caller fsyncs the directory that publishes it.
fn write_txn_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    // `append(true)`, not `write(true)`: see `crate::fsio::stage_sibling_temp_with_mode`.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    aterm_durable::durable(&file)
}

/// Commit a complete roster transition to one durable redo directory, returning its path.
///
/// The fixed name appears only after the predecessor, both target halves and the staging
/// directory are fsynced, so the commit `rename` publishes a directory already whole on the
/// device. The predecessor lets [`complete_roster_transaction`] tell an interrupted
/// transition from a newer pair that landed meanwhile.
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
    // The staging name is unique (pid + clock + attempt), never the fixed one: a run that
    // dies mid-staging leaves litter, not something recovery would replay.
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
    // Ordered: the staged entries land before the commit rename, as each member does.
    .and_then(|()| aterm_durable::ordered(&std::fs::File::open(&staged)?));
    if let Err(e) = staged_result {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(concat(&[
            "write staged roster transaction ",
            &staged,
            ": ",
            &e.to_string(),
        ]));
    }
    // The commit point: one rename of a whole directory, then the parent fsync that makes
    // the new name survive a power loss.
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

/// `Some(bytes)` if the file is there, `None` if it is absent, and an error otherwise: the
/// predecessor comparison must never score "unreadable" as "absent".
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

/// Whether a canonical half is one the transaction may overwrite: the predecessor it
/// recorded, or its own target (the crash came after this half's rename). Anything else is a
/// third state nobody in this transaction wrote.
fn transaction_half_is_known(
    current: Option<&[u8]>,
    predecessor: Option<&[u8]>,
    target: &[u8],
) -> bool {
    current == predecessor || current == Some(target)
}

/// Drop a completed transaction's fixed name so nothing replays it again. Rename, fsync, then
/// delete: once the fixed name is durably gone the leftover directory is inert, so its
/// best-effort removal cannot leave a half-deleted transaction under a trusted name.
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
    // Ordered: retirement only needs to land after the pair, and replay is idempotent.
    sync_parent_ordered(transaction).map_err(|e| {
        concat(&[
            "persist retirement of completed roster transaction ",
            transaction,
            ": ",
            &e.to_string(),
            "; the canonical pair is complete, but leave the retired directory in place",
        ])
    })?;
    let _ = std::fs::remove_dir_all(&retired);
    // Ordered: garbage collection; a resurrected retired directory is removed again.
    let _ = sync_parent_ordered(&retired);
    Ok(())
}

/// Complete a committed pair forward, if one is pending. Runs on every writer-lock
/// acquisition before the caller reads the roster, so a death after either canonical rename
/// converges on the signed pair the redo directory holds. Idempotent: replaying an installed
/// pair rewrites the same bytes and retires the transaction.
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
    #[cfg(test)]
    crate::fsio::window::reached("roster pair replayed", path);
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

/// The advisory lock a roster writer holds across its whole read, edit, sign and publish.
///
/// Without it, two runs could each read sequence N, sign N+1, and pass a compare-then-publish
/// check in the gap before the other's rename: a same-sequence fork the monotonic ratchet
/// cannot see, in which the second publish silently de-authorizes the first one's machine. It
/// is an [`aterm_update_core::FileLock`]: released by `LOCK_UN` when the guard drops (a copy
/// of the descriptor in a child mid-spawn cannot keep it) and by the kernel on a crash, so
/// there is no stale-lock recovery.
///
/// Every roster-pair writer in this workspace's binaries takes it (test fixtures elsewhere
/// write pair bytes directly):
///
/// * `atpkg-keys setup` / `join`: [`write_rest`], from before its premise check through
///   publication;
/// * `atpkg-keys machine-revoke`: one lock across read, revoke and publish;
/// * `targo --unverified ship provision`: its roster seeding (the kept-copy restore, the
///   channel/dist install, and the kept copy written after `authorize_cut`), in
///   `aterm-release`'s `provision::lock_roster_pair`, dropped before the in-process mint
///   re-takes it in `write_rest`;
/// * `targo --unverified ship recover`: `reconstruct_roster_assets`'s rewrite of `dist/`.
///
/// It is `flock`: it serializes across processes, per open file description, so a process
/// that takes it twice blocks on itself. Observers take [`lock_roster_read_only`], or apply
/// [`refuse_pending_roster_transaction`] alone when no rendezvous need exist.
pub struct RosterLock {
    path: String,
    _guard: aterm_update_core::FileLock,
}

impl RosterLock {
    /// Compare the guarded pair with the bytes the caller read (`None`: the caller planned a
    /// first roster, so both halves must still be absent). A method on the live guard, so
    /// this compare-and-swap cannot run without the serialization that gives it meaning.
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

/// Take the roster's writer lock, replaying any committed transaction forward first, so the
/// caller's first read sees a consistent pair.
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
        _guard: guard,
    })
}

/// Claim an existing roster lock without creating or repairing anything, for callers that
/// only observe: an observer reports an interrupted transaction, never completes one
/// (recovery is a write).
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
    let opened = file.metadata().map_err(|e| {
        concat(&[
            "inspect opened roster lock ",
            &lock_path,
            ": ",
            &e.to_string(),
        ])
    })?;
    let guard = aterm_update_core::FileLock::lock_open(file)
        .map_err(|e| concat(&["lock roster ", path, ": ", &e.to_string()]))?;
    #[cfg(test)]
    crate::fsio::window::reached("read-only claim locked", &lock_path);

    // Refuse a lock file unlinked or replaced after open: a read-only claim must not lock an
    // orphaned inode while a writer locks a new file under the same name.
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
        _guard: guard,
    })
}

/// Lock, read the pair, and publish in one call. Test-only: a real mutation holds one lock
/// across its own read, edit, sign and publish ([`publish_roster_locked`]); this states the
/// transaction layer's properties without the whole provisioning pipeline.
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

/// A roster transition durably committed but not yet installed under the two canonical
/// names: what a process death between the commit and the renames leaves, as a value.
///
/// [`publish_roster_locked`] is [`commit_roster_pair`] then [`CommittedRoster::complete`].
/// Dropping this value instead of completing it reproduces a killed process byte for byte
/// through the real commit path, which is how `aterm-release` (whose `targo --unverified ship
/// provision` seeds the same pair) tests its own crash recovery.
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

/// Check the caller's premise and commit the new pair to the redo log, touching neither
/// canonical name. `expected` is the pair the caller read under this lock: the CAS premise
/// and the predecessor the transaction records.
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

/// The state a verb has proved before any secret exists. Produced by [`preflight`],
/// consumed by [`plan`].
pub struct Preflight {
    verb: Verb,
    id: String,
    paths: Paths,
    /// The anchor file's exact current bytes, as text.
    pins_src: String,
    /// What `PAPER_MASTER_PUBKEYS` holds in the working tree right now.
    master_members: Vec<String>,
}

impl std::fmt::Debug for Preflight {
    /// Terse: this value travels beside a `MasterSeed`, and a derived impl would dump all of
    /// `pins.rs` into a panic message.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Preflight(")?;
        f.write_str(self.verb.name())?;
        f.write_str(", ")?;
        f.write_str(&self.id)?;
        f.write_str(")")
    }
}

/// Every refusal that can be made before a secret exists. Writes no file and needs no
/// master: an operator who will be refused is refused before typing the phrase, and `setup`
/// meeting an armed anchor stops before generating a second master, which would strand the
/// first (its only copy is on paper).
///
/// It does create missing directories (`$HOME/.aterm` on a fresh machine), so that failure
/// costs an error message instead of arriving after the anchor is armed.
pub fn preflight(verb: Verb, id: &str, paths: &Paths) -> Result<Preflight, String> {
    vet_machine_id(id)?;

    let raw = read_bytes(&paths.pins)
        .map_err(|e| concat(&["read ", &paths.pins, ": ", &e.to_string()]))?;
    let pins_src = String::from_utf8(raw).map_err(|_| {
        concat(&[
            &paths.pins,
            " is not valid UTF-8 — refusing to edit a trust anchor file this tool cannot \
             read",
        ])
    })?;

    // Reading the anchor is a shape check too: an unrecognised spelling stops the run now.
    let master = read_anchor(&pins_src, MASTER_ANCHOR)?;

    match verb {
        Verb::Setup => {
            // A second master is not an additional authority but a replacement for one
            // that exists only on paper.
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
            // The same refusal against the compiled-in anchor: a working tree can be
            // reverted, a shipped build cannot. Only for the discovered tree; an explicit
            // --pins names another tree (a fixture, another checkout), whose file anchor,
            // checked above, governs.
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
            // The roster must already be here. A committed master means a roster it signed
            // exists somewhere, but it lives in gitignored `dist/` and does not travel with
            // the repository, so a fresh checkout has an armed anchor and no roster.
            // Starting a new one at seq 1 under the real master would make two valid
            // same-sequence rosters: whichever is published de-authorizes every machine on
            // the other, the ratchet cannot see the fork, and a client meeting an
            // unattributable release is wedged. There is no safe guess, so ask for the file.
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

    // The directories this run writes into, created before anything secret exists.
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

    // After the verb checks: a second `setup` on the first machine trips both, and "a master
    // is already committed, use `join`" is the message that explains it. `create_new` at
    // write time remains the real guard against a race.
    if std::fs::metadata(&paths.key).is_ok() {
        return Err(concat(&[
            &paths.key,
            " already exists. This machine already has a key — minting a second one under \
             a new id is fine, but do it with --key pointing somewhere else, and never \
             overwrite a key the roster still names.",
        ]));
    }

    Ok(Preflight {
        verb,
        id: id.to_string(),
        paths: paths.clone(),
        pins_src,
        master_members: master.members,
    })
}

/// `join`'s transcription proof, part one: the typed master must be one the committed anchor
/// names. Part two is [`plan`]'s roster verification; both run before any write.
///
/// The orchestrator's call is a courtesy: [`plan`] re-runs this check for `join`, so a caller
/// that skips it (and supplies an attacker-signed `--roster`, which the roster proof alone
/// would accept) is still refused.
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

/// The whole result, computed in memory; producing it writes nothing.
pub struct Planned {
    verb: Verb,
    id: String,
    paths: Paths,
    /// The complete new text of `pins.rs`. `None` when the anchor needed no change —
    /// the idempotent re-run.
    pins_text: Option<String>,
    /// The anchor file's bytes as this run read them, so the write can prove it replaces the
    /// file it planned against.
    pins_src: String,
    /// The roster-pair bytes this plan was computed against (`None`: planned fresh), for
    /// [`write_rest`]'s premise check.
    roster_snapshot: Option<RosterSnapshot>,
    master_after: Vec<String>,
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
    /// Hand-written: this value holds `machine_pkcs8`, the machine's secret signing key.
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

/// Compute everything, write nothing: when this returns, the new `pins.rs`, the roster and
/// its master signature exist in memory, checked. What remains is writing decided bytes.
pub fn plan(pre: Preflight, seed: &MasterSeed, now: u64) -> Result<Planned, String> {
    let master_pubkey = seed.pubkey_b64()?;
    let master_fingerprint = seed.fingerprint()?;

    // `join`'s anchor proof, re-run here so it cannot be skipped: the roster verification
    // below proves the phrase only against the operator-supplied `--roster`, not against
    // the committed anchor.
    if pre.verb == Verb::Join {
        verify_master(&pre, seed)?;
    }

    // The machine keypair. Generated here, on this machine, and never copied off it.
    let (machine_pkcs8, machine_pubkey) = crate::generate()?;

    // The hierarchy must not collapse: a master that also signs releases would live on the
    // release machine, which the roster exists to prevent. Checked rather than assumed.
    if machine_pubkey == master_pubkey {
        return Err("the freshly minted machine key is the paper master; refusing".to_string());
    }

    // The anchor edit (setup only).
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

    // The roster. Loading verifies it under the master just derived: for `join` the second
    // half of the transcription proof; for `setup` nothing is on disk and it starts at
    // sequence 0. `MustExist` keeps the fresh path `setup`'s alone even if the roster
    // vanishes while the phrase is typed (see [`RosterExpectation`]).
    let expectation = match pre.verb {
        Verb::Join => RosterExpectation::MustExist,
        Verb::Setup => RosterExpectation::MayCreateFresh,
    };
    let (roster, roster_snapshot) =
        load_roster(&pre.paths.roster, &master_pubkey, now, expectation)?;
    let roster_was_fresh = roster_snapshot.is_none();

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
        roster_snapshot,
        master_after,
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

/// Write the anchor file, then prove it landed by re-reading the file, not the planned text:
/// a short write, a full filesystem or a concurrent editor can damage the file while the text
/// is correct. Runs before [`write_rest`], whose key and roster depend on a durable anchor.
///
/// The file must still be the one planned against. The text is a whole-file image of what
/// [`preflight`] read before the phrase was typed, and other agents and sessions write to
/// this repository: writing it over a moved file would silently revert their change (at
/// worst, a reviewed commit retiring a compromised key). A file that moved is a refusal, not
/// a merge.
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
        // Atomic: a truncating write could leave the anchor half-written while the operator
        // is told nothing was written.
        write_bytes_atomic(&planned.paths.pins, text.as_bytes())
            .map_err(|e| concat(&["write ", &planned.paths.pins, ": ", &e.to_string()]))?;
    }
    #[cfg(test)]
    crate::fsio::window::reached("anchor file written", &planned.paths.pins);
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
    verify_members(&back, MASTER_ANCHOR, &planned.master_after).map_err(damaged)
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
    pub roster_seq: u64,
    pub roster_valid_until: String,
    pub roster_machines: Vec<String>,
    /// Whether this run created the roster rather than extending one, reported aloud: a new
    /// roster is either the first ever (only `setup`) or a fork (fatal), and only the
    /// operator can tell which.
    pub roster_was_fresh: bool,
    pub pins_changed: bool,
}

impl std::fmt::Debug for Report {
    /// Terse, like [`Preflight`] and [`Planned`]: nothing here is secret, but a derived impl
    /// beside a `MasterSeed` stays derived while the fields change underneath it.
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
/// The secret key goes first: it is the step most likely to fail, and key-last could leave a
/// published roster naming a machine whose key was lost. Key first, the worst case is a key
/// nothing authorizes yet. `create_new` never clobbers an existing key.
///
/// The roster half runs under [`lock_roster`], and the recovery advice turns on the redo
/// transaction's commit point: before it, undo the key; after it, keep the key, because the
/// pair that authorizes it is durable and the next run installs it.
pub fn write_rest(planned: Planned) -> Result<Report, String> {
    // The recovery depends on the verb. `setup` armed the anchor before this ran
    // (`pins_text` is `Some` only then), so a failure leaving nothing authorized says to
    // discard the anchor and the paper and start over; a join (this CLI's, or `ship
    // provision`'s mint) only undoes its key and retries. A recoverable roster says to
    // commit the anchor, which names the master that signed it.
    let armed = planned.pins_text.is_some();
    let pins = &planned.paths.pins;
    let start_over = concat(&[
        "`git checkout -- ",
        pins,
        "`, destroy the paper you just wrote, fix the problem above, and run `atpkg-keys \
         setup` again.",
    ]);
    let when_armed = |text: &[&str]| if armed { concat(text) } else { String::new() };
    let armed_only = when_armed(&[
        " The anchor is armed but nothing else was written: ",
        &start_over,
    ]);
    // After `rm <key>`.
    let undo_tail = if armed {
        concat(&["`, ", &start_over])
    } else {
        "`.".to_string()
    };
    let then_commit = when_armed(&[" Then commit ", pins, "."]);

    // One crash-released lock, taken before the premise check and held through both roster
    // promotions, so two runs that read sequence N cannot both pass their check and publish
    // two N+1 lineages. Taking it also completes any redo transaction a dead run left, so the
    // premise is checked against a consistent pair.
    let roster_lock = lock_roster(&planned.paths.roster).map_err(|e| concat(&[&e, &armed_only]))?;

    // The roster's premise check, as `write_pins` does for the anchor: the signed roster
    // extends the pair `plan` read, so a pair that moved (or appeared, when planned fresh)
    // refuses before either local identity file is written.
    roster_lock
        .assert_snapshot(planned.roster_snapshot.as_ref())
        .map_err(|e| {
            if armed_only.is_empty() {
                concat(&[
                    &e,
                    " NOTHING HAS BEEN WRITTEN: this machine's key does not exist yet.",
                ])
            } else {
                concat(&[&e, &armed_only])
            }
        })?;
    // Preflight already created this directory; repeated for a `Planned` built without it,
    // as the library's tests do.
    let _ = ensure_parent_dir(&planned.paths.key);
    // Whole, 0600, fsynced and never over an existing key: a key present under its final
    // name but empty after a power loss is indistinguishable from one the roster authorizes.
    write_owner_file_create_new(&planned.paths.key, &planned.machine_pkcs8).map_err(|e| {
        concat(&[
            "atomically create and sync ",
            &planned.paths.key,
            " (refusing to overwrite an existing key): ",
            &e.to_string(),
            ". No roster was written, so nothing authorizes this machine; if a complete \
             key now exists, move it aside before retrying",
            &when_armed(&[". The anchor is armed: ", &start_over]),
        ])
    })?;

    let mut record = String::from("id = \"");
    record.push_str(&planned.id);
    record.push_str("\"\npubkey = \"");
    record.push_str(&planned.machine_pubkey);
    record.push_str("\"\nminted_at = \"");
    record.push_str(&aterm_types::rfc3339::format_rfc3339(planned.now));
    record.push_str("\"\n");
    // The record is checked and atomic: `atpkg-publish` refuses to build an index without a
    // parseable `machine.toml` (the id is not derivable from the key), and `join` cannot
    // restore it, since preflight refuses once `machine.key` exists and `roster_ops::add`
    // refuses a known id. A lost record leaves a machine that can neither publish nor
    // re-join.
    //
    // It precedes the roster publish, so a failure here leaves the same state as the key
    // write above: a key nothing authorizes.
    ensure_parent_dir(&planned.paths.machine_pub)
        .and_then(|()| write_bytes_atomic(&planned.paths.machine_pub, record.as_bytes()))
        .map_err(|e| {
            concat(&[
                "write this machine's public record ",
                &planned.paths.machine_pub,
                ": ",
                &e.to_string(),
                ".\nNothing authorizes this machine yet: the roster was not published, so \
                 the pair on disk is still the one from before this run. This machine's \
                 key file DOES exist; undo it before retrying, or the retry will mint a \
                 second key: `rm ",
                &planned.paths.key,
                &undo_tail,
            ])
        })?;

    // The roster last, under the lock already held. The redo commit is the line the recovery
    // advice turns on: before it, this machine holds an unauthorized key; after it, the
    // signed pair is durable and the operator must keep the key (deleting it would remove the
    // private half of a machine the recovered roster authorizes).
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
                &then_commit,
            ]),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && pair_is_target => concat(&[
                &e,
                "\nThe exact target roster pair IS installed, even though retiring the \
                 transaction could not be confirmed: KEEP this machine's key file and do \
                 NOT mint again. Rerun once the durability error above is fixed.",
                &then_commit,
            ]),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => concat(&[
                &e,
                "\nNothing authorizes this machine yet: no redo transaction was committed, \
                 so the roster pair is still the one from before this run. This machine's \
                 key file DOES exist; undo it before retrying, or the retry will mint a \
                 second key: `rm ",
                &planned.paths.key,
                &undo_tail,
            ]),
            Err(err) => concat(&[
                &e,
                "\nCould not inspect the possible redo transaction at ",
                &transaction,
                ": ",
                &err.to_string(),
                ". KEEP this machine's key file and investigate; do not mint again until \
                 the transaction state is known.",
                &when_armed(&[" Leave ", pins, " uncommitted until then."]),
            ]),
        }
    })?;

    Ok(Report {
        verb: planned.verb,
        id: planned.id,
        paths: planned.paths,
        master_pubkey: planned.master_pubkey,
        master_fingerprint: planned.master_fingerprint,
        master_after: planned.master_after,
        machine_pubkey: planned.machine_pubkey,
        roster_seq: planned.roster_seq,
        roster_valid_until: planned.roster_valid_until,
        roster_machines: planned.roster_machines,
        roster_was_fresh: planned.roster_was_fresh,
        pins_changed: planned.pins_text.is_some(),
    })
}

/// The closing output: DONE, then NEXT, facts and next actions only; each caveat rides as a
/// clause on the step it guards. Pure, so tests read what the operator reads.
#[must_use]
pub fn render_report(r: &Report) -> Vec<String> {
    let mut out = Vec::new();

    out.push(String::new());
    // Only `setup` edits the working tree (`pins_changed`); a join has nothing to commit, so
    // it carries no commit caveat.
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
    out.push(String::new());
    out.push("=== NEXT ===".to_string());
    let mut step = 0usize;
    let mut numbered = move |s: String| {
        step += 1;
        concat(&["  ", &step.to_string(), ". ", &s])
    };
    // The diff and commit steps exist only when there is a working-tree edit.
    if r.pins_changed {
        out.push(numbered(concat(&["review: git diff -- ", &r.paths.pins])));
    }
    if r.verb == Verb::Setup {
        out.push(numbered(
            "delete any test that asserts the anchor is empty".to_string(),
        ));
    }
    if r.pins_changed {
        out.push(numbered("commit — durable from here".to_string()));
    }
    out.push(numbered(concat(&[
        "cut from this machine — the master-signed roster authorizes '",
        &r.id,
        "'",
    ])));
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

    /// A private scratch directory for one test, removed when the test ends.
    fn scratch(name: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-keys-provision-{name}-"))
    }

    /// A `pins.rs` fixture with the master anchor empty: a copy, because the tests write to
    /// it.
    const PINS_FIXTURE: &str = "// Copyright 2026 Andrew Yates\n\
        // SPDX-License-Identifier: Apache-2.0\n\
        \n\
        /// The paper master. Empty here, and therefore INERT.\n\
        pub const PAPER_MASTER_PUBKEYS: &[&str] = &[];\n";

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
        let pre = preflight(Verb::Setup, id, paths)?;
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW)?;
        write_pins(&planned)?;
        write_rest(planned)
    }

    /// Run `join` end to end, with the phrase already in hand.
    fn run_join(paths: &Paths, id: &str, paper: &str) -> Result<Report, String> {
        let pre = preflight(Verb::Join, id, paths)?;
        let seed = seed_of(paper);
        verify_master(&pre, &seed)?;
        let planned = plan(pre, &seed, NOW)?;
        write_pins(&planned)?;
        write_rest(planned)
    }

    /// The first roster names this machine alone: no incumbent is seeded, since clients
    /// verify only under the roster.
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
        assert_eq!(
            roster.machines.len(),
            1,
            "the first roster names this machine alone"
        );
        assert_eq!(roster.machines[0].id, "m3");
        assert_eq!(roster.machines[0].pubkey, report.machine_pubkey);
        assert_eq!(roster.roster_seq, 1, "one bump per machine added");

        // The secret key is 0600 and holds a usable pkcs8 key.
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&paths.key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the machine key must be owner-only");
        let pkcs8 = std::fs::read(&paths.key).unwrap();
        assert_eq!(crate::pubkey_b64(&pkcs8).unwrap(), report.machine_pubkey);
    }

    #[test]
    fn setup_refuses_when_a_master_is_already_committed() {
        let dir = scratch("setup-refuses");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("the first setup succeeds");

        // Second run: the anchor is armed now.
        let before = std::fs::read_to_string(&paths.pins).unwrap();
        let err = preflight(Verb::Setup, "m11", &paths).unwrap_err();
        assert!(err.contains("ALREADY committed"), "{err}");
        assert!(err.contains("join"), "{err}");
        assert!(err.contains("strand"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&paths.pins).unwrap(),
            before,
            "a refused setup writes nothing"
        );

        // Negative control: an unarmed tree is accepted.
        let fresh = scratch("setup-refuses-control");
        let fresh_paths = paths_in(&fresh);
        write_fixture(&fresh_paths);
        assert!(preflight(Verb::Setup, "m11", &fresh_paths).is_ok());
    }

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

        let pre = preflight(Verb::Join, "m11", &second).expect("join preflight");
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
        assert!(
            !src.contains(first.machine_pubkey.as_str())
                && !src.contains(report.machine_pubkey.as_str()),
            "neither minted key appears in the anchor file"
        );
        assert_eq!(report.roster_seq, 2, "the roster advanced");
        assert_eq!(report.roster_machines, vec!["m3", "m11"]);
        assert!(!report.roster_was_fresh, "join edited the existing roster");
    }

    /// The default state of a second machine: the roster lives in gitignored `dist/`, so a
    /// fresh checkout has the anchor and no roster. A new seq-1 roster there would fork the
    /// real one.
    #[test]
    fn join_refuses_a_missing_roster_instead_of_forking_a_second_one() {
        let dir = scratch("join-no-roster");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup, on the first machine");

        // Machine 2: the committed anchor and no roster, as `git clone` leaves it.
        let dir2 = scratch("join-no-roster-second");
        let second = paths_in(&dir2);
        std::fs::copy(&paths.pins, &second.pins).expect("the anchor travels with the repo");
        assert!(
            !std::path::Path::new(&second.roster).exists(),
            "the roster does NOT travel with the repo; that is the premise"
        );

        let err = preflight(Verb::Join, "m11", &second).unwrap_err();
        assert!(err.contains("no roster at"), "{err}");
        assert!(err.contains("COPY"), "{err}");
        assert!(err.contains("will not start a second roster"), "{err}");
        assert!(
            !std::path::Path::new(&second.roster).exists(),
            "a refused join creates nothing"
        );
        assert!(!std::path::Path::new(&second.key).exists());

        // Negative control: with the real roster and signature copied across, the same call
        // is accepted.
        std::fs::copy(&paths.roster, &second.roster).unwrap();
        std::fs::copy(
            concat(&[&paths.roster, ".sig"]),
            concat(&[&second.roster, ".sig"]),
        )
        .unwrap();
        let report = run_join(&second, "m11", PAPER).expect("join, with the roster in hand");
        assert_eq!(
            report.roster_machines,
            vec!["m3", "m11"],
            "the roster was EXTENDED: every machine already on it survives"
        );
        assert!(!report.roster_was_fresh);
    }

    /// Preflight's refusal is not the enforcement: a roster that vanishes while the phrase
    /// is typed (a cleanup of gitignored `dist/`) must be refused in `plan`, not minted
    /// fresh. With `MayCreateFresh` for `join` in `plan`, this plan forks.
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
        let pre = preflight(Verb::Join, "m11", &second).expect("roster is present");
        // The window: both halves vanish while the phrase is typed (saved for the control).
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

        // Negative control: with the pair restored, the same sequence completes.
        std::fs::write(&second.roster, &saved_body).unwrap();
        std::fs::write(concat(&[&second.roster, ".sig"]), &saved_sig).unwrap();
        let report = run_join(&second, "m11", PAPER).expect("join with the pair in place");
        assert_eq!(report.roster_machines, vec!["m3", "m11"]);
    }

    /// A pair that moved between `plan` and the write (a concurrent join, mint, revoke or
    /// hand-copy) is refused before any write; publishing over it would fork the lineage.
    /// Without `write_rest`'s `assert_snapshot` call, the publish below succeeds.
    #[test]
    fn write_rest_refuses_a_roster_that_changed_since_the_plan_read_it() {
        let dir = scratch("roster-premise");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

        let pre = preflight(Verb::Join, "m11", &second).expect("preflight");
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW).expect("plan against the current pair");

        // A concurrent actor rewrites the pair while this run holds its plan.
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

        // The fresh-plan premise: `setup` planned the first roster, so a pair that appears
        // mid-run must not be overwritten.
        let fresh_dir = scratch("roster-premise-fresh");
        let fresh_paths = paths_in(&fresh_dir);
        write_fixture(&fresh_paths);
        let pre = preflight(Verb::Setup, "m3", &fresh_paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        write_pins(&planned).unwrap();
        std::fs::write(&fresh_paths.roster, b"someone else's first roster").unwrap();
        let err = write_rest(planned).unwrap_err();
        assert!(err.contains("APPEARED"), "{err}");
        assert!(
            !std::path::Path::new(&fresh_paths.key).exists(),
            "nothing written on the fresh-collision refusal either"
        );

        // Negative control: an untouched pair publishes.
        let control_dir = scratch("roster-premise-control");
        let control = paths_in(&control_dir);
        write_fixture(&control);
        run_setup(&control, "m3").expect("the same pipeline, unraced, completes");
    }

    /// The guarded publisher reads both halves under its lock before promoting either, so a
    /// directory squatting on the signature path is a premise failure and the document never
    /// moves. Promoting the halves in sequence without that read would land "NEW BODY" beside
    /// a signature no client verifies.
    #[test]
    fn a_broken_signature_target_leaves_the_roster_document_unchanged() {
        let dir = scratch("pair-rollback");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup publishes the first pair");
        let sig_path = concat(&[&paths.roster, ".sig"]);
        let old_body = std::fs::read(&paths.roster).unwrap();
        let old_sig = std::fs::read(&sig_path).unwrap();

        // A directory squats on the signature path, so the premise read of the pair fails.
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

        // The pair left behind still verifies, so the operator is not sent after the phrase.
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

        // A first publish with the same obstruction writes neither half.
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

        // Negative control: unblocked, the same publish replaces the pair whole.
        std::fs::remove_dir(&fresh_sig).unwrap();
        publish_roster(&fresh_roster, b"BODY", b"SIG").expect("publish with nothing in the way");
        assert_eq!(std::fs::read(&fresh_roster).unwrap(), b"BODY");
        assert_eq!(std::fs::read(&fresh_sig).unwrap(), b"SIG");

        // A lock makes the directory it guards (a fresh checkout has no `dist/`) and guards
        // that roster and no other, and a commit never replaces whatever holds the
        // transaction's fixed name, even an empty directory.
        let (guarded, other) = (fresh.join("dist/a.toml"), fresh.join("b.toml"));
        let guarded = guarded.to_str().unwrap();
        let lock = lock_roster(guarded).unwrap();
        assert!(publish_roster_locked(&lock, other.to_str().unwrap(), None, b"B", b"S").is_err());
        assert!(!other.exists());
        std::fs::create_dir(roster_txn_path(guarded)).unwrap();
        assert!(publish_roster_locked(&lock, guarded, None, b"A", b"S").is_err());
        assert!(!std::path::Path::new(guarded).exists());
    }

    /// The torn pair a crash between the two renames leaves (new body, old signature) is
    /// completed forward on the next lock acquisition, before any read. The crash is
    /// simulated by committing and promoting only the body. Without the
    /// `complete_roster_transaction` call in `lock_roster`, the signature stays stale.
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
        let recording = aterm_durable::seam::Recording::start();
        let transaction =
            begin_roster_transaction(roster, Some(&predecessor), b"new body", b"new sig").unwrap();
        // aterm-durable's counting seam: every redo-log member stays durable, the staged
        // directory's entries are only ordered ahead of the commit, and the commit's own
        // name is durable.
        let begun = recording.take();
        let (commit, rest) = begun.split_last().expect("flushes");
        let (staged, members) = rest.split_last().expect("the staged directory's flush");
        assert_eq!(*commit, aterm_durable::Flush::Durable, "{begun:?}");
        assert_eq!(*staged, aterm_durable::Flush::Ordered, "{begun:?}");
        assert!(
            !members.is_empty() && members.iter().all(|f| *f == aterm_durable::Flush::Durable),
            "{begun:?}"
        );
        write_bytes_atomic(roster, b"new body").unwrap();
        assert_eq!(
            std::fs::read(&sig_path).unwrap(),
            b"old sig",
            "the premise: a torn pair, which is what a verifier refuses"
        );

        // A filesystem that acknowledged the replay but kept an old half, the body or the
        // signature: the read-back catches either, and the redo log stays for the next lock
        // to replay.
        for (half, old) in [("", &b"old body"[..]), (".sig", &b"old sig"[..])] {
            let lost = crate::fsio::window::with(
                "roster pair replayed",
                move |body| std::fs::write(concat(&[body, half]), old).unwrap(),
                || lock_roster(roster).map(drop),
            );
            assert!(lost.is_err(), "a replay that lost {half:?} is not retired");
            assert!(std::path::Path::new(&transaction).exists());
        }

        let _ = recording.take();
        let _lock = lock_roster(roster).expect("lock acquisition replays the committed redo");
        let replayed = recording.take();
        assert!(
            replayed.ends_with(&[aterm_durable::Flush::Ordered, aterm_durable::Flush::Ordered]),
            "the retirement and its collection are only ordered: {replayed:?}"
        );
        assert!(
            replayed.contains(&aterm_durable::Flush::Durable),
            "the replayed pair is durable: {replayed:?}"
        );
        assert_eq!(std::fs::read(roster).unwrap(), b"new body");
        assert_eq!(std::fs::read(&sig_path).unwrap(), b"new sig");
        assert!(
            !std::path::Path::new(&transaction).exists(),
            "a completed transaction is retired, so it cannot replay a second time"
        );
        // ...and the retired directory is collected: nothing but the pair and its lock.
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "aterm-machines.toml",
                "aterm-machines.toml.lock",
                "aterm-machines.toml.sig"
            ]
        );
    }

    /// A canonical half that is neither the recorded predecessor nor the target (a newer
    /// roster copied in while this machine was down) is never overwritten. With
    /// `transaction_half_is_known` always `true`, the newer pair is lost.
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

        // A predecessor marker it cannot read is a damaged log, refused rather than read
        // as "published over nothing", even where nothing is on disk to protect.
        std::fs::write(
            std::path::Path::new(&transaction).join("predecessor-state"),
            b"torn",
        )
        .unwrap();
        std::fs::remove_file(roster).unwrap();
        std::fs::remove_file(&sig_path).unwrap();
        assert!(lock_roster(roster).is_err());
        assert!(!std::path::Path::new(roster).exists());
    }

    /// Recovery is a write, so the read-only claim neither creates the rendezvous nor
    /// replays a pending transaction. With `create(true)` or a `complete_roster_transaction`
    /// call in `lock_roster_read_only`, one refusal below becomes a mutation of the tree.
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
        // Negative control: with a rendezvous and no pending transaction, it succeeds.
        drop(lock_roster_read_only(roster).expect("an existing clean rendezvous is readable"));
        // A rendezvous replaced after the claim opened it is refused: the claim would hold
        // a lock on an orphaned inode while a writer locks the new one.
        let swapped = crate::fsio::window::with(
            "read-only claim locked",
            |lock| {
                let fresh = concat(&[lock, ".new"]);
                std::fs::write(&fresh, b"").unwrap();
                std::fs::rename(&fresh, lock).unwrap();
            },
            || lock_roster_read_only(roster).map(drop),
        );
        assert!(swapped.is_err(), "a replaced rendezvous is refused");

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

    /// `join` reads the roster in `plan` before taking the writer lock, so without this
    /// refusal a crashed publish surfaces as "does not verify under the master you typed".
    /// Without the `refuse_pending_roster_transaction` call in `load_roster`, it does.
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

        // Negative control: once a writer completes it forward, the read gets past it.
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

    /// The orchestrator's `verify_master` is a courtesy: skip it and hand `plan` a `--roster`
    /// signed by the wrong master, and the roster proof alone would pass. Without the
    /// `Verb::Join` check inside `plan`, the `unwrap_err` below fails.
    #[test]
    fn plan_reproves_the_anchor_even_when_the_orchestrator_skips_verify_master() {
        // The victim tree: armed with PAPER's master.
        let dir = scratch("plan-anchor-reproof");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        // The attacker's parallel lineage: a different tree, set up under OTHER_PAPER,
        // yielding a roster that verifies under the wrong master.
        let attacker_dir = scratch("plan-anchor-reproof-attacker");
        let attacker_paths = paths_in(&attacker_dir);
        write_fixture(&attacker_paths);
        let pre = preflight(Verb::Setup, "mx", &attacker_paths).unwrap();
        let wrong = seed_of(OTHER_PAPER);
        let planned = plan(pre, &wrong, NOW).unwrap();
        write_pins(&planned).unwrap();
        write_rest(planned).expect("the attacker's own lineage sets up fine");

        // The join, orchestrated without `verify_master` and with the substituted roster.
        let mut second = paths.clone();
        second.roster = attacker_paths.roster.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();
        let pre = preflight(Verb::Join, "m11", &second).expect("roster file exists");
        let err = plan(pre, &wrong, NOW).unwrap_err();
        assert!(
            err.contains("is NOT the master committed"),
            "the anchor containment must refuse inside plan itself: {err}"
        );
        assert!(!std::path::Path::new(&second.key).exists());

        // Negative control: the committed master with its own roster plans without a prior
        // `verify_master` call.
        let mut honest = paths.clone();
        honest.key = dir.join("m11b.key").to_str().unwrap().to_string();
        honest.machine_pub = dir.join("m11b.toml").to_str().unwrap().to_string();
        let pre = preflight(Verb::Join, "m11", &honest).unwrap();
        assert!(plan(pre, &seed_of(PAPER), NOW).is_ok());
    }

    /// `$HOME/.aterm` does not exist on a fresh machine, the one `setup` is for.
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

    /// Failing the roster step alone (its directory made unwritable after preflight) leaves
    /// a key nothing authorizes, never a signed roster naming a machine with no key.
    #[test]
    fn the_key_is_written_before_the_roster_that_names_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("key-before-roster");
        let mut paths = paths_in(&dir);
        let roster_dir = dir.join("dist");
        paths.roster = roster_dir.join("r.toml").to_str().unwrap().to_string();
        write_fixture(&paths);

        let pre = preflight(Verb::Setup, "m3", &paths).expect("preflight");
        let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
        write_pins(&planned).expect("the anchor is written");
        // Pre-create the lock inode (opening an existing file needs no directory write),
        // then revoke write permission so staging the roster transaction is what fails.
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

    /// The record is a checked write before the roster publish (see `write_rest`): revoking
    /// its directory's write permission after preflight fails that step alone, and the run
    /// must fail rather than publish a roster naming a machine with no record.
    #[test]
    fn a_record_that_cannot_be_written_fails_the_run_before_the_roster_is_published() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("record-write-fails");
        let mut paths = paths_in(&dir);
        let record_dir = dir.join("home");
        paths.machine_pub = record_dir
            .join("machine.toml")
            .to_str()
            .unwrap()
            .to_string();
        write_fixture(&paths);

        let pre = preflight(Verb::Setup, "m3", &paths).expect("preflight");
        let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
        write_pins(&planned).expect("the anchor is written");
        assert!(
            record_dir.is_dir(),
            "the premise: preflight created the record's directory"
        );
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_rest(planned);
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.unwrap_err();
        assert!(
            err.contains(&paths.machine_pub),
            "the message names the file that could not be written: {err}"
        );
        assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
        assert!(err.contains("rm "), "the recovery undoes the key: {err}");
        assert!(
            err.contains(
                "destroy the paper you just wrote, fix the problem above, and run \
                 `atpkg-keys setup` again."
            ),
            "setup armed the anchor, so its retry starts over: {err}"
        );
        assert!(
            std::path::Path::new(&paths.key).exists(),
            "the key was written first, which is what makes the recovery the two commands \
             the message names"
        );
        assert!(
            !std::path::Path::new(&paths.roster).exists()
                && !std::path::Path::new(&concat(&[&paths.roster, ".sig"])).exists(),
            "NO signed document names a machine whose record could not be written"
        );
        assert!(
            !std::path::Path::new(&paths.machine_pub).exists(),
            "and no half-written record is left behind"
        );
    }

    /// `join` never touches the anchor, so its failure says only `rm <key>`; `setup` armed
    /// the anchor, so its failure before any key exists says so, not "NOTHING HAS BEEN
    /// WRITTEN".
    #[test]
    fn a_failure_that_authorized_nothing_undoes_only_what_its_verb_did() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("retry-names-the-verb");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        let record_dir = dir.join("m11-home");
        second.machine_pub = record_dir
            .join("machine.toml")
            .to_str()
            .unwrap()
            .to_string();
        let pre = preflight(Verb::Join, "m11", &second).expect("preflight");
        let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
        write_pins(&planned).expect("a join leaves the anchor as it is");
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = write_rest(planned);
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let err = result.unwrap_err();
        assert!(err.contains("Nothing authorizes this machine yet"), "{err}");
        assert!(
            err.ends_with(&concat(&["`rm ", &second.key, "`."])),
            "a join undoes its key and retries: {err}"
        );
        assert!(
            !err.contains("git checkout") && !err.contains("destroy the paper"),
            "a join never armed the anchor: {err}"
        );

        // `setup`, refused at the roster premise: the anchor is armed, no key exists.
        let dir = scratch("retry-setup-premise");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).expect("preflight");
        let planned = plan(pre, &seed_of(PAPER), NOW).expect("plan");
        write_pins(&planned).expect("the anchor is written");
        std::fs::write(&paths.roster, b"# appeared after the plan\n").unwrap();
        let err = write_rest(planned).unwrap_err();
        assert!(
            err.contains("The anchor is armed but nothing else was written: `git checkout -- "),
            "{err}"
        );
        assert!(!err.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
        assert!(
            !std::path::Path::new(&paths.key).exists(),
            "refused before the key write"
        );
    }

    /// The plan is built from what preflight read, before the phrase is typed; an edit
    /// landing in that window must not be reverted by the write.
    #[test]
    fn write_pins_refuses_a_file_that_changed_under_it() {
        let dir = scratch("stale-snapshot");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();

        // A reviewed commit lands while the run is in flight.
        const REPLACEMENT: &str = "Edited by a reviewed commit that landed in flight.";
        let concurrent = PINS_FIXTURE.replace("Empty here, and therefore INERT.", REPLACEMENT);
        assert_ne!(concurrent, PINS_FIXTURE, "the fixture must actually differ");
        std::fs::write(&paths.pins, &concurrent).unwrap();

        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        let err = write_pins(&planned).unwrap_err();
        assert!(err.contains("CHANGED ON DISK"), "{err}");
        assert!(err.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&paths.pins).unwrap(),
            concurrent,
            "the concurrent edit survives"
        );

        // Negative control: planned against the current bytes, the write is accepted.
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        write_pins(&planned).expect("a plan built from the current bytes writes");
        let after = std::fs::read_to_string(&paths.pins).unwrap();
        assert!(
            after.contains(REPLACEMENT),
            "the concurrent edit was not reverted: {after}"
        );
    }

    /// The anchor check catches the wrong paper first; the roster verification would too.
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

        let pre = preflight(Verb::Join, "m11", &second).expect("preflight is fine");
        let wrong = seed_of(OTHER_PAPER);
        let err = verify_master(&pre, &wrong).unwrap_err();
        assert!(err.contains("is NOT the master committed"), "{err}");
        assert!(err.contains("Nothing has been written"), "{err}");

        // `plan` re-runs the anchor proof itself, so skipping `verify_master` does not help.
        let err = plan(pre, &wrong, NOW).unwrap_err();
        assert!(err.contains("is NOT the master committed"), "{err}");

        // The independent roster check refuses the wrong master too.
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

        // Negative control: the right paper is accepted on the same inputs.
        let pre = preflight(Verb::Join, "m11", &second).unwrap();
        assert!(verify_master(&pre, &seed_of(PAPER)).is_ok());
    }

    /// The counterpart of `setup`'s refusal, so neither verb runs where the other belongs.
    #[test]
    fn join_refuses_when_no_master_is_committed() {
        let dir = scratch("join-unarmed");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let err = preflight(Verb::Join, "m3", &paths).unwrap_err();
        assert!(err.contains("no paper master is committed"), "{err}");
        assert!(err.contains("setup"), "{err}");
        // Negative control: setup is allowed here.
        assert!(preflight(Verb::Setup, "m3", &paths).is_ok());
    }

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
        // The machine key appears nowhere in the anchor file.
        assert!(!src.contains(report.machine_pubkey.as_str()));
    }

    /// A second run on a machine whose key exists is refused at preflight, before anything
    /// is generated.
    #[test]
    fn an_existing_machine_key_is_refused_before_anything_is_generated() {
        let dir = scratch("existing-key");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");
        let err = preflight(Verb::Join, "m3", &paths).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(
            err.contains("never overwrite a key the roster still names"),
            "{err}"
        );
    }

    #[test]
    fn setup_refuses_to_start_over_an_existing_roster() {
        let dir = scratch("setup-roster-exists");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        std::fs::write(&paths.roster, b"schema = 1\n").unwrap();
        let err = preflight(Verb::Setup, "m3", &paths).unwrap_err();
        assert!(err.contains("roster already exists"), "{err}");

        // A lone SIGNATURE is a half-published pair, not an absent roster: the fresh path
        // refuses it, so no first roster is ever planned over one.
        std::fs::remove_file(&paths.roster).unwrap();
        std::fs::write(concat(&[&paths.roster, ".sig"]), b"sig").unwrap();
        let master = seed_of(PAPER).pubkey_b64().unwrap();
        let expect = RosterExpectation::MayCreateFresh;
        assert!(load_roster(&paths.roster, &master, NOW, expect).is_err());
    }

    /// A machine id is 1-32 ASCII letters, digits, `-` and `_`.
    #[test]
    fn a_machine_id_is_vetted_at_its_bounds() {
        let (longest, too_long) = ("a".repeat(32), "a".repeat(33));
        for (id, ok) in [
            ("m3-2026b_x", true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("m 3", false),
            ("m3\u{e9}", false),
        ] {
            assert_eq!(vet_machine_id(id).is_ok(), ok, "{id:?}");
        }
    }

    /// The anchor file is FOUND from inside the checkout: this crate's directory is two
    /// levels below the root.
    #[test]
    fn the_pins_file_is_discovered_from_inside_the_checkout() {
        let found = discover_pins_path_from(env!("CARGO_MANIFEST_DIR").into()).unwrap();
        let shipped = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../aterm-update-core/src/pins.rs"
        );
        assert_eq!(
            std::fs::canonicalize(found).unwrap(),
            std::fs::canonicalize(shipped).unwrap()
        );
    }

    #[test]
    fn an_unrecognised_pins_file_stops_the_run_at_preflight() {
        let dir = scratch("bad-pins");
        let paths = paths_in(&dir);
        std::fs::write(&paths.pins, "// nothing resembling an anchor\n").unwrap();
        let err = preflight(Verb::Setup, "m3", &paths).unwrap_err();
        assert!(err.contains("will not guess"), "{err}");
    }

    #[test]
    fn write_pins_reports_a_write_that_did_not_land() {
        let dir = scratch("verify-write");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW).unwrap();

        // An edit that lands between the write and its read-back is reported even though
        // the master's members are still the planned ones: the file must hold exactly the
        // bytes written, or another change rides along unseen.
        let landed = crate::fsio::window::with(
            "anchor file written",
            |pins| {
                let ours = std::fs::read_to_string(pins).unwrap();
                let theirs = ours.replace("therefore INERT.", "edited in the gap.");
                assert_ne!(theirs, ours, "the edit must change the file");
                std::fs::write(pins, theirs).unwrap();
            },
            || write_pins(&planned),
        );
        assert!(
            landed.is_err(),
            "a foreign edit after the write is reported"
        );
        std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();

        // Honest first: the ordinary path succeeds.
        write_pins(&planned).expect("the ordinary write verifies");

        // Put the unarmed original back and verify again: a pass here would make the check
        // decorative.
        std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();
        let planned2 = {
            let pre = preflight(
                Verb::Setup,
                "m3-b",
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

    /// THE SECRET IS NOT IN THE CLOSING OUTPUT. The report is built from public
    /// identities only (the machine's public key is in it), and this asserts it
    /// rather than trusting the type system to have.
    #[test]
    fn the_report_carries_no_secret_material() {
        let dir = scratch("report");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let report = run_setup(&paths, "m3").expect("setup");
        let text = render_report(&report).join("\n");
        assert!(text.contains(&report.machine_pubkey), "{text}");
        assert!(!text.contains(PAPER), "{text}");
        assert!(!text.contains(&PAPER[..16]), "{text}");
    }

    #[test]
    fn debug_renderings_carry_no_secret_material() {
        let dir = scratch("debug-redaction");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
        assert_eq!(format!("{pre:?}"), "Preflight(setup, m3)");
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW).unwrap();
        let rendered = format!("{planned:?}");
        assert_eq!(rendered, "Planned(setup, m3, secret=<redacted>)");
        assert!(!rendered.contains(PAPER), "{rendered}");
        assert_eq!(format!("{seed:?}"), "MasterSeed(<redacted>)");
    }

    /// Every file the run wrote is scanned for the phrase, any 16-character run of it, and
    /// its raw 32-byte seed.
    #[test]
    fn no_file_written_by_a_run_contains_the_phrase_or_its_seed() {
        let dir = scratch("no-phrase-on-disk");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        // The raw seed, by an independent base32 decode (`MasterSeed` has no accessor).
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
