// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Provisioning flows for `atpkg-keys setup` and `join`.

use crate::fsio::{
    concat, ensure_parent_dir, read_bytes, sync_parent, sync_parent_ordered, write_bytes_atomic,
    write_owner_file_create_new,
};
use crate::master::MasterSeed;
use crate::pins_edit::{
    Edit, MASTER_ANCHOR, MAX_MASTER_MEMBERS, append_member, read_anchor, verify_members,
};
use aterm_update_core::roster::{Roster, RosterReject};

/// Default relative path for the roster and its detached master signature.
pub const DEFAULT_ROSTER: &str = "dist/aterm-machines.toml";

/// Release ring directory path relative to `$HOME`.
pub const RELEASE_RING_REL: &str = ".aterm/release-ring";

/// Machine secret key path relative to `$HOME`.
pub const MACHINE_KEY_REL: &str = ".aterm/release-ring/machine.key";

/// Machine public record path relative to `$HOME`.
pub const MACHINE_PUB_REL: &str = ".aterm/release-ring/machine.toml";

/// Release-credentials profile path relative to `$HOME`.
pub const RELEASE_CREDENTIALS_REL: &str = ".aterm/release-ring/release-credentials.toml";

/// Installed release roster path relative to `$HOME`.
pub const RELEASE_ROSTER_REL: &str = ".aterm/release-ring/roster/aterm-machines.toml";

/// Anchor file path relative to the workspace root.
pub const PINS_REL: &str = "crates/aterm-update-core/src/pins.rs";

/// Provisioning verb being executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// First machine: generate the master and arm the anchor.
    Setup,
    /// Later machine (or re-run): verify the typed master and add or rotate this machine.
    Join,
}

impl Verb {
    /// Command-line name of the verb.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Join => "join",
        }
    }
}

/// File paths touched by a provisioning run.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Path to `crates/aterm-update-core/src/pins.rs`.
    pub pins: String,
    /// Path to the roster document (`<roster>.sig` holds its signature).
    pub roster: String,
    /// Path to this machine's secret key file (`0600`).
    pub key: String,
    /// Path to this machine's public record.
    pub machine_pub: String,
    /// Whether `pins` was passed explicitly via `--pins` rather than discovered.
    pub pins_explicit: bool,
}

/// Resolve `$HOME/<rel>`.
pub fn home_path(rel: &str) -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let mut p = home;
    p.push('/');
    p.push_str(rel);
    Ok(p)
}

/// Find `crates/aterm-update-core/src/pins.rs` by walking up from the current directory.
pub fn discover_pins_path() -> Result<String, String> {
    let dir = std::env::current_dir()
        .map_err(|e| concat(&["cannot read the current directory: ", &e.to_string()]))?;
    discover_pins_path_from(dir)
}

fn discover_pins_path_from(mut dir: std::path::PathBuf) -> Result<String, String> {
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

/// Validate a machine id.
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

/// Printable name for a [`RosterReject`].
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

/// Whether an absent roster may be treated as the first mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterExpectation {
    /// The roster must already exist (`join`).
    MustExist,
    /// An absent roster starts the first mint (`setup`).
    MayCreateFresh,
}

/// On-disk bytes of the roster pair a run planned against.
#[derive(Debug, Clone)]
pub struct RosterSnapshot {
    pub raw: Vec<u8>,
    pub sig: Vec<u8>,
}

/// Read, verify under `master_pubkey`, and parse the roster pair.
pub fn load_roster(
    path: &str,
    master_pubkey: &str,
    now: u64,
    expect: RosterExpectation,
) -> Result<(Roster, Option<RosterSnapshot>), String> {
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

fn roster_txn_path(path: &str) -> String {
    concat(&[path, ".atpkg-keys.txn"])
}

/// Refuse to read a roster while a committed transaction is pending.
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

fn write_txn_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    aterm_durable::durable(&file)
}

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

fn transaction_half_is_known(
    current: Option<&[u8]>,
    predecessor: Option<&[u8]>,
    target: &[u8],
) -> bool {
    current == predecessor || current == Some(target)
}

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
    let _ = sync_parent_ordered(&retired);
    Ok(())
}

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

/// Advisory writer lock held across roster read, edit, sign, and publish.
pub struct RosterLock {
    path: String,
    _guard: aterm_update_core::FileLock,
}

impl RosterLock {
    /// Verify that the on-disk roster pair still matches `expected`.
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

/// Acquire the roster writer lock, replaying any committed transaction first.
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

/// Acquire an existing roster lock without creating or repairing files.
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

/// A roster transaction committed to the redo directory.
#[must_use = "a committed roster transaction that is never completed leaves the pair for \
              the next writer to recover; drop it deliberately or call complete()"]
pub struct CommittedRoster {
    path: String,
    transaction: String,
}

impl CommittedRoster {
    /// Redo directory path holding the signed roster pair.
    #[must_use]
    pub fn transaction_path(&self) -> &str {
        &self.transaction
    }

    /// Install both canonical halves from the commit and remove the redo directory.
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

/// Verify `expected` and commit the new signed pair to the redo log.
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

/// Publish an already-signed roster pair while holding `lock`.
pub fn publish_roster_locked(
    lock: &RosterLock,
    path: &str,
    expected: Option<&RosterSnapshot>,
    bytes: &[u8],
    sig: &[u8],
) -> Result<(), String> {
    commit_roster_pair(lock, path, expected, bytes, sig)?.complete()
}

/// State validated by [`preflight`] before any secret is read or generated.
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Preflight(")?;
        f.write_str(self.verb.name())?;
        f.write_str(", ")?;
        f.write_str(&self.id)?;
        f.write_str(")")
    }
}

/// Validate paths and pre-existing files before reading or generating any secret.
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

/// Verify that `seed` derives a master key committed in `pre` (`join`).
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

/// Complete in-memory provisioning plan before writing any file.
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Planned(")?;
        f.write_str(self.verb.name())?;
        f.write_str(", ")?;
        f.write_str(&self.id)?;
        f.write_str(", secret=<redacted>)")
    }
}

fn today(now: u64) -> String {
    let stamp = aterm_types::rfc3339::format_rfc3339(now);
    stamp.get(..10).unwrap_or("").to_string()
}

/// Compute the new `pins.rs`, machine key, and signed roster in memory.
pub fn plan(pre: Preflight, seed: &MasterSeed, now: u64) -> Result<Planned, String> {
    let master_pubkey = seed.pubkey_b64()?;
    let master_fingerprint = seed.fingerprint()?;

    if pre.verb == Verb::Join {
        verify_master(&pre, seed)?;
    }

    let (machine_pkcs8, machine_pubkey) = crate::generate()?;

    if machine_pubkey == master_pubkey {
        return Err("the freshly minted machine key is the paper master; refusing".to_string());
    }

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

/// A [`write_pins`] failure.
#[derive(Debug)]
pub struct PinsError {
    pub message: String,
    /// This run's bytes may have reached the anchor file, which may now name the planned master.
    pub landed: bool,
}

impl std::fmt::Display for PinsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<PinsError> for String {
    fn from(e: PinsError) -> Self {
        e.message
    }
}

/// Write `pins.rs` and verify the written file.
pub fn write_pins(planned: &Planned) -> Result<(), PinsError> {
    write_pins_with_sync(planned, sync_parent)
}

fn write_pins_with_sync(
    planned: &Planned,
    sync: impl FnOnce(&str) -> std::io::Result<()>,
) -> Result<(), PinsError> {
    let unwritten = |message| PinsError {
        message,
        landed: false,
    };
    if let Some(text) = &planned.pins_text {
        let current = read_bytes(&planned.paths.pins).map_err(|e| {
            unwritten(concat(&[
                "re-read ",
                &planned.paths.pins,
                ": ",
                &e.to_string(),
            ]))
        })?;
        if current != planned.pins_src.as_bytes() {
            return Err(unwritten(concat(&[
                &planned.paths.pins,
                " CHANGED ON DISK since this run read it (something else edited it — a \
                 `git checkout`, a rebase, an editor, another agent). This run's edit was \
                 computed against the older bytes, so writing it now would silently revert \
                 that change. NOTHING HAS BEEN WRITTEN. Read `git diff` over the anchor \
                 file, then run this command again against what the file says now.",
            ])));
        }
        crate::fsio::write_bytes_atomic_with_sync(&planned.paths.pins, text.as_bytes(), sync)
            .map_err(|e| PinsError {
                message: concat(&["write ", &planned.paths.pins, ": ", &e.error.to_string()]),
                landed: e.renamed,
            })?;
    }
    #[cfg(test)]
    crate::fsio::window::reached("anchor file written", &planned.paths.pins);
    let after = |message| PinsError {
        message,
        landed: planned.pins_text.is_some(),
    };
    let raw = read_bytes(&planned.paths.pins).map_err(|e| {
        after(concat(&[
            "re-read ",
            &planned.paths.pins,
            " to verify the write: ",
            &e.to_string(),
        ]))
    })?;
    let back = String::from_utf8(raw).map_err(|_| {
        after(concat(&[
            &planned.paths.pins,
            " is not valid UTF-8 after writing",
        ]))
    })?;
    if let Some(text) = &planned.pins_text
        && back != *text
    {
        return Err(after(concat(&[
            &planned.paths.pins,
            " does not hold the bytes that were just written — `git diff` it before doing \
             anything else",
        ])));
    }
    let damaged = |e: String| {
        after(concat(&[
            "after writing, ",
            &e,
            " — the anchor file may be damaged; `git diff` ",
            &planned.paths.pins,
            " before doing anything else",
        ]))
    };
    verify_members(&back, MASTER_ANCHOR, &planned.master_after).map_err(damaged)
}

/// Completed provisioning run summary for [`render_report`].
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
    /// Whether this run created a fresh roster rather than extending an existing one.
    pub roster_was_fresh: bool,
    pub pins_changed: bool,
}

impl std::fmt::Debug for Report {
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

/// Write the machine files and publish the signed roster pair.
pub fn write_rest(planned: Planned) -> Result<Report, String> {
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
    let undo_tail = if armed {
        concat(&["`, ", &start_over])
    } else {
        "`.".to_string()
    };
    let then_commit = when_armed(&[" Then commit ", pins, "."]);

    let roster_lock = lock_roster(&planned.paths.roster).map_err(|e| concat(&[&e, &armed_only]))?;

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
    let _ = ensure_parent_dir(&planned.paths.key);
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

/// Format the human-readable provisioning report lines.
#[must_use]
pub fn render_report(r: &Report) -> Vec<String> {
    let mut out = Vec::new();

    out.push(String::new());
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

    const PAPER: &str = "0123456789abcdefghjkmnpqrstvwxyz0123456789abcdefghj0";
    const OTHER_PAPER: &str = "zyxwvtsrqpnmkjhgfedcba9876543210zyxwvtsrqpnmkjhgfed0";

    const NOW: u64 = 1_785_801_600;

    fn scratch(name: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-keys-provision-{name}-"))
    }

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

    fn run_setup(paths: &Paths, id: &str) -> Result<Report, String> {
        let pre = preflight(Verb::Setup, id, paths)?;
        let seed = seed_of(PAPER);
        let planned = plan(pre, &seed, NOW)?;
        write_pins(&planned)?;
        write_rest(planned)
    }

    fn run_join(paths: &Paths, id: &str, paper: &str) -> Result<Report, String> {
        let pre = preflight(Verb::Join, id, paths)?;
        let seed = seed_of(paper);
        verify_master(&pre, &seed)?;
        let planned = plan(pre, &seed, NOW)?;
        write_pins(&planned)?;
        write_rest(planned)
    }

    #[test]
    fn setup_arms_the_anchor_mints_the_machine_and_signs_the_roster() {
        let dir = scratch("setup-happy");
        let paths = paths_in(&dir);
        write_fixture(&paths);

        let report = run_setup(&paths, "m3").expect("setup completes");

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

    #[test]
    fn join_refuses_a_missing_roster_instead_of_forking_a_second_one() {
        let dir = scratch("join-no-roster");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup, on the first machine");

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

    #[test]
    fn join_refuses_a_roster_that_vanished_between_preflight_and_plan() {
        let dir = scratch("join-vanished-roster");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let mut second = paths.clone();
        second.key = dir.join("m11.key").to_str().unwrap().to_string();
        second.machine_pub = dir.join("m11.toml").to_str().unwrap().to_string();

        let pre = preflight(Verb::Join, "m11", &second).expect("roster is present");
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

        std::fs::write(&second.roster, &saved_body).unwrap();
        std::fs::write(concat(&[&second.roster, ".sig"]), &saved_sig).unwrap();
        let report = run_join(&second, "m11", PAPER).expect("join with the pair in place");
        assert_eq!(report.roster_machines, vec!["m3", "m11"]);
    }

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

        let control_dir = scratch("roster-premise-control");
        let control = paths_in(&control_dir);
        write_fixture(&control);
        run_setup(&control, "m3").expect("the same pipeline, unraced, completes");
    }

    #[test]
    fn a_broken_signature_target_leaves_the_roster_document_unchanged() {
        let dir = scratch("pair-rollback");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup publishes the first pair");
        let sig_path = concat(&[&paths.roster, ".sig"]);
        let old_body = std::fs::read(&paths.roster).unwrap();
        let old_sig = std::fs::read(&sig_path).unwrap();
        let snapshot = RosterSnapshot {
            raw: old_body.clone(),
            sig: old_sig.clone(),
        };

        std::fs::remove_file(&sig_path).unwrap();
        std::fs::create_dir(&sig_path).unwrap();
        let lock = lock_roster(&paths.roster).unwrap();
        let err = publish_roster_locked(
            &lock,
            &paths.roster,
            Some(&snapshot),
            b"NEW BODY",
            b"NEW SIG",
        )
        .unwrap_err();
        assert!(err.contains("CHANGED ON DISK"), "{err}");
        assert!(!std::path::Path::new(&roster_txn_path(&paths.roster)).exists());
        drop(lock);
        assert_eq!(
            std::fs::read(&paths.roster).unwrap(),
            old_body,
            "the document must still be the pair's previous bytes"
        );
        std::fs::remove_dir(&sig_path).unwrap();
        std::fs::write(&sig_path, &old_sig).unwrap();

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

        let fresh = scratch("pair-rollback-fresh");
        let fresh_roster = fresh
            .join("aterm-machines.toml")
            .to_str()
            .unwrap()
            .to_string();
        let fresh_sig = concat(&[&fresh_roster, ".sig"]);
        std::fs::create_dir(&fresh_sig).unwrap();
        let fresh_lock = lock_roster(&fresh_roster).unwrap();
        let err =
            publish_roster_locked(&fresh_lock, &fresh_roster, None, b"BODY", b"SIG").unwrap_err();
        assert!(err.contains("APPEARED"), "{err}");
        assert!(!std::path::Path::new(&roster_txn_path(&fresh_roster)).exists());
        assert!(
            !std::path::Path::new(&fresh_roster).exists(),
            "a first publish that failed leaves nothing, not a signatureless document"
        );

        std::fs::remove_dir(&fresh_sig).unwrap();
        publish_roster_locked(&fresh_lock, &fresh_roster, None, b"BODY", b"SIG")
            .expect("publish with nothing in the way");
        drop(fresh_lock);
        assert_eq!(std::fs::read(&fresh_roster).unwrap(), b"BODY");
        assert_eq!(std::fs::read(&fresh_sig).unwrap(), b"SIG");

        let (guarded, other) = (fresh.join("dist/a.toml"), fresh.join("b.toml"));
        let guarded = guarded.to_str().unwrap();
        let lock = lock_roster(guarded).unwrap();
        assert!(publish_roster_locked(&lock, other.to_str().unwrap(), None, b"B", b"S").is_err());
        assert!(!other.exists());
        std::fs::create_dir(roster_txn_path(guarded)).unwrap();
        assert!(publish_roster_locked(&lock, guarded, None, b"A", b"S").is_err());
        assert!(!std::path::Path::new(guarded).exists());
    }

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
        drop(lock_roster_read_only(roster).expect("an existing clean rendezvous is readable"));
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

    #[test]
    fn a_pending_roster_transaction_is_named_instead_of_a_mistyped_phrase() {
        let dir = scratch("pending-txn-read");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup publishes the first pair");
        let master_pub = seed_of(PAPER).pubkey_b64().unwrap();
        let sig_path = concat(&[&paths.roster, ".sig"]);

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

    #[test]
    fn plan_reproves_the_anchor_even_when_the_orchestrator_skips_verify_master() {
        let dir = scratch("plan-anchor-reproof");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

        let attacker_dir = scratch("plan-anchor-reproof-attacker");
        let attacker_paths = paths_in(&attacker_dir);
        write_fixture(&attacker_paths);
        let pre = preflight(Verb::Setup, "mx", &attacker_paths).unwrap();
        let wrong = seed_of(OTHER_PAPER);
        let planned = plan(pre, &wrong, NOW).unwrap();
        write_pins(&planned).unwrap();
        write_rest(planned).expect("the attacker's own lineage sets up fine");

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

        let mut honest = paths.clone();
        honest.key = dir.join("m11b.key").to_str().unwrap().to_string();
        honest.machine_pub = dir.join("m11b.toml").to_str().unwrap().to_string();
        let pre = preflight(Verb::Join, "m11", &honest).unwrap();
        assert!(plan(pre, &seed_of(PAPER), NOW).is_ok());
    }

    #[test]
    fn a_run_creates_the_directories_its_key_and_roster_need() {
        let dir = scratch("missing-dirs");
        let paths = Paths {
            pins: dir.join("pins.rs").to_str().unwrap().to_string(),
            roster: dir
                .join("dist/aterm-machines.toml")
                .to_str()
                .unwrap()
                .to_string(),
            key: dir
                .join("home/.aterm/release-ring/machine.key")
                .to_str()
                .unwrap()
                .to_string(),
            machine_pub: dir
                .join("home/.aterm/release-ring/machine.toml")
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

    #[test]
    fn write_pins_refuses_a_file_that_changed_under_it() {
        let dir = scratch("stale-snapshot");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();

        const REPLACEMENT: &str = "Edited by a reviewed commit that landed in flight.";
        let concurrent = PINS_FIXTURE.replace("Empty here, and therefore INERT.", REPLACEMENT);
        assert_ne!(concurrent, PINS_FIXTURE, "the fixture must actually differ");
        std::fs::write(&paths.pins, &concurrent).unwrap();

        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        let err = write_pins(&planned).unwrap_err();
        assert!(err.message.contains("CHANGED ON DISK"), "{err}");
        assert!(err.message.contains("NOTHING HAS BEEN WRITTEN"), "{err}");
        assert!(!err.landed, "a refused write leaves the anchor unarmed");
        assert_eq!(
            std::fs::read_to_string(&paths.pins).unwrap(),
            concurrent,
            "the concurrent edit survives"
        );

        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        write_pins(&planned).expect("a plan built from the current bytes writes");
        let after = std::fs::read_to_string(&paths.pins).unwrap();
        assert!(
            after.contains(REPLACEMENT),
            "the concurrent edit was not reverted: {after}"
        );
    }

    #[test]
    fn write_pins_says_whether_a_failed_write_reached_the_anchor() {
        let dir = scratch("failed-write");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
        let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
        let saved = dir.join("pins-before-rename.rs");
        let (pins, backup) = (paths.pins.clone(), saved.clone());
        let err = crate::fsio::window::with(
            "temp name drawn",
            move |_| {
                std::fs::rename(&pins, &backup).unwrap();
                std::fs::create_dir(&pins).unwrap();
            },
            || write_pins(&planned),
        )
        .unwrap_err();
        std::fs::remove_dir(&paths.pins).unwrap();
        std::fs::rename(&saved, &paths.pins).unwrap();
        assert!(
            !err.landed,
            "a refused rename did not publish this run's bytes: {err}"
        );
        assert_eq!(std::fs::read_to_string(&paths.pins).unwrap(), PINS_FIXTURE);

        for restore_original in [false, true] {
            write_fixture(&paths);
            let pre = preflight(Verb::Setup, "m3", &paths).unwrap();
            let planned = plan(pre, &seed_of(PAPER), NOW).unwrap();
            let expected = planned.pins_text.as_deref().unwrap();
            let err = write_pins_with_sync(&planned, |pins| {
                assert_eq!(std::fs::read_to_string(pins).unwrap(), expected);
                if restore_original {
                    std::fs::write(pins, PINS_FIXTURE).unwrap();
                }
                Err(std::io::Error::other("injected parent sync failure"))
            })
            .unwrap_err();
            assert!(
                err.landed,
                "an actual rename is reported even after restoration: {err}"
            );
            assert!(
                err.message.contains("injected parent sync failure"),
                "{err}"
            );
            assert_eq!(
                std::fs::read_to_string(&paths.pins).unwrap(),
                if restore_original {
                    PINS_FIXTURE
                } else {
                    expected
                },
            );
        }
        assert!(!std::path::Path::new(&paths.roster).exists());
        assert!(!std::path::Path::new(&paths.key).exists());
        assert!(!std::path::Path::new(&paths.machine_pub).exists());
    }

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

        let err = plan(pre, &wrong, NOW).unwrap_err();
        assert!(err.contains("is NOT the master committed"), "{err}");

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

        let pre = preflight(Verb::Join, "m11", &second).unwrap();
        assert!(verify_master(&pre, &seed_of(PAPER)).is_ok());
    }

    #[test]
    fn join_refuses_when_no_master_is_committed() {
        let dir = scratch("join-unarmed");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        let err = preflight(Verb::Join, "m3", &paths).unwrap_err();
        assert!(err.contains("no paper master is committed"), "{err}");
        assert!(err.contains("setup"), "{err}");
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
        assert!(!src.contains(report.machine_pubkey.as_str()));
    }

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

        std::fs::remove_file(&paths.roster).unwrap();
        std::fs::write(concat(&[&paths.roster, ".sig"]), b"sig").unwrap();
        let master = seed_of(PAPER).pubkey_b64().unwrap();
        let expect = RosterExpectation::MayCreateFresh;
        assert!(load_roster(&paths.roster, &master, NOW, expect).is_err());
    }

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
            landed.is_err_and(|e| e.landed),
            "a foreign edit after the write is reported, and this run's bytes landed"
        );
        std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();

        write_pins(&planned).expect("the ordinary write verifies");

        std::fs::write(&paths.pins, PINS_FIXTURE).unwrap();
        let mut no_write = {
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
        no_write.pins_text = None;
        let err = write_pins(&no_write).unwrap_err();
        assert!(
            err.message.contains("holds 0 keys but 1 were intended"),
            "{err}"
        );
        assert!(!err.landed, "a run with nothing to write wrote nothing");
    }

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

    #[test]
    fn no_file_written_by_a_run_contains_the_phrase_or_its_seed() {
        let dir = scratch("no-phrase-on-disk");
        let paths = paths_in(&dir);
        write_fixture(&paths);
        run_setup(&paths, "m3").expect("setup");

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
