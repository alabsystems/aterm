// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Store checks ([`check_install_path`], [`preflight_store`], [`read_install_policy`]).

use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt as _;

use aterm_toml::Value;

use crate::chain::{open_file, parse_toml, read_bounded, strings};
use crate::py::{PyPath, bare_os_error, equals_int, equals_str, os_error, truthy, uint, uint_or};
use crate::{MAX_BINARY, Refusal, Verdict, require};

/// The durable policy `.aterm-update/state.toml` records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub floors: [u64; 3],
    pub revoked_machines: Vec<String>,
    pub rejected_build: u64,
    /// `[min_build_sources]` as recorded, read by [`crate::withdrawn_floor`].
    pub min_build_sources: Option<Value>,
}

/// Whether another account can write the directory `path`, which `meta` describes.
fn shared_write(path: &std::path::Path, meta: &std::fs::Metadata, me: u32) -> bool {
    let mode = meta.mode();
    mode & 0o022 != 0
        && !(mode & 0o002 == 0
            && meta.uid() == me
            && !crate::host::acl_carried(path)
            && !crate::host::remote_filesystem(path)
            && crate::accounts::private_group(me, meta.gid()))
}

/// Refuse a store another account could replace.
/// # Errors
/// The first component that is not a directory of root's or this user's, or writable unshielded by
/// another account.
pub fn check_install_path(store: &OsStr) -> Verdict<()> {
    let store = PyPath::new(store).absolute()?;
    let me = crate::host::uid();
    let mut private_boundary = false;
    for ancestor in store.lineage() {
        let path = ancestor.to_path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| os_error(&e, &path))?;
        require(
            meta.file_type().is_dir() && (meta.uid() == 0 || meta.uid() == me),
            &format!("unsafe install path component: {ancestor}"),
        )?;
        if !private_boundary && shared_write(&path, &meta, me) {
            let remedy = if meta.uid() == me {
                format!(" (another account can write it; run: chmod go-w {ancestor})")
            } else {
                " (another account can write it; set ATERM_STORE_DIR to a directory of yours)"
                    .to_string()
            };
            return Err(Refusal::new(format!(
                "writable unshielded install path component: {ancestor}{remedy}"
            )));
        }
        private_boundary |= meta.uid() == me && meta.mode() & 0o077 == 0;
    }
    Ok(())
}

/// The installer's pre-flight, before any download.
/// # Errors
/// [`check_install_path`]'s.
pub fn preflight_store(store: &OsStr) -> Verdict<()> {
    let store = PyPath::new(store).absolute()?;
    let lineage = store.lineage();
    let existing = lineage
        .iter()
        .rev()
        .find(|path| std::fs::symlink_metadata(path.to_path()).is_ok())
        .unwrap_or(&lineage[0]);
    check_install_path(existing.to_path().as_os_str())
}

/// No record: nothing installed here yet.
fn clean_slate() -> Policy {
    Policy::default()
}

/// `re.fullmatch("[0-9a-f]{64}", text)`.
fn digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A trial identity's `sha256`, when it is a table holding a lowercase digest.
fn identity_digest(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_table)
        .and_then(|identity| identity.get("sha256"))
        .and_then(Value::as_str)
        .filter(|sha| digest(sha))
}

/// The lowercase SHA-256 of the open `file`.
fn sha256_of(mut file: std::fs::File) -> Verdict<String> {
    use std::io::Read as _;
    let mut hasher = aterm_digest::Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buffer).map_err(|e| bare_os_error(&e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(aterm_codec::hex::encode_lower(&hasher.finalize()))
}

/// The durable policy beside `store` ([`Policy`]), read only while it binds the installed
/// file: the record must be the updater's own private file of the schema it writes, its
/// target this store's `aterm`, every number in it a u64, and the target either absent while
/// updates are off and no trial is recorded, or a file no other account can write whose digest
/// is the identity recorded or one a trial's phase and identities explain. A store with no
/// record has a clean slate.
/// # Errors
/// [`check_install_path`]'s, then the first rule the record breaks.
pub fn read_install_policy(store: &OsStr) -> Verdict<Policy> {
    check_install_path(store)?;
    let store = PyPath::new(store).absolute()?;
    let path = store.join(".aterm-update").join("state.toml");
    let me = crate::host::uid();
    let parent_path = path.parent().to_path();
    let parent = match std::fs::symlink_metadata(&parent_path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(clean_slate()),
        Err(e) => return Err(os_error(&e, &parent_path)),
    };
    require(
        parent.file_type().is_dir() && parent.uid() == me && parent.mode() & 0o077 == 0,
        "unsafe update-state directory",
    )?;
    let real = path.to_path();
    let meta = match std::fs::symlink_metadata(&real) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(clean_slate()),
        Err(e) => return Err(os_error(&e, &real)),
    };
    require(
        meta.file_type().is_file() && meta.uid() == me && meta.mode() & 0o077 == 0,
        "unsafe update-state file",
    )?;
    let state = parse_toml(&read_bounded(&path, 262_144)?, &path)?;
    require(
        equals_int(state.get("schema"), 1),
        "unsupported update-state schema",
    )?;
    let target = store.join("aterm");
    require(
        state
            .get("target")
            .and_then(Value::as_str)
            .is_some_and(|recorded| target.is(recorded)),
        "update-state target mismatch",
    )?;
    let installed = state
        .get("installed")
        .and_then(Value::as_table)
        .filter(|installed| installed.get("sha256").and_then(Value::as_str).is_some())
        .ok_or_else(|| Refusal::new("invalid installed identity"))?;
    let installed_sha = installed
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let installed_build = uint(installed.get("build"), "installed build", u64::MAX)?;
    let revoked = strings(
        state.get("revoked_machines"),
        "invalid durable revoked machines",
    )?;
    let mut floors = [0; 3];
    for (floor, key) in floors
        .iter_mut()
        .zip(["high_water", "min_build", "roster_floor"])
    {
        *floor = uint(state.get(key), key, u64::MAX)?;
    }
    let mut policy = Policy {
        floors,
        revoked_machines: revoked,
        rejected_build: uint_or(state.get("rejected_build"), 0, "rejected build", u64::MAX)?,
        min_build_sources: state.get("min_build_sources").cloned(),
    };
    let target_path = target.to_path();
    let target_meta = match std::fs::symlink_metadata(&target_path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            require(
                matches!(state.get("enabled"), Some(Value::Boolean(false)))
                    && state.get("trial").is_none(),
                "enrolled executable is absent or an update trial needs recovery",
            )?;
            require(
                digest(installed_sha) || (installed_sha.is_empty() && installed_build == 0),
                "invalid fresh installed identity",
            )?;
            return Ok(policy);
        }
        Err(e) => return Err(os_error(&e, &target_path)),
    };
    require(digest(installed_sha), "invalid installed identity")?;
    require(
        target_meta.file_type().is_file()
            && target_meta.uid() == me
            && target_meta.mode() & 0o6022 == 0
            && target_meta.len() <= MAX_BINARY,
        "unsafe installed executable",
    )?;
    let actual = sha256_of(open_file(&target_path)?)?;
    let mut rejected = policy.rejected_build;
    let trial = state.get("trial");
    if truthy(trial) {
        let trial = trial
            .and_then(Value::as_table)
            .ok_or_else(|| Refusal::new("invalid durable update trial"))?;
        let (old, new) = (trial.get("old"), trial.get("new"));
        let (Some(old_sha), Some(new_sha)) = (identity_digest(old), identity_digest(new)) else {
            return Err(Refusal::new("invalid durable trial identity"));
        };
        let phase = trial.get("phase");
        let prepared_old =
            equals_str(phase, "Prepared") && actual == old_sha && old_sha == installed_sha;
        let healthy_new = equals_str(phase, "Installed")
            && matches!(trial.get("healthy"), Some(Value::Boolean(true)))
            && actual == new_sha
            && new_sha == installed_sha;
        let rolling_back =
            equals_str(phase, "RollbackPrepared") && (actual == old_sha || actual == new_sha);
        require(
            prepared_old || healthy_new || rolling_back,
            "an unresolved update trial requires a healthy launch or runtime recovery",
        )?;
        if rolling_back {
            let new_build = new
                .and_then(Value::as_table)
                .and_then(|identity| identity.get("build"));
            rejected = rejected.max(uint(new_build, "rejected trial build", u64::MAX)?);
        }
    } else {
        require(
            actual == installed_sha,
            "installed executable disagrees with durable update identity",
        )?;
    }
    policy.rejected_build = rejected;
    Ok(policy)
}
