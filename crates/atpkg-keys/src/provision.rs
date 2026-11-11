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
    let home = std::env::var("HOME").ok();
    home_path_from(home.as_deref(), rel)
}

/// Resolve `<home>/<rel>`, refusing unset, empty, or `/` `HOME` values and trimming trailing `/`.
pub fn home_path_from(home: Option<&str>, rel: &str) -> Result<String, String> {
    let base = home
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "HOME is not set".to_string())?;
    Ok(concat(&[base, "/", rel]))
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
        if std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.is_file()) {
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
    let body_absent = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Ok(_) => false,
        Err(e) => return Err(concat(&["inspect ", path, ": ", &e.to_string()])),
    };
    if body_absent {
        let sig_absent = match std::fs::symlink_metadata(&sig_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Ok(_) => false,
            Err(e) => return Err(concat(&["inspect ", &sig_path, ": ", &e.to_string()])),
        };
        if !sig_absent {
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

/// Where the redo transaction of the roster pair whose body is `path` is kept.
#[must_use]
pub fn roster_txn_path(path: &str) -> String {
    concat(&[path, ".atpkg-keys.txn"])
}

/// The writer lock of the roster pair whose body is `path`. [`lock_roster`] creates it before
/// any protocol writer writes either half or a transaction, and no roster code removes it:
/// once present, the pair is read under it.
#[must_use]
pub fn roster_lock_path(path: &str) -> String {
    concat(&[path, ".lock"])
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
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
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
    .and_then(|()| {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&staged)?;
        aterm_durable::ordered(&dir)
    });
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
    let lock_path = roster_lock_path(path);
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
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let lock_path = roster_lock_path(path);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
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
    if !opened.is_file() {
        return Err(concat(&[
            "open existing roster lock ",
            &lock_path,
            " without creating it: not a regular file",
        ]));
    }
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
    if !current.is_file() || opened.dev() != current.dev() || opened.ino() != current.ino() {
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
            if std::fs::symlink_metadata(&paths.roster).is_ok() {
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
            if !std::fs::symlink_metadata(&paths.roster).is_ok_and(|m| m.is_file()) {
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

    if std::fs::symlink_metadata(&paths.key).is_ok() {
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

impl Planned {
    /// The exact signed public pair this plan will publish on a successful write.
    /// This contains no machine key or master phrase and grants no signing authority.
    #[must_use]
    pub fn public_roster(&self) -> RosterSnapshot {
        RosterSnapshot {
            raw: self.roster_bytes.clone(),
            sig: self.roster_sig.clone(),
        }
    }
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

impl Drop for Planned {
    fn drop(&mut self) {
        crate::master::scrub(&mut self.machine_pkcs8);
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
pub fn write_rest(mut planned: Planned) -> Result<Report, String> {
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
        id: std::mem::take(&mut planned.id),
        paths: Paths {
            pins: std::mem::take(&mut planned.paths.pins),
            roster: std::mem::take(&mut planned.paths.roster),
            key: std::mem::take(&mut planned.paths.key),
            machine_pub: std::mem::take(&mut planned.paths.machine_pub),
            pins_explicit: planned.paths.pins_explicit,
        },
        master_pubkey: std::mem::take(&mut planned.master_pubkey),
        master_fingerprint: std::mem::take(&mut planned.master_fingerprint),
        master_after: std::mem::take(&mut planned.master_after),
        machine_pubkey: std::mem::take(&mut planned.machine_pubkey),
        roster_seq: planned.roster_seq,
        roster_valid_until: std::mem::take(&mut planned.roster_valid_until),
        roster_machines: std::mem::take(&mut planned.roster_machines),
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
#[path = "provision_tests.rs"]
mod tests;
