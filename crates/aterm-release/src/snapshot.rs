// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release's source take: the bytes its one build compiles, read from git objects.

use aterm_uds::spawnfd::{Stdio, command};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

/// The digest's schema tag: a change to what the digest covers changes this.
pub const SCHEMA: &str = "aterm-release-snapshot-v1";

/// The source registry every vendored package must come from.
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// The sealed source-replacement config, at `<snapshot>/.cargo/config.toml`.
pub const SEALED_CONFIG: &str = "# Written by the release cutter (crates/aterm-release/src/snapshot.rs):\n\
# every crates.io package comes from the vendored, checksum-verified tree\n\
# beside `src/`, and nothing is fetched while the release builds.\n\
[source.crates-io]\n\
replace-with = \"aterm-vendored\"\n\
\n\
[source.aterm-vendored]\n\
directory = \"vendor\"\n\
\n\
[net]\n\
offline = true\n";

/// The system tool path every child here runs under, behind the tool's own dir.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The per-clone release cache: `<clone>-release.noindex`, beside the clone.
#[derive(Debug, Clone)]
pub struct CacheRoot {
    root: PathBuf,
}

/// One take's exclusive hold on its [`CacheRoot`], from [`CacheRoot::hold`] until it drops.
pub struct CacheHold {
    cache: CacheRoot,
    _lock: aterm_update_core::FileLock,
}

impl CacheHold {
    /// The cache this hold is on: every take path ([`materialize`], the build's directories) is reached through a hold, never through a bare [`CacheRoot`].
    #[must_use]
    pub fn cache(&self) -> &CacheRoot {
        &self.cache
    }
}

impl Drop for CacheHold {
    fn drop(&mut self) {
        // a holder that has let go.
        let _ = fs::remove_file(self.cache.root.join("holder"));
    }
}

impl CacheRoot {
    /// The cache of the clone `repo` belongs to: `repo` may be the clone itself or any linked worktree of it (the cut tree is one), which share one cache.
    pub fn for_repo(repo: &Path) -> Result<Self, String> {
        let clone = main_checkout(repo)?;
        let name = clone
            .file_name()
            .ok_or_else(|| format!("{} has no name to derive its cache from", clone.display()))?;
        let mut leaf = name.to_os_string();
        leaf.push("-release.noindex");
        Self::at(clone.with_file_name(leaf))
    }

    /// The cache at `root`, created private when absent.
    pub fn at(root: PathBuf) -> Result<Self, String> {
        private_dir(&root, "release cache")?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Hold the cache for one take.
    pub fn hold(&self) -> Result<CacheHold, String> {
        match self.try_hold()? {
            Ok(hold) => Ok(hold),
            Err(busy) => {
                println!("==> {busy}; waiting for it to finish (^C stops this take)");
                let lock = aterm_update_core::FileLock::acquire(&self.root.join("lock"))
                    .map_err(|e| format!("hold the release cache {}: {e}", self.root.display()))?;
                self.held(lock)
            }
        }
    }

    /// Hold the cache when no other take does.
    pub fn try_hold(&self) -> Result<Result<CacheHold, String>, String> {
        let lock = self.root.join("lock");
        match aterm_update_core::FileLock::acquire_within(&lock, std::time::Duration::ZERO) {
            Ok(lock) => self.held(lock).map(Ok),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                let who = fs::read_to_string(self.root.join("holder"))
                    .ok()
                    .map(|note| note.trim().to_owned())
                    .filter(|note| !note.is_empty())
                    .unwrap_or_else(|| "another process".to_owned());
                Ok(Err(format!(
                    "the release cache {} is held by {who}, another take of this clone",
                    self.root.display()
                )))
            }
            Err(e) => Err(format!("hold the release cache {}: {e}", lock.display())),
        }
    }

    fn held(&self, lock: aterm_update_core::FileLock) -> Result<CacheHold, String> {
        let hold = CacheHold {
            cache: self.clone(),
            _lock: lock,
        };
        let holder = self.root.join("holder");
        let _ = fs::remove_file(&holder);
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&holder)
            .and_then(|mut note| writeln!(note, "pid {}", std::process::id()))
            .map_err(|e| format!("write {}: {e}", holder.display()))?;
        Ok(hold)
    }

    /// `snapshot/`: `src/`, `vendor/` and the sealed `.cargo/config.toml`.
    #[must_use]
    pub fn snapshot(&self) -> PathBuf {
        self.root.join("snapshot")
    }

    /// The vendoring step's private `CARGO_HOME` (its verified `.crate` cache).
    #[must_use]
    pub fn vendor_cargo_home(&self) -> PathBuf {
        self.root.join("cargo-home")
    }

    /// The builds' `CARGO_HOME` root, emptied before every build (one per slice under it): an offline build over the vendored tree needs nothing in it.
    #[must_use]
    pub fn build_cargo_home(&self) -> PathBuf {
        self.root.join("cargo-home-build")
    }

    /// The builds' target root (one `CARGO_TARGET_DIR` per slice under it), kept warm across builds.
    #[must_use]
    pub fn target(&self) -> PathBuf {
        self.root.join("target")
    }

    /// The release's bundles, built once and kept by what they were built as (`artifact.rs`): `artifacts/<key>/`, COMPLETE when its record is there.
    #[must_use]
    pub fn artifacts(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    /// The build's stage (lipo, strip, dSYM), recreated for every build.
    #[must_use]
    pub fn stage(&self) -> PathBuf {
        self.root.join("stage")
    }

    /// `TMPDIR` for every child the take runs.
    #[must_use]
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// `ship check`'s export and wasm32 gates: each run's engine copy, scratch clone, wasm32 source copy and logs under `runs/`, and the shared wasm32 target directory.
    #[must_use]
    pub fn preflight(&self) -> PathBuf {
        self.root.join("preflight")
    }

    /// Where the release's own handoff fixtures are staged before the claim and landed from after the cut (`crate::fixtures`): one directory per reserved identity, outside the clone, so writing them never dirties the cut tree.
    #[must_use]
    pub fn handoff_fixtures(&self) -> PathBuf {
        self.root.join("handoff-fixtures")
    }
}

/// The working tree of the clone `repo` belongs to: the parent of its common git directory when that is a `.git`, else `repo` itself.
pub(crate) fn main_checkout(repo: &Path) -> Result<PathBuf, String> {
    let common = git_text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(common.trim());
    let checkout = if common.file_name() == Some(OsStr::new(".git")) {
        common
            .parent()
            .map_or_else(|| repo.to_path_buf(), Path::to_path_buf)
    } else {
        repo.to_path_buf()
    };
    fs::canonicalize(&checkout).map_err(|e| format!("resolve {}: {e}", checkout.display()))
}

/// Create `path` private (0700) when absent, then hold it to the crate's one rule for a release directory ([`crate::buildplan::open_release_directory`]): a real directory this user owns, made private through its opened descriptor, never a symlink.
pub fn private_dir(path: &Path, what: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create {what} {}: {e}", path.display())),
        }
    }
    let uid = crate::buildplan::current_release_uid()?;
    crate::buildplan::open_release_directory(path, uid, true, what).map(drop)
}

/// The source take a release builds, synced and sealed.
#[derive(Debug)]
pub struct Snapshot {
    /// `<cache>/snapshot`.
    pub root: PathBuf,
    /// `<root>/src`: commit C's tree exactly, submodules included (`<root>/vendor` holds every crates.io package its lockfile names).
    pub src: PathBuf,
    /// `<root>/.cargo/config.toml`: [`SEALED_CONFIG`].
    pub sealed_config: PathBuf,
    /// The full commit C the take is of.
    pub commit: String,
    /// [`SCHEMA`]'s SHA-256 over the take, lowercase hex.
    pub digest: String,
    /// What the digest was taken over, kept so [`Snapshot::verify`] can name what
    /// changed.
    listing: Listing,
}

/// `relpath -> "kind\0mode\0hash-or-target"`, sorted by path bytes.
type Listing = BTreeMap<Vec<u8>, Vec<u8>>;

/// What a sync did, for the transcript.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    pub written: usize,
    pub unchanged: usize,
    pub removed: usize,
}

/// Materialize commit `commit` of `repo` (and its submodules) into the held cache's `src/` and make it exact ([`take_tree`]); only then vendor its crates.io dependencies with `targo` from that tree, write the sealed config, prune the rest of the snapshot ([`Syncer::finish`]), and digest it.
/// # Errors
/// A commit that does not resolve, an unfetched submodule commit, a symbolic link, an object whose bytes do not hash to its id, two tree paths the volume cannot hold apart, a lockfile source other than crates.io or a package without a checksum, a vendoring failure or a vendored result that disagrees with the lock, a root Cargo config, and any filesystem failure.
pub fn materialize(
    repo: &Path,
    commit: &str,
    hold: &CacheHold,
    targo: &Path,
) -> Result<Snapshot, String> {
    let cache = hold.cache();
    let commit = git_text(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{commit}^{{commit}}"),
        ],
    )
    .map_err(|e| {
        format!(
            "the release commit {commit} does not resolve in {}: {e}",
            repo.display()
        )
    })?;
    let commit = commit.trim().to_owned();
    let root = cache.snapshot();
    private_dir(&root, "release snapshot")?;
    let mut sync = Syncer::new(&root);
    take_tree(repo, &commit, &mut sync)?;

    let src = root.join("src");
    let vendored = vendor_into(&src, cache, targo, &mut sync)?;
    sync.put(
        Path::new(".cargo/config.toml"),
        SEALED_CONFIG.as_bytes(),
        false,
    )?;
    let report = sync.finish()?;
    let listing = listing(&root)?;
    let digest = digest_of(&listing);
    println!(
        "==> snapshot {} from git objects into {}: {} written, {} unchanged, {} removed; \
         {vendored} crates vendored; digest {}",
        &commit[..commit.len().min(12)],
        cache.path().display(),
        report.written,
        report.unchanged,
        report.removed,
        &digest[..12]
    );
    Ok(Snapshot {
        src,
        sealed_config: root.join(".cargo/config.toml"),
        root,
        commit,
        digest,
        listing,
    })
}

impl Snapshot {
    /// Walk the take again and refuse when any byte, mode, link or path differs from what [`materialize`] digested — run after the build, so a source the build (or anything else) changed while cargo ran is a refusal, not a release.
    pub fn verify(&self) -> Result<(), String> {
        let now = listing(&self.root)?;
        if digest_of(&now) == self.digest {
            return Ok(());
        }
        let mut changed: Vec<String> = Vec::new();
        for (path, line) in &self.listing {
            match now.get(path) {
                None => changed.push(format!("{} (removed)", String::from_utf8_lossy(path))),
                Some(seen) if seen != line => {
                    changed.push(format!("{} (changed)", String::from_utf8_lossy(path)));
                }
                Some(_) => {}
            }
        }
        for path in now.keys().filter(|p| !self.listing.contains_key(*p)) {
            changed.push(format!("{} (added)", String::from_utf8_lossy(path)));
        }
        let shown = changed.iter().take(10).cloned().collect::<Vec<_>>();
        Err(format!(
            "the release snapshot {} changed while it was built ({} path(s)): {}{} — \
             nothing it built is used",
            self.root.display(),
            changed.len(),
            shown.join(", "),
            if changed.len() > shown.len() {
                ", …"
            } else {
                ""
            }
        ))
    }
}

/// One file of the tree: which repository holds its blob, where it goes, and how.
struct TreeEntry {
    repo: usize,
    path: Vec<u8>,
    exec: bool,
    oid: String,
}

fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = crate::ledger::object_git(dir)
        .args(args)
        .output()
        .map_err(|e| format!("spawn git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} in {} failed ({}): {}",
            args.join(" "),
            dir.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// `git <args>` in `dir` through [`crate::ledger::object_git`], its stdout as text.
pub(crate) fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git_bytes(dir, args)?)
        .map_err(|e| format!("git {} printed non-UTF-8: {e}", args.join(" ")))
}

/// The id git gives an object of `kind` holding `data`: the object format's hash
/// (SHA-1 for a 40-digit id, SHA-256 for a 64-digit one) over `<kind> <len>\0<data>`.
fn object_id(kind: &str, data: &[u8], hex_digits: usize) -> Option<String> {
    let algorithm = match hex_digits {
        40 => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
        64 => &ring::digest::SHA256,
        _ => return None,
    };
    let mut hash = ring::digest::Context::new(algorithm);
    hash.update(format!("{kind} {}\0", data.len()).as_bytes());
    hash.update(data);
    Some(aterm_codec::hex::encode_lower(hash.finish().as_ref()))
}

/// One repository's `git cat-file --batch`, asked for one object at a time, every answer
/// held to the id and kind asked for.
struct ObjectReader {
    dir: PathBuf,
    child: aterm_uds::spawnfd::Child,
    stdin: Option<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl ObjectReader {
    fn spawn(dir: &Path) -> Result<Self, String> {
        let mut child = crate::ledger::object_git(dir)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn git cat-file in {}: {e}", dir.display()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("git cat-file in {} has no pipes", dir.display()));
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
        })
    }

    /// The bytes of the object `oid`, which must be a `kind`: refused unless git
    /// answers for that id with that kind, and the bytes hash to that id.
    fn read(&mut self, oid: &str, kind: &str) -> Result<Vec<u8>, String> {
        let read = self.answer(oid, kind);
        if read.is_err() {
            drop(self.stdin.take());
        }
        read
    }

    fn answer(&mut self, oid: &str, kind: &str) -> Result<Vec<u8>, String> {
        let dir = self.dir.display();
        if !(aterm_codec::hex::is_lower(oid, 40) || aterm_codec::hex::is_lower(oid, 64)) {
            return Err(format!(
                "refusing to read {oid:?} in {dir}: not an object id"
            ));
        }
        let stdin = self.stdin.as_mut().ok_or("git cat-file is closed")?;
        stdin
            .write_all(format!("{oid}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("ask git cat-file in {dir} for {oid}: {e}"))?;
        let mut header = Vec::new();
        self.stdout
            .read_until(b'\n', &mut header)
            .map_err(|e| format!("read git cat-file in {dir}: {e}"))?;
        let line = String::from_utf8_lossy(&header);
        let line = line.trim_end_matches('\n');
        let mut words = line.split(' ');
        let (Some(echo), Some(seen), Some(size), None) =
            (words.next(), words.next(), words.next(), words.next())
        else {
            return Err(format!(
                "git cat-file in {dir} cannot read the {kind} {oid} ({line})"
            ));
        };
        if echo != oid || seen != kind {
            return Err(format!(
                "git cat-file in {dir} answered {echo} {seen} for the {kind} {oid}"
            ));
        }
        let size: u64 = size
            .parse()
            .map_err(|_| format!("git cat-file in {dir} answered a bad size: {line}"))?;
        let mut data = Vec::new();
        (&mut self.stdout)
            .take(size.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|e| format!("read the {kind} {oid} in {dir}: {e}"))?;
        if data.pop() != Some(b'\n') || data.len() as u64 != size {
            return Err(format!("git cat-file in {dir} cut the {kind} {oid} short"));
        }
        if object_id(kind, &data, oid.len()).as_deref() != Some(oid) {
            return Err(format!(
                "the object store of {dir} answered the {kind} {oid} with bytes that are not \
                 that object (a corrupted or substituted object)"
            ));
        }
        Ok(data)
    }

    /// End git: close its stdin, drain what a refusal left unread (so it cannot block
    /// on a full pipe before it reads the end), and wait for it.
    fn close(&mut self) -> (std::io::Result<std::process::ExitStatus>, String) {
        drop(self.stdin.take());
        let _ = std::io::copy(&mut self.stdout, &mut std::io::sink());
        let mut stderr = Vec::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = pipe.read_to_end(&mut stderr);
        }
        (
            self.child.wait(),
            String::from_utf8_lossy(&stderr).into_owned(),
        )
    }

    /// Close the reader, refusing unless git exited cleanly.
    fn finish(mut self) -> Result<(), String> {
        let (status, stderr) = self.close();
        let status =
            status.map_err(|e| format!("wait for git cat-file in {}: {e}", self.dir.display()))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "git cat-file in {} failed ({status}): {}",
                self.dir.display(),
                stderr.trim()
            ))
        }
    }
}

impl Drop for ObjectReader {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Append every file of `commit`'s tree in `repos[repo]` under `prefix`, recursing into
/// each gitlink at the commit it records, one [`ObjectReader`] per repository.
fn list_tree(
    repos: &mut Vec<PathBuf>,
    repo: usize,
    commit: &str,
    prefix: &[u8],
    out: &mut Vec<TreeEntry>,
) -> Result<(), String> {
    let dir = repos[repo].clone();
    let mut reader = ObjectReader::spawn(&dir)?;
    let gitlinks = walk_commit(&mut reader, repo, commit, prefix, out)?;
    reader.finish()?;
    for (path, oid) in gitlinks {
        let sub = dir.join(os_path(&path));
        submodule_at(&sub, &oid, &path)?;
        repos.push(sub);
        let index = repos.len() - 1;
        let mut full = prefix.to_vec();
        full.extend_from_slice(&path);
        full.push(b'/');
        list_tree(repos, index, &oid, &full, out)?;
    }
    Ok(())
}

/// Read `commit` and walk its tree: append each file to `out`, refuse what a release
/// cannot build from, and return each gitlink — its path in this repository and the
/// commit it records — for [`list_tree`] to recurse into.
fn walk_commit(
    reader: &mut ObjectReader,
    repo: usize,
    commit: &str,
    prefix: &[u8],
    out: &mut Vec<TreeEntry>,
) -> Result<Vec<(Vec<u8>, String)>, String> {
    let refusing = |e: String, rel: &[u8]| {
        let mut path = prefix.to_vec();
        path.extend_from_slice(rel);
        format!(
            "{e}: refusing {}",
            String::from_utf8_lossy(&path).trim_end_matches('/')
        )
    };
    let data = reader
        .read(commit, "commit")
        .map_err(|e| refusing(e, b""))?;
    let root = commit_tree(&data, commit.len()).ok_or_else(|| {
        format!(
            "the commit {commit} in {} names no tree",
            reader.dir.display()
        )
    })?;
    let mut gitlinks = Vec::new();
    let mut trees = vec![(root, Vec::new())];
    while let Some((oid, rel)) = trees.pop() {
        let data = reader.read(&oid, "tree").map_err(|e| refusing(e, &rel))?;
        let entries = tree_entries(&data, commit.len() / 2).ok_or_else(|| {
            format!(
                "the tree {oid} in {} does not parse as one",
                reader.dir.display()
            )
        })?;
        for RawEntry { mode, name, id } in entries {
            let mut path = rel.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name);
            check_relative(&path)?;
            let exec = match mode {
                b"40000" => {
                    trees.push((id, path));
                    continue;
                }
                b"100644" => false,
                b"100755" => true,
                b"120000" => {
                    return Err(format!(
                        "{} has a symbolic link at {}: a release builds from regular files \
                         only (what a link reaches is no byte of the snapshot, and its digest \
                         could not see it)",
                        reader.dir.display(),
                        String::from_utf8_lossy(&path)
                    ));
                }
                b"160000" => {
                    gitlinks.push((path, id));
                    continue;
                }
                other => {
                    return Err(format!(
                        "{} has a tree entry of mode {} at {}, which a release cannot build \
                         from",
                        reader.dir.display(),
                        String::from_utf8_lossy(other),
                        String::from_utf8_lossy(&path)
                    ));
                }
            };
            let mut full = prefix.to_vec();
            full.extend_from_slice(&path);
            out.push(TreeEntry {
                repo,
                path: full,
                exec,
                oid: id,
            });
        }
    }
    Ok(gitlinks)
}

/// The root tree a commit object names: its first line, `tree <id>`.
fn commit_tree(data: &[u8], hex_digits: usize) -> Option<String> {
    let line = data.strip_prefix(b"tree ")?.get(..=hex_digits)?;
    let (id, end) = line.split_at(hex_digits);
    let id = std::str::from_utf8(id).ok()?;
    (end == b"\n" && aterm_codec::hex::is_lower(id, hex_digits)).then(|| id.to_owned())
}

/// One entry of a tree object, as git stores it: `<octal mode> <name>\0<raw id>`.
struct RawEntry<'a> {
    mode: &'a [u8],
    name: &'a [u8],
    /// The raw id, in lowercase hex.
    id: String,
}

/// A tree object's entries, each id `raw_len` bytes.
fn tree_entries(data: &[u8], raw_len: usize) -> Option<Vec<RawEntry<'_>>> {
    let mut entries = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest.iter().position(|b| *b == b' ')?;
        let (mode, after) = (&rest[..space], &rest[space + 1..]);
        let nul = after.iter().position(|b| *b == 0)?;
        let (name, after) = (&after[..nul], &after[nul + 1..]);
        if mode.is_empty() || name.contains(&b'/') || after.len() < raw_len {
            return None;
        }
        let (id, after) = after.split_at(raw_len);
        entries.push(RawEntry {
            mode,
            name,
            id: aterm_codec::hex::encode_lower(id),
        });
        rest = after;
    }
    Some(entries)
}

/// Refuse unless `sub` is the top of its own git checkout and holds commit `oid`.
fn submodule_at(sub: &Path, oid: &str, path: &[u8]) -> Result<(), String> {
    let shown = String::from_utf8_lossy(path);
    let refuse = |why: &str| {
        format!(
            "the submodule {shown} is not fetched at {oid} ({why})\nfix:  git submodule update \
             --init {shown}"
        )
    };
    let top = git_text(
        sub,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    )
    .map_err(|_| refuse("it is not checked out"))?;
    let (Ok(top), Ok(here)) = (fs::canonicalize(top.trim()), fs::canonicalize(sub)) else {
        return Err(refuse("it is not checked out"));
    };
    if top != here {
        return Err(refuse("it is not checked out"));
    }
    git_bytes(sub, &["cat-file", "-e", &format!("{oid}^{{commit}}")])
        .map_err(|_| refuse("its repository does not hold that commit"))?;
    Ok(())
}

/// Sync `commit`'s tree in `repo`, submodules included, into `sync`'s `src/`, then remove
/// every path under `src/` the tree does not name ([`Syncer::prune_under`]) before anything
/// reads it.
fn take_tree(repo: &Path, commit: &str, sync: &mut Syncer) -> Result<(), String> {
    let mut repos = vec![repo.to_path_buf()];
    let mut entries = Vec::new();
    list_tree(&mut repos, 0, commit, b"src/", &mut entries)?;
    read_blobs(&repos, &entries, sync)?;
    sync.prune_under(Path::new("src"))
}

/// Stream every entry's blob through one `git cat-file --batch` per repository
/// into `sync`, each held to its tree entry's id.
fn read_blobs(repos: &[PathBuf], entries: &[TreeEntry], sync: &mut Syncer) -> Result<(), String> {
    for (index, dir) in repos.iter().enumerate() {
        let mine: Vec<&TreeEntry> = entries.iter().filter(|e| e.repo == index).collect();
        if mine.is_empty() {
            continue;
        }
        let mut child = crate::ledger::object_git(dir)
            .args(["cat-file", "--batch", "--buffer"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn git cat-file in {}: {e}", dir.display()))?;
        let mut stdin = child.stdin.take().ok_or("git cat-file has no stdin")?;
        let stdout = child.stdout.take().ok_or("git cat-file has no stdout")?;
        let read = std::thread::scope(|scope| {
            let requests: Vec<u8> = mine
                .iter()
                .flat_map(|e| e.oid.bytes().chain(std::iter::once(b'\n')))
                .collect();
            let writer = scope.spawn(move || {
                let written = stdin.write_all(&requests);
                drop(stdin);
                written
            });
            let mut reader = BufReader::new(stdout);
            let mut header = Vec::new();
            let mut result = Ok(());
            for entry in &mine {
                header.clear();
                if let Err(e) = reader.read_until(b'\n', &mut header) {
                    result = Err(format!("read git cat-file: {e}"));
                    break;
                }
                let line = String::from_utf8_lossy(&header);
                let mut words = line.split_whitespace();
                let (Some(oid), Some(kind), Some(size)) =
                    (words.next(), words.next(), words.next())
                else {
                    result = Err(format!(
                        "git cat-file in {} cannot read {} ({})",
                        dir.display(),
                        entry.oid,
                        line.trim()
                    ));
                    break;
                };
                let Ok(size) = size.parse::<usize>() else {
                    result = Err(format!("git cat-file answered a bad size: {}", line.trim()));
                    break;
                };
                if oid != entry.oid || kind != "blob" {
                    result = Err(format!(
                        "git cat-file answered {oid} {kind} for the blob {}",
                        entry.oid
                    ));
                    break;
                }
                let mut data = vec![0u8; size + 1];
                if let Err(e) = reader.read_exact(&mut data) {
                    result = Err(format!("read blob {}: {e}", entry.oid));
                    break;
                }
                data.pop();
                if object_id("blob", &data, entry.oid.len()).as_deref() != Some(&entry.oid) {
                    result = Err(format!(
                        "the object store of {} answered {} with bytes that are not that \
                         object (a corrupted or substituted object): refusing {}",
                        dir.display(),
                        entry.oid,
                        String::from_utf8_lossy(&entry.path)
                    ));
                    break;
                }
                if let Err(e) = sync.put(&os_path(&entry.path), &data, entry.exec) {
                    result = Err(e);
                    break;
                }
            }
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
            let written = writer
                .join()
                .unwrap_or_else(|_| Err(std::io::Error::other("writer panicked")));
            result.and_then(|()| written.map_err(|e| format!("feed git cat-file: {e}")))
        });
        let out = child
            .wait_with_output()
            .map_err(|e| format!("wait for git cat-file: {e}"))?;
        read?;
        if !out.status.success() {
            return Err(format!(
                "git cat-file in {} failed ({}): {}",
                dir.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn os_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    PathBuf::from(OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn os_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().replace('\\', "/").into_bytes()
}

/// A relative path of plain components only: no root, no `.`, no `..`, no empty
/// component — what git itself allows in a tree, checked again before any write.
fn check_relative(path: &[u8]) -> Result<(), String> {
    let ok = !path.is_empty()
        && path
            .split(|b| *b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b"..");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "refusing the tree path {:?}: not a plain relative path",
            String::from_utf8_lossy(path)
        ))
    }
}

/// Writes a tree into `root` one path at a time, touching only what differs, then removes every path it was not given: in a completed subtree at [`Syncer::prune_under`], everywhere else at [`Syncer::finish`].
pub struct Syncer {
    root: PathBuf,
    keep: BTreeSet<PathBuf>,
    /// The names each directory the sync has looked in holds on disk, read once and
    /// kept current by every write and removal here.
    names: BTreeMap<PathBuf, BTreeSet<OsString>>,
    report: SyncReport,
}

impl Syncer {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            keep: BTreeSet::new(),
            names: BTreeMap::new(),
            report: SyncReport::default(),
        }
    }

    /// Make `rel` a regular file holding `data` (mode 0755 when `exec`, else 0644).
    pub fn put(&mut self, rel: &Path, data: &[u8], exec: bool) -> Result<(), String> {
        let parts = take_parts(rel)?;
        let mut dir = PathBuf::new();
        for part in &parts[..parts.len() - 1] {
            dir.push(part);
            self.ensure_dir(&dir)?;
        }
        let full = self.root.join(rel);
        let existing = self.exact(rel)?;
        let same = match &existing {
            Some(meta) if meta.file_type().is_file() => {
                meta.len() == data.len() as u64
                    && fs::read(&full).map_err(|e| format!("read {}: {e}", full.display()))? == data
                    && {
                        set_mode(&full, meta, exec)?;
                        true
                    }
            }
            _ => false,
        };
        if same {
            self.report.unchanged += 1;
        } else {
            if existing.as_ref().is_some_and(|m| m.file_type().is_dir()) {
                fs::remove_dir_all(&full)
                    .map_err(|e| format!("replace {}: {e}", full.display()))?;
                self.forget(rel);
            }
            write_beside(&full, data, exec)?;
            self.listed(rel);
            self.report.written += 1;
        }
        self.keep.insert(rel.to_path_buf());
        Ok(())
    }

    fn ensure_dir(&mut self, rel: &Path) -> Result<(), String> {
        if self.keep.contains(rel) {
            return Ok(());
        }
        let full = self.root.join(rel);
        match self.exact(rel)? {
            Some(meta) if meta.file_type().is_dir() => {}
            Some(_) => {
                fs::remove_file(&full).map_err(|e| format!("replace {}: {e}", full.display()))?;
                fs::create_dir(&full).map_err(|e| format!("create {}: {e}", full.display()))?;
                self.names.insert(rel.to_path_buf(), BTreeSet::new());
            }
            None => {
                fs::create_dir(&full).map_err(|e| format!("create {}: {e}", full.display()))?;
                self.listed(rel);
                self.names.insert(rel.to_path_buf(), BTreeSet::new());
            }
        }
        self.keep.insert(rel.to_path_buf());
        Ok(())
    }

    /// What is at `rel` under exactly that name, or `None`: an entry the volume finds
    /// there under another spelling is removed first, unless this sync wrote it — then
    /// the tree names both spellings, and that is refused.
    fn exact(&mut self, rel: &Path) -> Result<Option<fs::Metadata>, String> {
        let full = self.root.join(rel);
        let meta = match fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("inspect {}: {e}", full.display())),
        };
        let parent = rel.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let name = rel
            .file_name()
            .ok_or_else(|| format!("refusing the take path {}", rel.display()))?;
        if self.names_in(&parent)?.contains(name) {
            return Ok(Some(meta));
        }
        let dir = self.root.join(&parent);
        let other = self
            .names_in(&parent)?
            .iter()
            .find(|listed| {
                fs::symlink_metadata(dir.join(listed)).is_ok_and(|seen| same_entry(&seen, &meta))
            })
            .cloned()
            .ok_or_else(|| {
                format!(
                    "{} finds an entry that {} does not list",
                    full.display(),
                    dir.display()
                )
            })?;
        let other_rel = parent.join(&other);
        if self.keep.contains(&other_rel) {
            return Err(format!(
                "the take names both {} and {}, which this volume cannot hold apart (the \
                 names differ only in case or Unicode normalization)",
                other_rel.display(),
                rel.display()
            ));
        }
        let stale = dir.join(&other);
        if meta.file_type().is_dir() {
            fs::remove_dir_all(&stale)
        } else {
            fs::remove_file(&stale)
        }
        .map_err(|e| format!("remove {}: {e}", stale.display()))?;
        self.names_in(&parent)?.remove(&other);
        self.forget(&other_rel);
        self.report.removed += 1;
        Ok(None)
    }

    /// The names `rel` holds on disk, read the first time it is asked about.
    fn names_in(&mut self, rel: &Path) -> Result<&mut BTreeSet<OsString>, String> {
        if !self.names.contains_key(rel) {
            let dir = self.root.join(rel);
            let mut names = BTreeSet::new();
            for entry in fs::read_dir(&dir).map_err(|e| format!("list {}: {e}", dir.display()))? {
                let entry = entry.map_err(|e| format!("list {}: {e}", dir.display()))?;
                names.insert(entry.file_name());
            }
            self.names.insert(rel.to_path_buf(), names);
        }
        Ok(self
            .names
            .get_mut(rel)
            .expect("the listing was just inserted"))
    }

    /// `rel` now exists under its own name.
    fn listed(&mut self, rel: &Path) {
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        if let (Some(names), Some(name)) = (self.names.get_mut(parent), rel.file_name()) {
            names.insert(name.to_os_string());
        }
    }

    /// `rel` is gone, and every listing under it with it.
    fn forget(&mut self, rel: &Path) {
        self.names.retain(|dir, _| !dir.starts_with(rel));
    }

    /// `rel` (a file or a whole directory) was removed: counted, taken out of its
    /// directory's listing, and every listing under it forgotten, so a later
    /// [`Syncer::put`] sees the disk as it now is.
    fn removed(&mut self, rel: &Path) {
        let parent = rel.parent().unwrap_or_else(|| Path::new(""));
        if let (Some(names), Some(name)) = (self.names.get_mut(parent), rel.file_name()) {
            names.remove(name);
        }
        self.forget(rel);
        self.report.removed += 1;
    }

    /// Remove now every path at or under `rel` that no [`Syncer::put`] has named, for a subtree whose every path has been put; paths outside `rel` wait for [`Syncer::finish`].
    pub fn prune_under(&mut self, rel: &Path) -> Result<(), String> {
        take_parts(rel)?;
        let full = self.root.join(rel);
        if self.keep.contains(rel) {
            let meta = fs::symlink_metadata(&full)
                .map_err(|e| format!("inspect {}: {e}", full.display()))?;
            return if meta.is_dir() {
                self.prune(&full, rel)
            } else {
                Ok(())
            };
        }
        if let Some(meta) = self.exact(rel)? {
            if meta.is_dir() {
                fs::remove_dir_all(&full)
            } else {
                fs::remove_file(&full)
            }
            .map_err(|e| format!("remove {}: {e}", full.display()))?;
            self.removed(rel);
        }
        Ok(())
    }

    /// Remove every remaining path under the root that no [`Syncer::put`] named, and report.
    pub fn finish(mut self) -> Result<SyncReport, String> {
        let root = self.root.clone();
        self.prune(&root, Path::new(""))?;
        Ok(self.report)
    }

    fn prune(&mut self, dir: &Path, rel: &Path) -> Result<(), String> {
        let entries = fs::read_dir(dir).map_err(|e| format!("list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("list {}: {e}", dir.display()))?;
            let here = rel.join(entry.file_name());
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|e| format!("inspect {}: {e}", path.display()))?;
            if !self.keep.contains(&here) {
                if kind.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                }
                .map_err(|e| format!("remove {}: {e}", path.display()))?;
                self.removed(&here);
            } else if kind.is_dir() {
                self.prune(&path, &here)?;
            }
        }
        Ok(())
    }
}

/// The components of a take path, refused unless it is a non-empty relative path of
/// plain components: nothing a [`Syncer`] writes or removes is outside its root.
fn take_parts(rel: &Path) -> Result<Vec<&OsStr>, String> {
    let parts: Vec<&OsStr> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => Ok(part),
            _ => Err(format!("refusing the take path {}", rel.display())),
        })
        .collect::<Result<_, _>>()?;
    if parts.is_empty() {
        return Err("refusing an empty take path".into());
    }
    Ok(parts)
}

/// Whether two inspections are of one directory entry.
#[cfg(unix)]
fn same_entry(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

/// Without inode identity, no other spelling is ever taken for this one: [`Syncer`]
/// refuses rather than remove what it cannot name.
#[cfg(not(unix))]
fn same_entry(_a: &fs::Metadata, _b: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_mode(path: &Path, meta: &fs::Metadata, exec: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let want = if exec { 0o755 } else { 0o644 };
    if meta.permissions().mode() & 0o7777 != want {
        fs::set_permissions(path, fs::Permissions::from_mode(want))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _meta: &fs::Metadata, _exec: bool) -> Result<(), String> {
    Ok(())
}

/// Write `data` to a temporary sibling of `full` and rename it into place.
fn write_beside(full: &Path, data: &[u8], exec: bool) -> Result<(), String> {
    let name = full
        .file_name()
        .ok_or_else(|| format!("no file name: {}", full.display()))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".aterm-sync-{}", std::process::id()));
    let tmp = full.with_file_name(tmp_name);
    let _ = fs::remove_file(&tmp);
    fs::write(&tmp, data).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    let meta = fs::symlink_metadata(&tmp).map_err(|e| format!("inspect {}: {e}", tmp.display()))?;
    set_mode(&tmp, &meta, exec)?;
    fs::rename(&tmp, full).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("place {}: {e}", full.display())
    })
}

/// One crates.io package the lockfile names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedCrate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

impl LockedCrate {
    /// `<name>-<version>`: its `.crate` file stem and its `--versioned-dirs` name.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

#[derive(serde::Deserialize)]
struct LockFile {
    #[serde(default)]
    package: Vec<LockPackage>,
}

#[derive(serde::Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
}

/// The crates.io packages `Cargo.lock` names, each with its checksum.
pub fn locked_crates(lock_text: &str) -> Result<Vec<LockedCrate>, String> {
    let lock: LockFile =
        aterm_toml::from_str(lock_text).map_err(|e| format!("Cargo.lock does not parse: {e}"))?;
    let mut crates = Vec::new();
    for package in lock.package {
        match package.source.as_deref() {
            None => {}
            Some(CRATES_IO) => {
                let checksum = package
                    .checksum
                    .filter(|c| aterm_codec::hex::is_lower(c, 64))
                    .ok_or_else(|| {
                        format!(
                            "Cargo.lock names {} {} from crates.io without a SHA-256 checksum",
                            package.name, package.version
                        )
                    })?;
                crates.push(LockedCrate {
                    name: package.name,
                    version: package.version,
                    checksum,
                });
            }
            Some(other) => {
                return Err(format!(
                    "Cargo.lock takes {} {} from {other}: a release builds only crates.io and \
                     path dependencies",
                    package.name, package.version
                ));
            }
        }
    }
    Ok(crates)
}

/// Every `<cargo_home>/registry/cache/*/<name>-<version>.crate` the lockfile names:
/// which locked crate it is, where, and whether its SHA-256 is the lock's checksum.
fn cached_archives<'a>(
    locked: &'a [LockedCrate],
    cargo_home: &Path,
) -> Result<Vec<(&'a LockedCrate, PathBuf, bool)>, String> {
    let cache = cargo_home.join("registry/cache");
    let indexes = match fs::read_dir(&cache) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("list {}: {e}", cache.display())),
    };
    let mut found = Vec::new();
    for index in indexes {
        let index = index.map_err(|e| format!("list {}: {e}", cache.display()))?;
        if !index.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        for krate in locked {
            let archive = index.path().join(format!("{}.crate", krate.stem()));
            match fs::read(&archive) {
                Ok(bytes) => {
                    let matches = sha256_hex(&bytes) == krate.checksum;
                    found.push((krate, archive, matches));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("read {}: {e}", archive.display())),
            }
        }
    }
    Ok(found)
}

/// Hash every cached `.crate` the lockfile names against its checksum, and delete each one that differs — cargo trusts its cache, so a bad archive left there would be vendored as it is.
pub fn verify_crate_cache(locked: &[LockedCrate], cargo_home: &Path) -> Result<usize, String> {
    let mut deleted = 0;
    for (_, archive, matches) in cached_archives(locked, cargo_home)? {
        if !matches {
            fs::remove_file(&archive).map_err(|e| format!("remove {}: {e}", archive.display()))?;
            deleted += 1;
        }
    }
    Ok(deleted)
}

/// After vendoring: every locked crate's archive is in the cache vendoring extracted it from, and every one there hashes to the lock's checksum.
pub fn check_crate_archives(locked: &[LockedCrate], cargo_home: &Path) -> Result<(), String> {
    let found = cached_archives(locked, cargo_home)?;
    if let Some((krate, archive, _)) = found.iter().find(|(_, _, matches)| !matches) {
        return Err(format!(
            "{} (vendored as {}) does not hash to its Cargo.lock checksum {}",
            archive.display(),
            krate.stem(),
            krate.checksum
        ));
    }
    if let Some(missing) = locked
        .iter()
        .find(|krate| !found.iter().any(|(seen, _, _)| seen.stem() == krate.stem()))
    {
        return Err(format!(
            "vendoring left no archive of {} in {}: its vendored bytes come from nothing the \
             lock's checksum was held to",
            missing.stem(),
            cargo_home.join("registry/cache").display()
        ));
    }
    Ok(())
}

/// Refuse a Cargo config at the filesystem root.
pub(crate) fn refuse_root_cargo_config() -> Result<(), String> {
    for config in ["/.cargo/config.toml", "/.cargo/config"] {
        match fs::symlink_metadata(config) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("inspect root Cargo config {config}: {error}")),
            Ok(_) => {
                return Err(format!(
                    "refusing ambient root Cargo config during sealed release build: {config}"
                ));
            }
        }
    }
    Ok(())
}

/// Remove any Cargo config from the vendoring `CARGO_HOME`.
fn clear_cargo_home_config(cargo_home: &Path) -> Result<(), String> {
    for name in ["config.toml", "config"] {
        let path = cargo_home.join(name);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("inspect {}: {e}", path.display())),
        };
        if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        }
        .map_err(|e| format!("remove {}: {e}", path.display()))?;
        println!(
            "    vendoring: removed {}: the cutter's CARGO_HOME holds no Cargo config",
            path.display()
        );
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct CargoChecksum {
    #[serde(default)]
    package: Option<String>,
}

/// Hold the vendored tree to the lockfile: exactly one `<name>-<version>` directory per locked package and nothing else, each with a `.cargo-checksum.json` whose `package` is the lockfile's checksum.
pub fn check_vendored(locked: &[LockedCrate], vendor: &Path) -> Result<(), String> {
    let want: BTreeMap<String, &LockedCrate> = locked.iter().map(|c| (c.stem(), c)).collect();
    let mut seen = BTreeSet::new();
    let entries = fs::read_dir(vendor).map_err(|e| format!("list {}: {e}", vendor.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("list {}: {e}", vendor.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(krate) = want.get(&name) else {
            return Err(format!(
                "vendoring produced {name}, which Cargo.lock does not name"
            ));
        };
        let json = entry.path().join(".cargo-checksum.json");
        let text = fs::read(&json).map_err(|e| format!("read {}: {e}", json.display()))?;
        let parsed: CargoChecksum = aterm_json::from_slice(&text)
            .map_err(|e| format!("{} does not parse: {e}", json.display()))?;
        if parsed.package.as_deref() != Some(krate.checksum.as_str()) {
            return Err(format!(
                "the vendored {name} checksums as {:?}, and Cargo.lock says {}",
                parsed.package, krate.checksum
            ));
        }
        seen.insert(name);
    }
    if let Some(missing) = want.keys().find(|k| !seen.contains(*k)) {
        return Err(format!(
            "vendoring left out {missing}, which Cargo.lock names"
        ));
    }
    Ok(())
}

/// Vendor `src`'s locked crates.io packages into `sync`'s `vendor/`, checked.
fn vendor_into(
    src: &Path,
    cache: &CacheRoot,
    targo: &Path,
    sync: &mut Syncer,
) -> Result<usize, String> {
    let lock_path = src.join("Cargo.lock");
    let lock_text =
        fs::read_to_string(&lock_path).map_err(|e| format!("read {}: {e}", lock_path.display()))?;
    let locked = locked_crates(&lock_text)?;
    refuse_root_cargo_config()?;
    let cargo_home = cache.vendor_cargo_home();
    private_dir(&cargo_home, "vendoring CARGO_HOME")?;
    clear_cargo_home_config(&cargo_home)?;
    let tmp = cache.tmp();
    private_dir(&tmp, "release temporary directory")?;
    let deleted = verify_crate_cache(&locked, &cargo_home)?;
    if deleted > 0 {
        println!(
            "    vendoring: {deleted} cached .crate archive(s) differed from Cargo.lock and were \
             removed; cargo fetches them again"
        );
    }
    match fs::remove_dir_all(cargo_home.join("registry/src")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("clear the unpacked registry sources: {e}")),
    }
    let out = tmp.join("vendor");
    match fs::remove_dir_all(&out) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("clear {}: {e}", out.display())),
    }
    let tool_dir = targo
        .parent()
        .ok_or_else(|| format!("targo has no directory: {}", targo.display()))?;
    let mut path = tool_dir.as_os_str().to_owned();
    path.push(":");
    path.push(SYSTEM_PATH);
    let home = std::env::var_os("HOME").ok_or("vendoring needs HOME")?;
    let result = command(targo)
        .args(["vendor", "--locked", "--versioned-dirs", "--manifest-path"])
        .arg(src.join("Cargo.toml"))
        .arg(&out)
        .current_dir("/")
        .env_clear()
        .env("HOME", home)
        .env("PATH", path)
        .env("CARGO_HOME", &cargo_home)
        .env("TMPDIR", &tmp)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("spawn {} vendor: {e}", targo.display()))?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(12).collect();
        return Err(format!(
            "`targo vendor --locked` failed ({}):\n  {}",
            result.status,
            tail.into_iter().rev().collect::<Vec<_>>().join("\n  ")
        ));
    }
    if !out.is_dir() {
        // Nothing to vendor: cargo writes no directory for an empty set.
        fs::create_dir(&out).map_err(|e| format!("create {}: {e}", out.display()))?;
    }
    check_vendored(&locked, &out)?;
    check_crate_archives(&locked, &cargo_home)?;
    put_tree(sync, &out, Path::new("vendor"))?;
    fs::remove_dir_all(&out).map_err(|e| format!("remove {}: {e}", out.display()))?;
    Ok(locked.len())
}

/// Hand every file under `from` to `sync`, at `prefix`.
fn put_tree(sync: &mut Syncer, from: &Path, prefix: &Path) -> Result<(), String> {
    let mut dirs = vec![(from.to_path_buf(), prefix.to_path_buf())];
    while let Some((dir, rel)) = dirs.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("list {}: {e}", dir.display()))?;
            let path = entry.path();
            let here = rel.join(entry.file_name());
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("inspect {}: {e}", path.display()))?;
            if meta.is_dir() {
                dirs.push((path, here));
            } else if meta.is_file() {
                let data = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
                sync.put(&here, &data, is_exec(&meta))?;
            } else {
                return Err(format!(
                    "vendoring left {} at {}: a release builds from regular files only",
                    if meta.file_type().is_symlink() {
                        "a symbolic link"
                    } else {
                        "a special file"
                    },
                    here.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_exec(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_exec(_meta: &fs::Metadata) -> bool {
    false
}

/// Every file under `root`, by relative path: `file\0<mode>\0<sha256>`.
fn listing(root: &Path) -> Result<Listing, String> {
    let mut out = Listing::new();
    let mut dirs = vec![(root.to_path_buf(), Vec::new())];
    while let Some((dir, rel)) = dirs.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("list {}: {e}", dir.display()))?;
            let path = entry.path();
            let mut here = rel.clone();
            if !here.is_empty() {
                here.push(b'/');
            }
            here.extend_from_slice(&path_bytes(Path::new(&entry.file_name())));
            let meta = fs::symlink_metadata(&path)
                .map_err(|e| format!("inspect {}: {e}", path.display()))?;
            let line = if meta.is_dir() {
                dirs.push((path, here));
                continue;
            } else if meta.file_type().is_symlink() {
                let target = fs::read_link(&path)
                    .map_err(|e| format!("read link {}: {e}", path.display()))?;
                let mut line = b"link\0-\0".to_vec();
                line.extend_from_slice(&path_bytes(&target));
                line
            } else if meta.is_file() {
                let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
                let sha = sha256_hex(&bytes);
                format!("file\0{:o}\0{sha}", file_mode(&meta)).into_bytes()
            } else {
                return Err(format!(
                    "a special file is in the snapshot: {}",
                    path.display()
                ));
            };
            out.insert(here, line);
        }
    }
    Ok(out)
}

#[cfg(unix)]
fn file_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_meta: &fs::Metadata) -> u32 {
    0o644
}

fn sha256_hex(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(bytes))
}

/// [`SCHEMA`], then each `path\0line\n` in path order, under one SHA-256.
fn digest_of(listing: &Listing) -> String {
    let mut hasher = aterm_digest::Sha256::new();
    hasher.update(SCHEMA.as_bytes());
    hasher.update(b"\n");
    for (path, line) in listing {
        hasher.update(path);
        hasher.update(b"\0");
        hasher.update(line);
        hasher.update(b"\n");
    }
    aterm_codec::hex::encode_lower(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git with nothing of this machine's own configuration in it
    /// ([`crate::ledger::testing::git`]).
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = crate::ledger::testing::git()
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// The reader refuses planted bytes under an id (which plain `git cat-file` serves, the
    /// negative control), a wrong kind and a missing object; a refusal closes it.
    #[test]
    fn the_object_reader_holds_every_answer_to_its_id_and_kind() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-objects-")
            .tempdir()
            .unwrap();
        let repo = scratch.path();
        git(repo, &["init", "-q"]);
        fs::write(repo.join("a.txt"), "alpha\n").unwrap();
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "one"]);
        let head = git(repo, &["rev-parse", "HEAD"]);
        let tree = git(repo, &["rev-parse", "HEAD^{tree}"]);
        let other = git(repo, &["commit-tree", "-p", &head, "-m", "planted", &tree]);
        let reader = || ObjectReader::spawn(repo).unwrap();

        let mut good = reader();
        let bytes = good.read(&head, "commit").unwrap();
        assert_eq!(
            commit_tree(&bytes, head.len()).as_deref(),
            Some(tree.as_str())
        );
        let bytes = good.read(&tree, "tree").unwrap();
        let entries = tree_entries(&bytes, head.len() / 2).unwrap();
        let blob = git(repo, &["rev-parse", "HEAD:a.txt"]);
        assert!(
            matches!(&entries[..], [RawEntry { mode: b"100644", name: b"a.txt", id }] if *id == blob)
        );
        good.finish().unwrap();

        let mut kind = reader();
        kind.read(&tree, "commit").unwrap_err();
        let err = kind.read(&head, "commit").unwrap_err();
        assert!(err.contains("closed"), "{err}");
        let err = reader().read(&"0".repeat(head.len()), "tree").unwrap_err();
        assert!(err.contains("missing"), "{err}");

        let loose = |oid: &str| repo.join(".git/objects").join(&oid[..2]).join(&oid[2..]);
        fs::remove_file(loose(&head)).unwrap();
        fs::copy(loose(&other), loose(&head)).unwrap();
        assert!(
            git(repo, &["cat-file", "commit", &head]).contains("planted"),
            "the negative control: git serves the planted bytes under the commit's id"
        );
        reader().read(&head, "commit").unwrap_err();
    }

    /// A path the new commit dropped is gone from `src/` when [`take_tree`] returns, before
    /// [`vendor_into`] reads it (a `members = ["crates/*"]` workspace still takes a leftover
    /// crate as a member).
    #[test]
    fn a_path_the_commit_dropped_is_gone_from_src_before_vendoring_reads_it() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("aterm-take-")
            .tempdir()
            .unwrap();
        let workspace = "[workspace]\nmembers = [\"crates/*\"]\n";
        let kept = "[package]\nname = \"kept\"\n";
        let repo = scratch.path().join("repo");
        fs::create_dir_all(repo.join("crates/kept")).unwrap();
        git(&repo, &["init", "-q"]);
        fs::write(repo.join("Cargo.toml"), workspace).unwrap();
        fs::write(repo.join("crates/kept/Cargo.toml"), kept).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "the release"]);
        let commit = git(&repo, &["rev-parse", "HEAD"]);

        let root = scratch.path().join("snapshot");
        for (rel, data) in [
            ("src/Cargo.toml", workspace),
            ("src/crates/kept/Cargo.toml", kept),
            ("src/crates/gone/Cargo.toml", "[package]\nname = \"gone\"\n"),
            ("vendor/old-1.0.0/src/lib.rs", "pub fn old() {}\n"),
        ] {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, data).unwrap();
        }
        let mtime = |rel: &str| fs::metadata(root.join(rel)).unwrap().modified().unwrap();
        let before = mtime("src/crates/kept/Cargo.toml");
        let listed = |rel: &str| {
            let mut names: Vec<String> = fs::read_dir(root.join(rel))
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        let mut sync = Syncer::new(&root);
        take_tree(&repo, &commit, &mut sync).unwrap();
        assert_eq!(
            listed("src/crates"),
            ["kept"],
            "the tree vendoring resolves is the commit's alone"
        );
        assert_eq!(mtime("src/crates/kept/Cargo.toml"), before);
        assert_eq!(listed("vendor"), ["old-1.0.0"]);

        sync.put(
            Path::new("vendor/new-1.0.0/src/lib.rs"),
            b"pub fn new() {}\n",
            false,
        )
        .unwrap();
        sync.put(
            Path::new(".cargo/config.toml"),
            SEALED_CONFIG.as_bytes(),
            false,
        )
        .unwrap();
        let report = sync.finish().unwrap();
        assert_eq!(listed("vendor"), ["new-1.0.0"]);
        assert_eq!(listed(".cargo"), ["config.toml"]);
        assert_eq!(
            report,
            SyncReport {
                written: 2,
                unchanged: 2,
                removed: 2,
            }
        );
    }
}
