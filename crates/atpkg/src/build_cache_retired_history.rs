// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private store's files can be clean while its other refs or reflogs hold unique work.
//! Their raw object roots must be reachable from live roots in an independently complete
//! canonical module store. A borrowed or unrecognized witness supplies no deletion authority.
//! These bounded observations do not exclude concurrent writers or audit incoming borrowers.

use super::Git;
use super::registry::{self, Budget, Directory, Kind};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const FILE_BYTES: usize = 16 << 20;
const METADATA_BYTES: usize = 64 << 20;
const ROOTS: usize = 65_536;
const WITNESS_ROOTS: usize = 1_024;
const ELAPSED: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    bytes: u64,
    modified: SystemTime,
    identity: [u64; 2],
    change: [i64; 2],
    directory: bool,
    regular: bool,
}

fn stamp(path: &Path, meta: &Metadata) -> io::Result<Stamp> {
    #[cfg(unix)]
    let (identity, change) = {
        use std::os::unix::fs::MetadataExt as _;
        ([meta.dev(), meta.ino()], [meta.ctime(), meta.ctime_nsec()])
    };
    #[cfg(windows)]
    let (identity, change) = {
        use std::os::windows::fs::MetadataExt as _;
        let (volume, file) = crate::platform::directory_identity(path)
            .ok_or_else(|| io::Error::other("private Git identity is unavailable"))?;
        (
            [u64::from(volume), file],
            [
                meta.creation_time() as i64,
                i64::from(meta.file_attributes()),
            ],
        )
    };
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, meta);
        return Err(io::Error::other("private Git identity is unavailable"));
    }
    #[cfg(unix)]
    let _ = path;
    #[cfg(any(unix, windows))]
    Ok(Stamp {
        bytes: meta.len(),
        modified: meta.modified()?,
        identity,
        change,
        directory: meta.is_dir(),
        regular: meta.is_file(),
    })
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    stamps: BTreeMap<PathBuf, Stamp>,
    files: BTreeMap<PathBuf, Vec<u8>>,
}

impl Snapshot {
    fn read(directory: &Directory, objects: bool, budget: &mut Budget) -> io::Result<Self> {
        let mut snapshot = Self {
            stamps: BTreeMap::new(),
            files: BTreeMap::new(),
        };
        let mut bytes = 0;
        snapshot.walk(directory, Path::new(""), objects, 0, budget, &mut bytes)?;
        let named = Directory::root(&directory.path)?
            .ok_or_else(|| io::Error::other("private Git store disappeared"))?;
        if stamp(&directory.path, &directory.metadata()?)?
            != stamp(&named.path, &named.metadata()?)?
        {
            return Err(io::Error::other("private Git store binding changed"));
        }
        Ok(snapshot)
    }

    fn walk(
        &mut self,
        directory: &Directory,
        relative: &Path,
        objects: bool,
        depth: usize,
        budget: &mut Budget,
        bytes: &mut usize,
    ) -> io::Result<()> {
        budget.check(depth)?;
        self.stamps.insert(
            relative.to_path_buf(),
            stamp(&directory.path, &directory.metadata()?)?,
        );
        for (name, kind) in directory.names(budget)? {
            let at = relative.join(&name);
            let path = directory.path.join(&name);
            if matches!(kind, Kind::Link | Kind::Other) {
                return Err(io::Error::other(
                    "private Git metadata is linked or special",
                ));
            }
            let meta = fs::symlink_metadata(&path)?;
            if (kind == Kind::Directory && !meta.is_dir())
                || (kind == Kind::File && !meta.is_file())
                || meta.file_type().is_symlink()
            {
                return Err(io::Error::other(
                    "private Git metadata changed type during observation",
                ));
            }
            self.stamps.insert(at.clone(), stamp(&path, &meta)?);
            if kind == Kind::Directory {
                let read = if relative.as_os_str().is_empty() {
                    name == "refs" || name == "logs" || name == "objects" || name == "info"
                } else {
                    !relative.starts_with("objects") || objects || at == Path::new("objects/info")
                };
                if read {
                    self.walk(
                        &directory.child(&name)?,
                        &at,
                        objects,
                        depth + 1,
                        budget,
                        bytes,
                    )?;
                }
            } else if !at.starts_with("objects")
                || at == Path::new("objects/info/alternates")
                || at == Path::new("objects/info/http-alternates")
            {
                let content = directory.read(&name, FILE_BYTES)?;
                *bytes = bytes
                    .checked_add(content.len())
                    .ok_or_else(|| io::Error::other("private Git metadata byte count overflow"))?;
                if *bytes > METADATA_BYTES {
                    return Err(io::Error::other(
                        "private Git metadata reached its byte bound",
                    ));
                }
                self.files.insert(at, content);
            }
        }
        Ok(())
    }

    fn file(&self, name: &str) -> Option<&[u8]> {
        self.files.get(Path::new(name)).map(Vec::as_slice)
    }
}

struct Read<'a> {
    git: &'a dyn Git,
    checkout: &'a Path,
    deadline: Instant,
}

impl Read<'_> {
    fn git(
        &self,
        store: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
    ) -> Result<String, String> {
        completed(self.answer(store, args, input, objects)?)
    }

    fn answer(
        &self,
        store: &Path,
        args: &[&str],
        input: &[u8],
        objects: Option<&Path>,
    ) -> Result<super::Answer, String> {
        if Instant::now() >= self.deadline {
            return Err("private Git history reached its time bound".to_owned());
        }
        let store = super::git_path(store)
            .to_str()
            .ok_or("private Git path is not UTF-8")?;
        let worktree = super::git_path(self.checkout)
            .to_str()
            .ok_or("retained Git worktree path is not UTF-8")?;
        // Metadata survives the snapshot's source removal. Never let core.worktree
        // redirect these reads back into that missing or partly removed source tree.
        let mut all = vec!["--git-dir", store, "--work-tree", worktree];
        all.extend_from_slice(args);
        Ok(self
            .git
            .read_history(self.checkout, &all, input, objects, self.deadline))
    }

    /// Each ref of `store` and its object, one `<name>NUL<object>` row each, as
    /// `for-each-ref --include-root-refs` lists them: HEAD and the other root refs beside
    /// `refs/`. A git without the option (Ubuntu 24.04's 2.43.0, say) refuses it as a usage
    /// error (129). In the files backend, the one ref storage [`Read::format`] admits, each root
    /// ref is a regular top-level file of the store, so for such a git those rows come from
    /// `snapshot`'s files by git's rule ([`root_ref`]) beside its `refs/` listing: an object name
    /// as written, or a symbolic ref's target as listed, a dangling one naming no object, as git
    /// omits it.
    fn refs(
        &self,
        store: &Path,
        snapshot: &Snapshot,
        objects: Option<&Path>,
    ) -> Result<String, String> {
        let rooted = self.answer(
            store,
            &["for-each-ref", "--include-root-refs", REF_ROWS],
            &[],
            objects,
        )?;
        if rooted.code != Some(129) {
            return completed(rooted);
        }
        let mut listed = self.git(store, &["for-each-ref", REF_ROWS], &[], objects)?;
        let named: BTreeMap<String, String> = listed
            .lines()
            .filter_map(|row| row.split_once('\0'))
            .map(|(name, oid)| (name.to_owned(), oid.to_owned()))
            .collect();
        for (path, bytes) in &snapshot.files {
            let Some(name) = path.to_str().filter(|name| root_ref(name)) else {
                continue;
            };
            let value = std::str::from_utf8(bytes)
                .ok()
                .and_then(|text| text.strip_suffix('\n'))
                .ok_or("private Git root ref is malformed")?;
            let oid = match value.strip_prefix("ref: ") {
                Some(target) if target.starts_with("refs/") => match named.get(target) {
                    Some(oid) => oid.as_str(),
                    None => continue,
                },
                Some(_) => return Err("private Git root ref names no ref".to_owned()),
                None => value,
            };
            listed.push_str(&format!("{name}\0{oid}\n"));
        }
        Ok(listed)
    }

    fn format(&self, snapshot: &Snapshot, witness: bool) -> Result<usize, String> {
        if snapshot.stamps.contains_key(Path::new("commondir")) {
            return Err(
                "private Git common store has another common-directory redirect".to_owned(),
            );
        }
        if snapshot.stamps.contains_key(Path::new("config.worktree")) {
            return Err("private Git worktree-specific configuration is unproven".to_owned());
        }
        let config = snapshot
            .file("config")
            .ok_or("private Git configuration is missing")?;
        // An explicit non-repository Git directory keeps the parser from loading the
        // source repository's includes before we have inspected these exact bytes.
        let parser = Path::new(if cfg!(windows) { "NUL" } else { "/dev/null" });
        let listed = self.git(
            parser,
            &["config", "--no-includes", "--file", "-", "--null", "--list"],
            config,
            None,
        )?;
        let mut format = 40;
        for row in listed.split('\0').filter(|row| !row.is_empty()) {
            let (key, value) = row
                .split_once('\n')
                .ok_or("private Git configuration is malformed")?;
            if key.starts_with("include.") || key.starts_with("includeif.") {
                return Err("private Git configuration includes another file".to_owned());
            }
            if witness && key.starts_with("fsck.") {
                return Err("canonical Git witness customizes its integrity validation".to_owned());
            }
            match key {
                "extensions.objectformat" => match value {
                    "sha1" => format = 40,
                    "sha256" => format = 64,
                    _ => return Err("private Git object format is unknown".to_owned()),
                },
                "extensions.refstorage" if value == "files" => {}
                "core.repositoryformatversion" if matches!(value, "0" | "1") => {}
                "core.repositoryformatversion" => {
                    return Err("private Git repository format is unknown".to_owned());
                }
                key if key.starts_with("extensions.") => {
                    return Err("private Git repository extension is unproven".to_owned());
                }
                _ => {}
            }
        }
        if witness {
            for name in [
                "objects/info/alternates",
                "objects/info/http-alternates",
                "shallow",
            ] {
                if snapshot.stamps.contains_key(Path::new(name)) && snapshot.file(name).is_none() {
                    return Err("canonical Git witness has unrecognized object metadata".to_owned());
                }
                if snapshot.file(name).is_some_and(|bytes| !bytes.is_empty()) {
                    return Err(
                        "canonical Git witness has borrowed or shallow object storage".to_owned(),
                    );
                }
            }
        }
        if snapshot.stamps.contains_key(Path::new("info/grafts"))
            && snapshot.file("info/grafts").is_none()
        {
            return Err("private Git graft metadata is unrecognized".to_owned());
        }
        if snapshot
            .file("info/grafts")
            .is_some_and(|bytes| !bytes.is_empty())
        {
            return Err("private Git grafted history is unproven".to_owned());
        }
        Ok(format)
    }

    fn roots(
        &self,
        store: &Path,
        snapshot: &Snapshot,
        width: usize,
        reflogs: bool,
        objects: Option<&Path>,
    ) -> Result<BTreeSet<String>, String> {
        let listed = self.refs(store, snapshot, objects)?;
        let mut roots = BTreeSet::new();
        let mut head = false;
        let mut names = BTreeSet::new();
        for row in listed.lines() {
            let (name, oid) = row
                .split_once('\0')
                .ok_or("private Git reference row is malformed")?;
            if !names.insert(name) || name.starts_with("refs/replace/") || !oid_valid(oid, width) {
                return Err("private Git reference inventory is unproven".to_owned());
            }
            head |= name == "HEAD";
            if !reflogs && name != "HEAD" && !name.starts_with("refs/") {
                continue;
            }
            roots.insert(oid.to_owned());
        }
        if !head {
            return Err("private Git HEAD is missing or unreadable".to_owned());
        }
        if reflogs {
            // Git's reference iterator intentionally omits these multi-object pseudorefs,
            // even with --include-root-refs. They can retain work independently of HEAD.
            for name in ["FETCH_HEAD", "MERGE_HEAD"] {
                let Some(bytes) = snapshot.file(name) else {
                    if snapshot.stamps.contains_key(Path::new(name)) {
                        return Err("private Git pseudoref is not a regular file".to_owned());
                    }
                    continue;
                };
                if (!bytes.is_empty() && !bytes.ends_with(b"\n")) || bytes.contains(&0) {
                    return Err("private Git pseudoref has an incomplete record".to_owned());
                }
                for record in bytes.split_inclusive(|byte| *byte == b'\n') {
                    let row = record
                        .strip_suffix(b"\n")
                        .ok_or("private Git pseudoref record is incomplete")?;
                    if row.is_empty() {
                        return Err("private Git pseudoref has an empty record".to_owned());
                    }
                    let oid = if name == "FETCH_HEAD" {
                        let mut fields = row.splitn(3, |byte| *byte == b'\t');
                        let oid = fields
                            .next()
                            .ok_or("private Git fetch record is malformed")?;
                        let merge = fields
                            .next()
                            .ok_or("private Git fetch record is malformed")?;
                        let _description = fields
                            .next()
                            .ok_or("private Git fetch record is malformed")?;
                        if !merge.is_empty() && merge != b"not-for-merge" {
                            return Err("private Git fetch merge flag is malformed".to_owned());
                        }
                        oid
                    } else {
                        row
                    };
                    let oid = std::str::from_utf8(oid)
                        .map_err(|_| "private Git pseudoref object is malformed")?;
                    if !oid_valid(oid, width) {
                        return Err("private Git pseudoref object is malformed".to_owned());
                    }
                    roots.insert(oid.to_owned());
                    if roots.len() > ROOTS {
                        return Err("private Git roots reached their work bound".to_owned());
                    }
                }
            }
            for (path, bytes) in &snapshot.files {
                if !path.starts_with("logs") {
                    continue;
                }
                if (!bytes.is_empty() && !bytes.ends_with(b"\n")) || bytes.contains(&0) {
                    return Err("private Git reflog has an incomplete record".to_owned());
                }
                for record in bytes.split_inclusive(|byte| *byte == b'\n') {
                    let row = record
                        .strip_suffix(b"\n")
                        .ok_or("private Git reflog record is incomplete")?;
                    if row.is_empty() {
                        return Err("private Git reflog has an empty record".to_owned());
                    }
                    let mut fields = row.splitn(3, |byte| *byte == b' ');
                    let old = fields.next().ok_or("private Git reflog is malformed")?;
                    let new = fields.next().ok_or("private Git reflog is malformed")?;
                    let rest = fields.next().ok_or("private Git reflog is malformed")?;
                    // Git omits TAB when the message is empty; a committer tail remains.
                    // Both OID fields carry retained history, and the tail stays opaque.
                    if rest.is_empty() {
                        return Err("private Git reflog is malformed".to_owned());
                    }
                    for oid in [old, new] {
                        let oid = std::str::from_utf8(oid)
                            .map_err(|_| "private Git reflog object is malformed")?;
                        if oid.len() != width
                            || !oid
                                .bytes()
                                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                        {
                            return Err("private Git reflog object is malformed".to_owned());
                        }
                        if !oid.bytes().all(|byte| byte == b'0') {
                            roots.insert(oid.to_owned());
                        }
                    }
                    if roots.len() > ROOTS {
                        return Err("private Git roots reached their work bound".to_owned());
                    }
                }
            }
        }
        if roots.len() > ROOTS || (!reflogs && roots.len() > WITNESS_ROOTS) {
            return Err("private Git roots reached their work bound".to_owned());
        }
        Ok(roots)
    }
}

/// `for-each-ref`'s row for each ref: its name and object.
const REF_ROWS: &str = "--format=%(refname)%00%(objectname)";

/// A history read's output, where git exited 0 and said nothing on standard error.
fn completed(answer: super::Answer) -> Result<String, String> {
    if answer.code != Some(0) || !answer.err.is_empty() {
        return Err(format!(
            "original Git history read refused: {}",
            answer.err.trim()
        ));
    }
    Ok(answer.out)
}

/// Whether `name` is a root ref as `for-each-ref --include-root-refs` lists one (git 2.56.0,
/// measured): its name only capitals, `-` and `_`; `HEAD`, a name ending in `_HEAD`, or one of
/// the irregular root refs; never the pseudorefs `FETCH_HEAD` and `MERGE_HEAD`, which can hold
/// several objects.
fn root_ref(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || matches!(byte, b'-' | b'_'))
        && !matches!(name, "FETCH_HEAD" | "MERGE_HEAD")
        && (name.ends_with("_HEAD")
            || matches!(
                name,
                "HEAD"
                    | "AUTO_MERGE"
                    | "BISECT_EXPECTED_REV"
                    | "NOTES_MERGE_PARTIAL"
                    | "NOTES_MERGE_REF"
                    | "MERGE_AUTOSTASH"
            ))
}

fn oid_valid(oid: &str, width: usize) -> bool {
    oid.len() == width
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        && !oid.bytes().all(|byte| byte == b'0')
}

fn location(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or("private Git cut has no parent")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    Ok(parent.join(path.file_name().ok_or("private Git cut has no name")?))
}

fn witness_path(
    checkout: &Path,
    wt: &Path,
    admin: Option<&Path>,
    store: &Path,
) -> Result<PathBuf, String> {
    let namespace = admin
        .and_then(|admin| store.strip_prefix(admin.join("modules")).ok())
        .or_else(|| store.strip_prefix(wt.join(".aterm-verify/modules")).ok())
        .ok_or("private Git store has no canonical module witness")?;
    if namespace.as_os_str().is_empty()
        || !namespace
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("private Git module namespace is unproven".to_owned());
    }
    let parent = checkout
        .join(".git")
        .canonicalize()
        .map_err(|error| error.to_string())?;
    Ok(parent.join("modules").join(namespace))
}

fn outside(path: &Path, cuts: &[PathBuf], private: &[Directory]) -> Result<PathBuf, String> {
    let resolved = path.canonicalize().map_err(|error| error.to_string())?;
    if cuts.iter().any(|cut| resolved.starts_with(cut)) {
        return Err("canonical Git witness is inside a retiring tree".to_owned());
    }
    let mut prohibited = Vec::new();
    for path in cuts {
        if let Some(directory) = Directory::root(path).map_err(|error| error.to_string())? {
            prohibited.push(
                stamp(
                    path,
                    &directory.metadata().map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?
                .identity,
            );
        }
    }
    for directory in private {
        prohibited.push(
            stamp(
                &directory.path,
                &directory.metadata().map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .identity,
        );
        let objects = directory
            .child(OsStr::new("objects"))
            .map_err(|error| error.to_string())?;
        prohibited.push(
            stamp(
                &objects.path,
                &objects.metadata().map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?
            .identity,
        );
    }
    for ancestor in resolved.ancestors() {
        let meta = fs::symlink_metadata(ancestor).map_err(|error| error.to_string())?;
        if prohibited.contains(
            &stamp(ancestor, &meta)
                .map_err(|error| error.to_string())?
                .identity,
        ) {
            return Err("canonical Git witness aliases retiring metadata".to_owned());
        }
    }
    Ok(resolved)
}

struct Proof {
    private: usize,
    private_snapshot: Snapshot,
    private_roots: BTreeSet<String>,
    witness: Directory,
    candidate: PathBuf,
    witness_snapshot: Snapshot,
    witness_roots: BTreeSet<String>,
    width: usize,
}

fn check(git: &dyn Git, checkout: &Path, wt: &Path, admin: Option<&Path>) -> Result<(), String> {
    let private = registry::clear(wt, admin)?;
    if private.is_empty() {
        return Ok(());
    }
    let mut budget = Budget::new();
    let read = Read {
        git,
        checkout,
        deadline: Instant::now() + ELAPSED,
    };
    let mut cuts = vec![location(wt)?];
    if let Some(admin) = admin {
        cuts.push(location(admin)?);
    }
    let mut proofs = Vec::new();
    for (index, directory) in private.iter().enumerate() {
        let snapshot =
            Snapshot::read(directory, false, &mut budget).map_err(|error| error.to_string())?;
        let width = read.format(&snapshot, false)?;
        let candidate = witness_path(checkout, wt, admin, &directory.path)?;
        let path = outside(&candidate, &cuts, &private)?;
        let witness = Directory::root(&path)
            .map_err(|error| error.to_string())?
            .ok_or("canonical Git module witness is missing")?;
        let witness_snapshot =
            Snapshot::read(&witness, true, &mut budget).map_err(|error| error.to_string())?;
        if read.format(&witness_snapshot, true)? != width {
            return Err("canonical Git witness uses another object format".to_owned());
        }
        let objects = witness.path.join("objects");
        // Ref enumeration need not read the private store's possibly broken alternates.
        // Use only the witness's bound local objects; its complete rooted closure is
        // proved below before any private store can be admitted.
        let roots = read.roots(&directory.path, &snapshot, width, true, Some(&objects))?;
        let witness_roots = read.roots(&witness.path, &witness_snapshot, width, false, None)?;
        let input = witness_roots
            .iter()
            .map(|root| format!("{root}\n"))
            .collect::<String>();
        let listed = read.git(
            &witness.path,
            &[
                "rev-list",
                "--objects",
                "--no-object-names",
                "--missing=print",
                "--stdin",
            ],
            input.as_bytes(),
            None,
        )?;
        let mut covered = BTreeSet::new();
        for oid in listed.lines() {
            if !oid_valid(oid, width) {
                return Err(
                    "canonical Git witness has missing or unreadable original objects".to_owned(),
                );
            }
            covered.insert(oid);
        }
        if let Some(missing) = roots.iter().find(|root| !covered.contains(root.as_str())) {
            return Err(format!(
                "private Git root {missing} is not rooted in the canonical module witness"
            ));
        }
        let mut fsck = vec![
            "fsck",
            "--full",
            "--strict",
            "--no-reflogs",
            "--no-dangling",
        ];
        fsck.extend(witness_roots.iter().map(String::as_str));
        read.git(&witness.path, &fsck, &[], None)?;
        proofs.push(Proof {
            private: index,
            private_snapshot: snapshot,
            private_roots: roots,
            witness,
            candidate,
            witness_snapshot,
            witness_roots,
            width,
        });
    }
    for proof in proofs {
        let directory = &private[proof.private];
        let snapshot =
            Snapshot::read(directory, false, &mut budget).map_err(|error| error.to_string())?;
        let witness_snapshot =
            Snapshot::read(&proof.witness, true, &mut budget).map_err(|error| error.to_string())?;
        if snapshot != proof.private_snapshot
            || witness_snapshot != proof.witness_snapshot
            || read.roots(
                &directory.path,
                &snapshot,
                proof.width,
                true,
                Some(&proof.witness.path.join("objects")),
            )? != proof.private_roots
            || read.roots(
                &proof.witness.path,
                &witness_snapshot,
                proof.width,
                false,
                None,
            )? != proof.witness_roots
            || outside(&proof.candidate, &cuts, &private)? != proof.witness.path
        {
            return Err(
                "private Git history or its canonical witness changed during admission".to_owned(),
            );
        }
    }
    let final_stores = registry::clear(wt, admin)?;
    if final_stores.len() != private.len() {
        return Err("private Git store inventory changed during admission".to_owned());
    }
    for (after, before) in final_stores.iter().zip(&private) {
        let after_stamp = stamp(
            &after.path,
            &after.metadata().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let before_stamp = stamp(
            &before.path,
            &before.metadata().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if after.path != before.path || after_stamp != before_stamp {
            return Err("private Git store inventory changed during admission".to_owned());
        }
    }
    Ok(())
}

/// Recheck registry custody and all private history before either retiring tree is removed.
pub(super) fn clear(
    git: &dyn Git,
    checkout: &Path,
    wt: &Path,
    admin: Option<&Path>,
) -> Result<(), String> {
    check(git, checkout, wt, admin).map_err(|error| format!("private Git history stays: {error}"))
}
