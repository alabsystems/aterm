// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RELEASE'S BUNDLE, BUILT ONCE (2026-10-09).
//!
//! A release builds its unsigned universal `aterm.app` ONCE — both architecture
//! slices at once, from the snapshot of the reserved release commit's git objects
//! (`buildplan::run`, [`crate::snapshot`]) — and keeps it here, in the clone's
//! release cache: `<clone>-release.noindex/artifacts/<key>/`. `verify release --app
//! <app>` judges THESE bytes (MEASURE among its checks; the release records the pass
//! with the executable's SHA-256), and the cut signs, notarizes and packages a byte copy
//! of them ([`copy_bundle`]): what was verified is what ships. The cut never builds.
//!
//! KEYED BY WHAT THE BUNDLE WAS BUILT AS ([`key`]): the version, the build number,
//! the release commit (which fixes every source byte and, through its lockfile's
//! checksums, every vendored one) and both compilers' commits. A key the store holds
//! COMPLETE is never built again: a resumed release re-proves its bytes and reuses
//! them; a key it does not hold — another build number, another prover — is a build.
//!
//! COMPLETE IS WRITTEN LAST ([`COMPLETE_FILE`]): a directory without it is a build
//! that did not finish, removed and built again; one with it is re-proved on every
//! read — its executable's digest and the whole bundle's ([`bundle_digest`]) against
//! the record — and bytes that changed after the build are refused, never reused and
//! never shipped.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::snapshot::CacheRoot;

/// The record's schema, and what the key and the bundle digest are taken under.
pub const SCHEMA: &str = "aterm-release-artifact-v1";

/// The store entry's record, written last.
pub const COMPLETE_FILE: &str = "COMPLETE";

/// What a bundle is built AS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `X.Y.0`.
    pub version: String,
    /// The reserved build number `n` (HEAD's between releases: provisional).
    pub build: u64,
    /// The full commit the bytes come from: the reserved release commit.
    pub commit: String,
}

/// The two compilers a build runs: the native slice's Trust `trustc` and the x86_64
/// slice's upstream stable `rustc`, each by its `-vV` commit-hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compilers {
    pub trustc: String,
    pub rustc: String,
}

/// One slice's build, as the record keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SliceRecord {
    pub triple: String,
    pub secs: u64,
    #[serde(default)]
    pub peak_bytes: Option<u64>,
}

/// A COMPLETE build: what it was built as, from, with, and the digests of what it
/// made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Complete {
    pub schema: String,
    pub key: String,
    pub version: String,
    pub build: u64,
    pub commit: String,
    pub trustc: String,
    pub rustc: String,
    /// The snapshot digest the bytes were built from.
    pub snapshot: String,
    /// `lipo -archs` of the executable.
    pub archs: String,
    /// The executable's own `--diagnose` `compiler:` line.
    pub compiler_line: String,
    /// SHA-256 of `aterm.app/Contents/MacOS/aterm`: what the verified record names, and
    /// what the cut's copy must still be before it signs.
    pub macho_sha256: String,
    /// [`bundle_digest`] of the whole bundle.
    pub bundle_digest: String,
    #[serde(default)]
    pub slices: Vec<SliceRecord>,
    /// The published index run the bytes compiled in (`buildplan::index_hint_now`), when
    /// the build found one: a guess its fresh installs' first discovery round follows. Not
    /// in the [`key`]: a resumed release reuses the bytes whatever the channel did since.
    #[serde(default)]
    pub index_hint: Option<String>,
}

impl Complete {
    /// Its directory in `store`.
    #[must_use]
    pub fn dir(&self, store: &Path) -> PathBuf {
        store.join(&self.key)
    }

    /// Its bundle in `store`.
    #[must_use]
    pub fn app(&self, store: &Path) -> PathBuf {
        app_in(&self.dir(store))
    }

    /// Whether it is the bundle of `identity`.
    #[must_use]
    pub fn is(&self, identity: &Identity) -> bool {
        self.version == identity.version
            && self.build == identity.build
            && self.commit == identity.commit
    }
}

/// The bundle in a store entry `dir`.
#[must_use]
pub fn app_in(dir: &Path) -> PathBuf {
    dir.join("aterm.app")
}

/// The store: `<clone>-release.noindex/artifacts`.
///
/// # Errors
/// The clone's cache cannot be made or named.
pub fn store(repo: &Path) -> Result<PathBuf, String> {
    Ok(CacheRoot::for_repo(repo)?.artifacts())
}

/// The key a bundle of `identity`, built with `compilers`, is kept under: SHA-256 over
/// [`SCHEMA`] and each input, NUL-separated, lowercase hex.
#[must_use]
pub fn key(identity: &Identity, compilers: &Compilers) -> String {
    let mut h = aterm_digest::Sha256::new();
    for part in [
        SCHEMA,
        identity.version.as_str(),
        &identity.build.to_string(),
        identity.commit.as_str(),
        compilers.trustc.as_str(),
        compilers.rustc.as_str(),
        "universal",
    ] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    aterm_codec::hex::encode_lower(&h.finalize())
}

/// The digest of a bundle: SHA-256 over [`SCHEMA`] and, for every entry under `app`
/// sorted by relative path, `path NUL kind NUL mode NUL content-sha256-or-link-target
/// NUL` — relative paths only, so a byte copy elsewhere digests the same, and every
/// file, link and mode of the bundle is in it.
///
/// # Errors
/// An entry that cannot be read.
pub fn bundle_digest(app: &Path) -> Result<String, String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    walk(app, app, &mut entries)?;
    entries.sort();
    let mut h = aterm_digest::Sha256::new();
    h.update(SCHEMA.as_bytes());
    h.update([0u8]);
    for (path, line) in entries {
        h.update(path.as_bytes());
        h.update([0u8]);
        h.update(line.as_bytes());
        h.update([0u8]);
    }
    Ok(aterm_codec::hex::encode_lower(&h.finalize()))
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("inspect {}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is not under {}", path.display(), root.display()))?
            .to_string_lossy()
            .into_owned();
        let mode = mode_of(&meta);
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&path)
                .map_err(|e| format!("read link {}: {e}", path.display()))?;
            out.push((rel, format!("link\0{mode:o}\0{}", target.display())));
        } else if meta.is_dir() {
            out.push((rel, format!("dir\0{mode:o}\0")));
            walk(root, &path, out)?;
        } else {
            out.push((rel, format!("file\0{mode:o}\0{}", file_sha256(&path)?)));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn mode_of(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(_meta: &std::fs::Metadata) -> u32 {
    0
}

/// SHA-256 of a file, lowercase hex — the one hasher every digest here uses
/// (`dmg::sha256_file`).
///
/// # Errors
/// The file cannot be read.
pub fn file_sha256(path: &Path) -> Result<String, String> {
    crate::dmg::sha256_file(path)
}

/// The SHA-256 of `app`'s executable.
///
/// # Errors
/// It cannot be read.
pub fn macho_sha256(app: &Path) -> Result<String, String> {
    file_sha256(&app.join("Contents/MacOS/aterm"))
}

/// The COMPLETE entry `key` holds in `store`, its bytes re-proved: `Ok(None)` when
/// there is none (no directory, or a build that did not finish).
///
/// # Errors
/// A record that does not parse or names another key, and bytes that are not the
/// record's — changed after the build, so never reused and never shipped.
pub fn read(store: &Path, key: &str) -> Result<Option<Complete>, String> {
    let dir = store.join(key);
    let record = dir.join(COMPLETE_FILE);
    let text = match std::fs::read_to_string(&record) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", record.display())),
    };
    let complete: Complete = aterm_toml::from_str(&text)
        .map_err(|e| format!("{} is not a build record: {e}", record.display()))?;
    if complete.schema != SCHEMA || complete.key != key {
        return Err(format!(
            "{} is the record of {} ({}), not of {key}",
            record.display(),
            complete.key,
            complete.schema
        ));
    }
    prove(&complete, &app_in(&dir))?;
    Ok(Some(complete))
}

/// `app`'s bytes are `complete`'s: the executable's digest and the whole bundle's.
///
/// # Errors
/// Either differs, named, with what it is now.
pub fn prove(complete: &Complete, app: &Path) -> Result<(), String> {
    let macho = macho_sha256(app)?;
    if macho != complete.macho_sha256 {
        return Err(format!(
            "{}'s executable is {macho}, not the {} built (and verified) as build {} of \
             v{} — the bytes changed after the build; they are never shipped",
            app.display(),
            complete.macho_sha256,
            complete.build,
            complete.version
        ));
    }
    let bundle = bundle_digest(app)?;
    if bundle != complete.bundle_digest {
        return Err(format!(
            "{} digests {bundle}, not the {} built as build {} of v{} — a file of the \
             bundle changed after the build; it is never shipped",
            app.display(),
            complete.bundle_digest,
            complete.build,
            complete.version
        ));
    }
    Ok(())
}

/// This Mac's physical memory in bytes (`sysctl -n hw.memsize`); `None` when it will not
/// say.
pub(crate) fn host_memory_bytes() -> Option<u64> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&bytes: &u64| bytes > 0)
}

/// Whether the two slices may build at once: the peaks the newest COMPLETE build in
/// `store` recorded for its slices, together, fit in three quarters of `memory`
/// (physical bytes). No record, or no memory figure, builds them at once — the
/// measured publishers (128 GiB) always hold both.
#[must_use]
pub fn slices_fit(store: &Path, memory: Option<u64>) -> bool {
    let Some(memory) = memory else {
        return true;
    };
    let newest = std::fs::read_dir(store)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let record = e.path().join(COMPLETE_FILE);
            let when = std::fs::metadata(&record).and_then(|m| m.modified()).ok()?;
            let text = std::fs::read_to_string(&record).ok()?;
            let complete: Complete = aterm_toml::from_str(&text).ok()?;
            Some((when, complete))
        })
        .max_by_key(|(when, _)| *when);
    let Some((_, complete)) = newest else {
        return true;
    };
    let peaks: u64 = complete.slices.iter().filter_map(|s| s.peak_bytes).sum();
    peaks <= memory / 4 * 3
}

/// BUILD THE BUNDLE OF `identity` — the ONE build a release makes — unless the store
/// already holds it COMPLETE, in which case its re-proved record is returned and
/// nothing is built. Before building: the universal gate (this builder can build the
/// x86_64 slice and run it under Rosetta) and the disk floor. Then both slices at once (or one after
/// the other where the last build's peaks would not fit, [`slices_fit`]) into a fresh
/// entry, the bundle laid out from the same snapshot, and [`COMPLETE_FILE`] written
/// last, durably. Every slice must embed the updater pin the committed paper master
/// fixes (`publish::expected_embedded_update_pin`).
///
/// An entry whose bytes changed after its build is not reused: before a claim nothing
/// rests on it, so it is removed and built again (and verified again); after a claim
/// the cut never calls this — it refuses such bytes ([`read`]).
///
/// # Errors
/// The gate's refusal, the build's, and any write of the entry.
pub fn build(repo: &Path, identity: &Identity) -> Result<Complete, String> {
    let store = store(repo)?;
    crate::snapshot::private_dir(&store, "release artifact store")?;
    let compilers = compilers()?;
    let key = key(identity, &compilers);
    match read(&store, &key) {
        Ok(Some(complete)) => {
            println!(
                "==> build {} of v{} from {}: built already ({}), its bytes re-proved — \
                 nothing is built again",
                identity.build,
                identity.version,
                short(&identity.commit),
                complete.app(&store).display()
            );
            return Ok(complete);
        }
        Ok(None) => {}
        Err(why) => println!("==> {why}; the entry is built again"),
    }
    let expected_pin =
        crate::publish::expected_embedded_update_pin(aterm_update_core::pins::PAPER_MASTER_PUBKEYS)
            .map_err(|e| e.to_string())?;
    crate::gates::universal_gate(crate::gates::x86_target_probe, || {
        crate::gates::rosetta_runs(&mut |command| command.output())
    })
    .map_err(|e| e.to_string())?;
    crate::gates::disk_gate(&store).map_err(|e| e.to_string())?;
    let dir = store.join(&key);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => println!(
            "==> {} held no usable build; removed, built again",
            dir.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("remove the unfinished {}: {e}", dir.display())),
    }
    crate::snapshot::private_dir(&dir, "release artifact")?;
    let memory = host_memory_bytes();
    let index_hint = crate::buildplan::index_hint_now();
    let built = crate::buildplan::run(&crate::buildplan::BuildPlan {
        repo_root: repo.to_path_buf(),
        commit: identity.commit.clone(),
        out_dir: dir.clone(),
        build_number: identity.build,
        short_version: identity.version.clone(),
        expected_update_pin_sha256: expected_pin,
        one_slice_at_a_time: !slices_fit(&store, memory),
        index_hint: index_hint.clone(),
    })?;
    if built.app != app_in(&dir) {
        return Err(format!(
            "the build left its bundle at {}, not in its store entry {}",
            built.app.display(),
            dir.display()
        ));
    }
    let complete = Complete {
        schema: SCHEMA.to_string(),
        key: key.clone(),
        version: identity.version.clone(),
        build: identity.build,
        commit: identity.commit.clone(),
        trustc: compilers.trustc,
        rustc: compilers.rustc,
        snapshot: built.snapshot_digest,
        archs: built.archs,
        compiler_line: built.compiler_line,
        macho_sha256: macho_sha256(&built.app)?,
        bundle_digest: bundle_digest(&built.app)?,
        slices: built
            .slices
            .iter()
            .map(|s| SliceRecord {
                triple: s.triple.to_string(),
                secs: s.secs,
                peak_bytes: s.peak_bytes,
            })
            .collect(),
        index_hint,
    };
    write_complete(&dir, &complete)?;
    let needed = needed_keys(repo);
    let mut keep: Vec<&str> = needed.iter().map(String::as_str).collect();
    keep.push(&key);
    prune(&store, &keep);
    println!(
        "==> built ONCE: {} — executable sha256 {}, bundle {}",
        built.app.display(),
        complete.macho_sha256,
        complete.bundle_digest
    );
    Ok(complete)
}

/// The compilers this machine's build runs now.
///
/// # Errors
/// Either compiler cannot be asked for its commit.
pub fn compilers() -> Result<Compilers, String> {
    let stage2 = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
    let out = std::process::Command::new(stage2.join("trustc"))
        .arg("-vV")
        .output()
        .map_err(|e| format!("ask {} -vV: {e}", stage2.join("trustc").display()))?;
    let trustc = crate::buildplan::commit_hash_of(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| {
            format!(
                "{} -vV names no commit-hash — a build no other build can be told from",
                stage2.join("trustc").display()
            )
        })?;
    Ok(Compilers {
        trustc,
        rustc: crate::buildplan::stable_rustc_commit()?,
    })
}

/// Write `complete` as `dir`'s record: a temporary, flushed, renamed, the directory
/// flushed — so a record is whole or absent.
fn write_complete(dir: &Path, complete: &Complete) -> Result<(), String> {
    let text = aterm_toml::to_string(complete).map_err(|e| format!("serialize the record: {e}"))?;
    let tmp = dir.join(format!(".{COMPLETE_FILE}.tmp"));
    let path = dir.join(COMPLETE_FILE);
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        crate::durable::sync_all(&file)?;
        std::fs::rename(&tmp, &path)?;
        crate::durable::sync_dir(dir)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("write {}: {e}", path.display())
    })
}

/// A BYTE COPY of the store's bundle at `to` (absent), re-proved against the record
/// before anything signs it: the copy's executable and whole-bundle digests are the
/// built ones — so the bytes the cut signs are the bytes `verify release` judged. Files keep
/// their bytes and modes, links their targets; nothing else is carried.
///
/// # Errors
/// A copy that fails, or bytes that differ from the record.
pub fn copy_bundle(complete: &Complete, from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        return Err(format!(
            "{} exists — the verified bundle is copied into a fresh place",
            to.display()
        ));
    }
    copy_tree(from, to)?;
    prove(complete, to)
}

/// The build's records beside the bundle, copied from the store entry `dir` into
/// `dist` for the cut that signs it: `aterm.dSYM` (which the provenance record looks
/// for), the owner's never-published symbolication archive `aterm-<v>-dSYM.zip`, and
/// the x86_64 slice's execution record `aterm-<v>-x86_64-probe.txt`. Each replaces
/// what `dist` held under its name; a record the build did not make (dsymutil can
/// fail, and the dSYM is then skipped, never faked) is removed from `dist`, so a
/// stale one from an earlier cut never rides along.
///
/// # Errors
/// A copy or a removal that fails.
pub fn place_records(dir: &Path, dist: &Path, version: &str) -> Result<(), String> {
    for name in [
        "aterm.dSYM".to_string(),
        format!("aterm-{version}-dSYM.zip"),
        crate::buildplan::x86_64_probe_asset_name(version),
    ] {
        let to = dist.join(&name);
        match std::fs::symlink_metadata(&to) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&to),
            Ok(_) => std::fs::remove_file(&to),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
        .map_err(|e| format!("replace {}: {e}", to.display()))?;
        let from = dir.join(&name);
        if from.exists() {
            copy_tree(&from, &to)?;
        }
    }
    Ok(())
}

/// Remove every entry of `store` but those named in `keep`: a release keeps the one
/// bundle it is building or signing, and nothing older. An entry that cannot be
/// removed is said and left for the next build.
fn prune(store: &Path, keep: &[&str]) {
    let Ok(entries) = std::fs::read_dir(store) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if keep.iter().any(|k| name.as_os_str() == *k) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_dir_all(&path) {
            Ok(()) => println!(
                "==> removed the release cache's older bundle {}",
                path.display()
            ),
            Err(e) => println!("==> kept {} ({e}); the next build retries", path.display()),
        }
    }
}

/// The store keys this clone's journals still need: the release journal's bundle
/// (a release between its build and its cut) and an unfinished cut's (signed from
/// the store until its `sign` step is journaled).
fn needed_keys(repo: &Path) -> Vec<String> {
    let mut keys = Vec::new();
    let git = crate::ledger::GitCli::new(repo);
    if let Ok(dir) = crate::release::release_dir(&git)
        && let Ok(Some(journal)) =
            crate::release::Journal::load(&dir.join(crate::release::JOURNAL_FILE))
        && let Some(artifact) = journal.artifact
    {
        keys.push(artifact.key);
    }
    let main = crate::snapshot::main_checkout(repo).unwrap_or_else(|_| repo.to_path_buf());
    if let Ok(Some(cut)) = crate::publish::Journal::load_if_ours(&main.join("dist/cut-state.toml"))
        && !cut.is_done("sign")
        && let Some(key) = cut.artifact
    {
        keys.push(key);
    }
    keys
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let meta =
        std::fs::symlink_metadata(from).map_err(|e| format!("inspect {}: {e}", from.display()))?;
    if meta.file_type().is_symlink() {
        let target =
            std::fs::read_link(from).map_err(|e| format!("read link {}: {e}", from.display()))?;
        return symlink(&target, to);
    }
    if meta.is_dir() {
        std::fs::create_dir(to).map_err(|e| format!("create {}: {e}", to.display()))?;
        let mut names: Vec<_> = std::fs::read_dir(from)
            .map_err(|e| format!("read {}: {e}", from.display()))?
            .flatten()
            .map(|e| e.file_name())
            .collect();
        names.sort();
        for name in names {
            copy_tree(&from.join(&name), &to.join(&name))?;
        }
        return set_mode(to, &meta);
    }
    let bytes = std::fs::read(from).map_err(|e| format!("read {}: {e}", from.display()))?;
    std::fs::write(to, bytes).map_err(|e| format!("write {}: {e}", to.display()))?;
    set_mode(to, &meta)
}

#[cfg(unix)]
fn symlink(target: &Path, at: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, at)
        .map_err(|e| format!("link {} -> {}: {e}", at.display(), target.display()))
}

#[cfg(not(unix))]
fn symlink(_target: &Path, at: &Path) -> Result<(), String> {
    Err(format!("{}: a bundle's links need Unix", at.display()))
}

#[cfg(unix)]
fn set_mode(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(meta.permissions().mode() & 0o7777),
    )
    .map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _meta: &std::fs::Metadata) -> Result<(), String> {
    Ok(())
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// `targo --unverified ship build` — THE ONE BUILD OF A RELEASE BUNDLE, into the
/// release cache, or the COMPLETE one found there, its bytes re-proved:
///
/// * The `ship release` build step, which runs it as its child: the candidate this
///   clone's unfinished release journal reserved (`release.toml` [reserved]) — its
///   build number and release commit;
/// * with no release in progress (no journal, or a finished one): HEAD's bundle under
///   a provisional number — `max(HEAD's ledger tail + 1, now)`, claimed nowhere — the
///   bundle `verify measure --app <app>` judges between releases.
///
/// # Errors
/// A release in progress that has not reserved yet, and [`build`]'s.
pub fn run_build(repo: &Path) -> Result<(), crate::ledger::Error> {
    crate::gates::arm_toolchain_lease();
    let git = crate::ledger::GitCli::new(repo);
    let dir = crate::release::release_dir(&git)?;
    let journal = crate::release::Journal::load(&dir.join(crate::release::JOURNAL_FILE))?
        .filter(|j| !j.finished());
    let (identity, provisional) = match journal {
        Some(journal) => {
            let reserved = journal.reserved.ok_or_else(|| {
                crate::ledger::Error::new(format!(
                    "this clone's release of v{} from {} reserves no identity yet — `{}` \
                     reserves it during the build step (`{} --forget` drops that \
                     release, and `ship build` then builds HEAD under a provisional number)",
                    journal.version,
                    short(&journal.commit),
                    crate::release::RELEASE_COMMAND,
                    crate::release::RELEASE_COMMAND
                ))
            })?;
            let identity = Identity {
                version: journal.version,
                build: reserved.build,
                commit: reserved.commit,
            };
            (identity, false)
        }
        None => (provisional_identity(&git)?, true),
    };
    let complete = build(repo, &identity)?;
    let app = complete.app(&store(repo)?);
    println!(
        "BUILT aterm {} ({}build {}) from {}: {} — executable sha256 {}",
        complete.version,
        if provisional { "provisional " } else { "" },
        complete.build,
        short(&complete.commit),
        app.display(),
        complete.macho_sha256
    );
    if provisional {
        println!(
            "    measure it: targo --unverified run -q -p aterm-verify -- measure --app {}",
            crate::release::shell_word(&app.display().to_string())
        );
    }
    Ok(())
}

/// HEAD's bundle under a provisional number, read from HEAD's objects: its workspace
/// version (`X.Y.0`), and `max(tail + 1, now)` over its own ledger — claimed nowhere.
///
/// # Errors
/// git, a version that is not `X.Y.0`, or a ledger that does not parse.
pub fn provisional_identity(
    git: &dyn crate::ledger::GitRunner,
) -> Result<Identity, crate::ledger::Error> {
    let commit = crate::ledger::rev_parse(git, "HEAD")?;
    let show = |path: &str| -> Result<String, crate::ledger::Error> {
        let out = crate::ledger::git_ok(git, &["show", &format!("{commit}:{path}")])?;
        String::from_utf8(out.stdout)
            .map_err(|_| crate::ledger::Error::new(format!("{commit}:{path} is not UTF-8")))
    };
    let version = crate::publish::release_version_from_workspace(
        &crate::publish::workspace_version(&show("Cargo.toml")?)?,
    )?;
    let tail = crate::ledger::tail(&show(crate::ledger::LEDGER_FILE)?)?;
    let build = crate::ledger::next_build(tail.build, crate::publish::unix_now())?;
    Ok(Identity {
        version,
        build,
        commit,
    })
}
