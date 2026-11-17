// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WORKTREE REGISTRATION WHOSE FOLDER IS GONE (DC-13b, `docs/DESIGN-disk-custody-2026-11-12.md`
//! §3.1): git's entry for a linked worktree (`<checkout>/.git/worktrees/<id>`) outlives a folder
//! someone removed by hand, and holds what nothing else may: its HEAD, its index, its reflogs,
//! its own refs and the private repositories of the submodules checked out in it
//! (`…/worktrees/<id>/modules/<path>`). `git worktree prune` drops every such entry at once with
//! all of that ([`super::retired`]'s `remove_snapshot` says why it is never run). [`drop_gone`]
//! drops them one at a time, each only after what it alone holds is on a ref:
//!
//! - Its HEAD, each root ref it holds (`ORIG_HEAD`, `MERGE_HEAD`, …; `FETCH_HEAD` is what a
//!   remote has, `AUTO_MERGE` a merge's scratch tree), each of its own refs (`refs/bisect/…`,
//!   `refs/worktree/…`, …), each object its reflogs name, each commit the files of an operation
//!   under way in it name (`rebase-merge/autostash`, the stash `git rebase --autostash` made of
//!   what was changed and staged, which no ref names; `rebase-apply`, `sequencer`,
//!   `BISECT_LOG`), and its index as a commit over its HEAD where the two differ (its author and
//!   committer `aterm`, dated at its HEAD's commit time, so the same entry read twice makes the
//!   same commit): each of these that `origin/main`, a branch, a tag or a pin under
//!   `refs/orphans/` (other than the entry's own, [`ref_name`]) does not reach is a root, and
//!   each root no other root reaches gets its own pin, `refs/orphans/<name>/<12 hex digits>`, in
//!   the repository. A root that is no commit (an annotated tag, a tree, a blob) gets its own
//!   pin whatever reaches it.
//! - Each commit a private submodule repository of the entry holds (its refs' and its reflogs'
//!   tips, and its index as a commit over its HEAD) that the checkout's canonical module store
//!   (`<checkout>/.git/modules/<path>`) does not reach through a ref other than the entry's own
//!   pins is copied into that store, its objects alone, and pinned there under the same name
//!   ([`root_module_commit`]); a store that cannot take them, a shallow private store, and one
//!   with an operation under way (whose folder the registry's walk refuses) keep the entry.
//!
//! The pins are written only once the caller put them on record ([`Custody::pinning`]), and the
//! entry goes only once the caller recorded what their pushes owe ([`Custody::dropping`]: the
//! daily branch-name step owes them to the repository's origin), the same pins are read again
//! and stand, and the retired gate's private-history check finds every private store rooted in
//! its canonical one. Then the entry, and the stores under it, go through the build cache's
//! remover, after the process fence, and nothing else: no other entry, no other repository's
//! and no object of the repository.
//!
//! An entry stays, with nothing written, while it is locked (`git worktree lock`), while its
//! folder stands, and while that folder is away rather than gone ([`checkout::gone_placed`]: a
//! folder whose parent is gone too is on a volume not mounted now unless the home volume places
//! it; one whose parent stands empty is in a mount point whose volume is not mounted now). An
//! entry that cannot be read or pinned stays too, with why ([`Pass::kept`]).
//!
//! A pin under the entry's own name holds nothing for it, in the repository or a module store:
//! a pass that stopped after its pins and before their pushes were recorded leaves them, and
//! the next pass plans them again, so their pushes are recorded before the entry goes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use super::checkout::{self, Placing};
use super::retired::{Answer, Git, history, read_until};

/// The config every git run that writes a pin is given: no file system monitor or untracked
/// cache answers for a file git did not read itself, no hook runs (a reference-transaction hook
/// would run on each pin), no split index, no submodule recursion, no automatic maintenance and
/// no signing program.
pub const CONFIG: &[&str] = &[
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "submodule.recurse=false",
    "-c",
    "maintenance.auto=false",
    "-c",
    "gc.auto=0",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.splitIndex=false",
    "-c",
    "core.untrackedCache=false",
];

/// The most entries of one checkout's `.git/worktrees` one pass reads.
const MAX_ENTRIES: usize = 4096;

/// The most pins one entry may need; one that needs more stays.
const MAX_PINS: usize = 256;

/// The most bytes of one file of an entry's refs and reflogs read.
const FILE_BYTES: usize = 16 << 20;

/// The most bytes of all of one entry's refs and reflogs read.
const METADATA_BYTES: usize = 64 << 20;

/// The most files and folders of one entry's refs and reflogs read.
const MAX_FILES: usize = 65_536;

/// The most commits one entry's refs and reflogs may name.
const MAX_ROOTS: usize = 65_536;

/// A worktree registration of a checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// Git's entry for it, `<checkout>/.git/worktrees/<id>`.
    pub entry: PathBuf,
    /// The folder its `gitdir` names, where its tree was.
    pub folder: PathBuf,
    /// The commit its HEAD names, `None` where it names none (unborn, or unread).
    pub head: Option<String>,
}

/// One ref a pin writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    /// The repository that holds it: the main checkout, or a canonical module store in its
    /// git directory.
    pub repo: PathBuf,
    /// Its full name, `refs/orphans/<name>/<12 hex digits>`.
    pub name: String,
    /// The commit it names.
    pub oid: String,
}

/// What [`drop_gone`] asks of its caller.
pub trait Custody {
    /// Whether the pass may judge another entry (its time, the claim it runs under).
    fn more(&mut self) -> bool;
    /// Asked before `pins` of `gone` are written: `false` writes none, and `gone` stays.
    fn pinning(&mut self, gone: &Registration, pins: &[Pin]) -> bool;
    /// Asked once `pins` stand, before `gone`'s entry goes (`pins` empty: nothing it holds needed
    /// one): `Err` keeps it, for why.
    ///
    /// # Errors
    /// Why `gone` stays (its pins could not be put on record).
    fn dropping(&mut self, gone: &Registration, pins: &[Pin]) -> Result<(), String>;
}

/// What one pass over a checkout did.
#[derive(Debug, Default)]
pub struct Pass {
    /// Each entry that went, with the pins that hold what it held.
    pub dropped: Vec<(Registration, Vec<Pin>)>,
    /// Each entry whose folder is gone, or that could not be read, that stays, with why.
    pub kept: Vec<(Registration, String)>,
    /// Why the pass stopped before every entry was judged.
    pub unfinished: Option<String>,
}

/// `git <CONFIG> <args>` in `dir`, with `index` as its index when given and `env` set, in the C
/// locale.
fn run(
    git: &dyn Git,
    dir: &Path,
    args: &[&str],
    index: Option<&Path>,
    env: &[(&str, &str)],
) -> Answer {
    let mut all = CONFIG.to_vec();
    all.extend_from_slice(args);
    let mut vars = vec![("LC_ALL", "C")];
    vars.extend_from_slice(env);
    git.run_env(dir, &all, index, &vars)
}

/// What `answer` printed, trimmed, where git exited 0; else why git could not `what`.
fn line(answer: Answer, what: &str) -> Result<String, String> {
    if answer.code == Some(0) {
        Ok(answer.out.trim().to_owned())
    } else {
        Err(format!("git could not {what}: {}", answer.err.trim()))
    }
}

/// `path` as one argument.
fn arg(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("{} is no path git takes as an argument", path.display()))
}

/// The first 12 hex digits of `oid`.
fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

/// Whether `oid` is a full lowercase object name of either of git's formats, not all zeros.
fn oid_shaped(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && !oid.bytes().all(|b| b == b'0')
}

/// The name a worktree's pins carry: its folder's name less `.noindex`, each character git
/// does not take in a ref name's part made `-`.
#[must_use]
pub fn ref_name(wt: &Path) -> String {
    let folder = wt
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let folder = folder.strip_suffix(".noindex").unwrap_or(&folder);
    let mut name: String = folder
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    name.truncate(96);
    let name = name.trim_matches('-');
    if name.is_empty() {
        "worktree".to_owned()
    } else {
        name.to_owned()
    }
}

/// Create `name` at `oid` in the repository `dir` (a `--git-dir` in `prefix` when given) only
/// where no ref stands by that name; one that already names `oid` is the same pin.
///
/// # Errors
/// Why it could not be made: a ref of that name names another commit, or git refused.
pub fn create_ref(
    git: &dyn Git,
    dir: &Path,
    prefix: &[&str],
    name: &str,
    oid: &str,
) -> Result<(), String> {
    let mut args = prefix.to_vec();
    args.extend(["update-ref", "--no-deref", name, oid, ""]);
    if run(git, dir, &args, None, &[]).code == Some(0) {
        return Ok(());
    }
    let mut args = prefix.to_vec();
    args.extend(["rev-parse", "--verify", "--quiet", name]);
    let now = run(git, dir, &args, None, &[]);
    if now.code == Some(0) && now.out.trim() == oid {
        return Ok(());
    }
    Err(format!(
        "its pin {name} could not be made at {}: a ref of that name names another commit",
        short(oid)
    ))
}

/// `text` as one word of a POSIX shell's command line.
fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Copy `root`, a commit the private module store `store` holds, into the canonical module
/// store `witness`, its objects alone (a fetch that writes no ref and no `FETCH_HEAD`), then
/// root it there on `pin` once the witness holds it. A `pin` that names `root` already is the
/// same pin, and nothing is fetched. `checkout` is the main checkout both belong to: the store's
/// own `core.worktree` names a folder in its worktree, which may be gone, and git's local
/// transport drops this run's `-c` and `GIT_WORK_TREE` before it starts the store's
/// `upload-pack`, whose `pack-objects` would change into that folder and die; so `upload-pack`
/// is started with `checkout`, which stands, as its work tree.
///
/// A shallow `store` is refused before anything is fetched: git takes a shallow store's
/// commits into a witness that is not shallow while it refuses their shallow roots, exits 0,
/// and leaves the witness holding a commit whose history it lacks, so a pin there would name
/// history no store holds. Whatever the fetch brought is also walked before the pin: every
/// object `root` reaches that no ref of the witness reaches must be there.
///
/// # Errors
/// Why the witness could not take it: `store` is shallow, the fetch failed, the commit or an
/// object it reaches is not there after it, or a ref named `pin` names another commit.
pub fn root_module_commit(
    git: &dyn Git,
    checkout: &Path,
    witness: &Path,
    store: &Path,
    root: &str,
    pin: &str,
) -> Result<(), String> {
    if !oid_shaped(root) {
        return Err(format!(
            "its submodule store names {root}, no commit git writes"
        ));
    }
    let upload_pack = format!(
        "--upload-pack=git -c uploadpack.allowAnySHA1InWant=true --work-tree={} upload-pack",
        shell_word(arg(checkout)?)
    );
    let git_dir = format!("--git-dir={}", arg(witness)?);
    let had = run(
        git,
        witness,
        &[&git_dir, "rev-parse", "--verify", "--quiet", pin],
        None,
        &[],
    );
    if had.code == Some(0) && had.out.trim() == root {
        return Ok(());
    }
    let shallow = store.join("shallow");
    match std::fs::symlink_metadata(&shallow) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(meta) if meta.is_file() && meta.len() == 0 => {}
        Ok(_) => {
            return Err(format!(
                "its submodule store {} is shallow, so a commit of it would name history \
                 neither store holds",
                store.display()
            ));
        }
        Err(e) => return Err(format!("{} could not be read: {e}", shallow.display())),
    }
    line(
        run(
            git,
            witness,
            &[
                git_dir.as_str(),
                "-c",
                "protocol.version=2",
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.file.allow=always",
                "-c",
                "uploadpack.allowAnySHA1InWant=true",
                "-c",
                "fetch.writeCommitGraph=false",
                "fetch",
                "--quiet",
                "--no-write-fetch-head",
                "--no-tags",
                "--recurse-submodules=no",
                &upload_pack,
                arg(store)?,
                root,
            ],
            None,
            &[],
        ),
        "copy a submodule commit into the checkout's module store",
    )?;
    let commit = format!("{root}^{{commit}}");
    line(
        run(
            git,
            witness,
            &[git_dir.as_str(), "cat-file", "-e", &commit],
            None,
            &[],
        ),
        "find the submodule commit it copied",
    )?;
    let walked = line(
        run(
            git,
            witness,
            &[
                git_dir.as_str(),
                "rev-list",
                "--objects",
                "--no-object-names",
                "--missing=print",
                root,
                "--not",
                "--all",
            ],
            None,
            &[],
        ),
        "walk the submodule commit it copied",
    )?;
    if let Some(missing) = walked.lines().find_map(|row| row.strip_prefix('?')) {
        return Err(format!(
            "the checkout's module store took {} without {missing}, which it reaches",
            short(root)
        ));
    }
    create_ref(git, witness, &[git_dir.as_str()], pin, root)
}

/// `path` with each `.` dropped and each `..` taking the part before it: `None` where a `..`
/// would climb above its root.
fn lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    Some(out)
}

/// One small regular file of git's, read whole without following a link.
fn small(path: &Path) -> Result<Vec<u8>, String> {
    crate::metadata_io::read_bounded_regular(path, 4096)
        .map_err(|e| format!("{} could not be read: {e}", path.display()))
}

/// A path git wrote in one of its files of `entry`: one line, absolute or relative to `entry`
/// (`worktree.useRelativePaths`), made absolute without `.` or `..`.
fn written_path(entry: &Path, name: &str) -> Result<PathBuf, String> {
    let at = entry.join(name);
    let bytes = small(&at)?;
    let text = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|t| t.strip_suffix('\n').or(Some(t)))
        .filter(|t| !t.is_empty() && !t.contains('\n') && !t.contains('\0'))
        .ok_or_else(|| format!("{} is not one path", at.display()))?;
    lexical(&entry.join(text)).ok_or_else(|| format!("{} names no place", at.display()))
}

/// Whether nothing stands at `folder`: `Err` where that could not be read.
fn absent(folder: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(folder) {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(format!("{} could not be read: {e}", folder.display())),
    }
}

/// Whether `folder` is gone, not away: nothing stands there, the rule the build cache judges a
/// gone workspace by calls it gone ([`checkout::gone_placed`]), and the folder that held it,
/// where it stands, holds something. An empty one may be where a volume not mounted now is
/// mounted (`/data`, `~/ext`, outside the folders volumes are mounted in), whose tree is away
/// with it; [`checkout::gone_placed`] holds the nearest standing folder above a folder whose
/// parent is gone too to the same rule ([`checkout::empty_folder`]).
fn gone_folder(folder: &Path, home: Option<&Path>, placing: Option<&Placing>) -> bool {
    let held = folder
        .parent()
        .is_some_and(|parent| match checkout::empty_folder(parent) {
            Ok(empty) => !empty,
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        });
    absent(folder).unwrap_or(false) && checkout::gone_placed(folder, home, placing).is_ok() && held
}

/// The folders of a git directory that hold an operation under way (a rebase, `git am`, a
/// cherry-pick or revert): their files name commits no ref may, `rebase-merge/autostash` (the
/// stash `--autostash` made of what was changed and staged) above all.
const UNDER_WAY: &[&str] = &["rebase-merge", "rebase-apply", "sequencer"];

/// What an entry's own files name: each object its root refs, its own refs and its reflogs
/// name, read from its files (bounded), `MERGE_HEAD`'s records included and `FETCH_HEAD` (what
/// a remote has) and `AUTO_MERGE` (a merge's scratch tree) left out. A symbolic ref names a ref
/// read in its own right, or a ref of the repository that holds its commit itself, so it adds
/// nothing. Beside them, `noted`: each full object name the files of an operation under way
/// ([`UNDER_WAY`]) and `BISECT_LOG` hold, which may name an object the repository does not hold
/// (a message's words), so only those that name a commit there are roots ([`kinds`]).
struct Named {
    roots: BTreeSet<String>,
    noted: BTreeSet<String>,
    bytes: usize,
    files: usize,
}

impl Named {
    fn new() -> Self {
        Self {
            roots: BTreeSet::new(),
            noted: BTreeSet::new(),
            bytes: 0,
            files: 0,
        }
    }

    /// Each full object name in the file `at`: a run of lowercase hex digits of either of git's
    /// lengths that no other hex digit adjoins.
    fn tokens(&mut self, at: &Path) -> Result<(), String> {
        let bytes = self.read(at)?;
        let hex = |b: &u8| b.is_ascii_digit() || (b'a'..=b'f').contains(b);
        for run in bytes.split(|b| !hex(b)) {
            if let Ok(oid) = std::str::from_utf8(run)
                && oid_shaped(oid)
            {
                self.noted.insert(oid.to_owned());
                if self.noted.len() > MAX_ROOTS {
                    return Err(format!(
                        "its operations under way name more than {MAX_ROOTS} objects"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The files of each operation under way in the git directory `dir` ([`UNDER_WAY`]), and
    /// its `BISECT_LOG`, through [`Self::tokens`].
    fn under_way(&mut self, dir: &Path) -> Result<(), String> {
        for name in UNDER_WAY {
            self.walk(&dir.join(name), 0, Self::tokens)?;
        }
        let log = dir.join("BISECT_LOG");
        if !absent(&log)? {
            self.tokens(&log)?;
        }
        Ok(())
    }

    fn add(&mut self, oid: &str, at: &Path) -> Result<(), String> {
        if !oid_shaped(oid) {
            return Err(format!(
                "{} names {oid}, which is no object name git writes",
                at.display()
            ));
        }
        self.roots.insert(oid.to_owned());
        if self.roots.len() > MAX_ROOTS {
            return Err(format!(
                "its refs and reflogs name more than {MAX_ROOTS} objects"
            ));
        }
        Ok(())
    }

    fn read(&mut self, at: &Path) -> Result<Vec<u8>, String> {
        self.files += 1;
        if self.files > MAX_FILES {
            return Err(format!(
                "its refs and reflogs are more than {MAX_FILES} files"
            ));
        }
        let bytes = crate::metadata_io::read_bounded_regular(at, FILE_BYTES)
            .map_err(|e| format!("{} could not be read: {e}", at.display()))?;
        self.bytes = self.bytes.saturating_add(bytes.len());
        if self.bytes > METADATA_BYTES {
            return Err(format!(
                "its refs and reflogs are more than {} MiB",
                METADATA_BYTES >> 20
            ));
        }
        Ok(bytes)
    }

    /// A ref file's one value: an object name, or a symbolic ref (`None`).
    fn value(&mut self, at: &Path) -> Result<(), String> {
        let bytes = self.read(at)?;
        let text = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| t.strip_suffix('\n'))
            .ok_or_else(|| format!("{} is not one ref", at.display()))?;
        if text.starts_with("ref: ") {
            return Ok(());
        }
        self.add(text, at)
    }

    /// `MERGE_HEAD`: one object name a line.
    fn records(&mut self, at: &Path) -> Result<(), String> {
        let bytes = self.read(at)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| format!("{} is not text", at.display()))?;
        for row in text.lines() {
            self.add(row.trim(), at)?;
        }
        Ok(())
    }

    /// A reflog: `<old> <new> <who> <when>\t<message>` a line, both object names held (an
    /// all-zero one names nothing).
    fn reflog(&mut self, at: &Path) -> Result<(), String> {
        let bytes = self.read(at)?;
        if (!bytes.is_empty() && !bytes.ends_with(b"\n")) || bytes.contains(&0) {
            return Err(format!("{} has an incomplete record", at.display()));
        }
        let text =
            std::str::from_utf8(&bytes).map_err(|_| format!("{} is not text", at.display()))?;
        for row in text.lines() {
            let mut fields = row.splitn(3, ' ');
            let (Some(old), Some(new), Some(rest)) = (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("{} has a malformed record", at.display()));
            };
            if rest.is_empty() {
                return Err(format!("{} has a malformed record", at.display()));
            }
            for oid in [old, new] {
                if !oid.bytes().all(|b| b == b'0') {
                    self.add(oid, at)?;
                }
            }
        }
        Ok(())
    }

    /// Every file under `dir`, through `each`; a link or anything but a file or folder keeps
    /// the entry.
    fn walk(
        &mut self,
        dir: &Path,
        depth: usize,
        each: fn(&mut Self, &Path) -> Result<(), String>,
    ) -> Result<(), String> {
        if depth > 64 {
            return Err(format!("{} is nested too deep", dir.display()));
        }
        let listing = match std::fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{} could not be read: {e}", dir.display())),
        };
        for item in listing {
            let item = item.map_err(|e| format!("{} could not be read: {e}", dir.display()))?;
            let at = item.path();
            let meta = std::fs::symlink_metadata(&at)
                .map_err(|e| format!("{} could not be read: {e}", at.display()))?;
            if meta.is_dir() {
                self.files += 1;
                self.walk(&at, depth + 1, each)?;
            } else if meta.is_file() {
                each(self, &at)?;
            } else {
                return Err(format!(
                    "{} is a link or no regular file, so its entry stays",
                    at.display()
                ));
            }
        }
        Ok(())
    }

    fn of(entry: &Path) -> Result<Self, String> {
        let mut named = Self::new();
        if !absent(&entry.join("reftable"))? {
            return Err("its refs are kept in a reftable, which this pass does not read".into());
        }
        let listing = std::fs::read_dir(entry)
            .map_err(|e| format!("{} could not be read: {e}", entry.display()))?;
        for item in listing {
            let item = item.map_err(|e| format!("{} could not be read: {e}", entry.display()))?;
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            // HEAD is read by git, which follows it to its branch ([`head_of`]).
            if matches!(name, "HEAD" | "FETCH_HEAD" | "AUTO_MERGE") {
                continue;
            }
            let at = item.path();
            if name == "MERGE_HEAD" {
                named.records(&at)?;
            } else if history::root_ref(name) {
                let meta = std::fs::symlink_metadata(&at)
                    .map_err(|e| format!("{} could not be read: {e}", at.display()))?;
                if !meta.is_file() {
                    return Err(format!("{} is no regular file", at.display()));
                }
                named.value(&at)?;
            }
        }
        named.walk(&entry.join("refs"), 0, Self::value)?;
        named.walk(&entry.join("logs"), 0, Self::reflog)?;
        named.under_way(entry)?;
        Ok(named)
    }
}

/// The commit HEAD names in the git directory `prefix` selects (none: the checkout's), through
/// git, which follows a symbolic HEAD to its branch: `None` where it names none (an unborn
/// branch).
fn head_of(git: &dyn Git, checkout: &Path, prefix: &[&str]) -> Result<Option<String>, String> {
    let mut args = prefix.to_vec();
    args.extend(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
    let head = run(git, checkout, &args, None, &[]);
    match head.code {
        Some(0) if oid_shaped(head.out.trim()) => Ok(Some(head.out.trim().to_owned())),
        Some(1) if head.out.trim().is_empty() => {
            let mut args = prefix.to_vec();
            args.extend(["symbolic-ref", "--quiet", "HEAD"]);
            if run(git, checkout, &args, None, &[]).code == Some(0) {
                Ok(None)
            } else {
                Err("its HEAD names no commit and no branch".to_owned())
            }
        }
        _ => Err(format!("git could not read its HEAD: {}", head.err.trim())),
    }
}

/// The commit of what the index `own` staged, over `head`, in the repository `prefix` selects,
/// where its tree differs from `head`'s; read through a copy of the index in the system's
/// temporary folder, so neither the index nor git's entry is written. Its author and committer
/// are `aterm`, dated at `head`'s commit time, so the same index read twice makes the same
/// commit; its message names `of`, where the tree was. `None`: no index, or the same tree as
/// its HEAD.
fn index_commit(
    git: &dyn Git,
    checkout: &Path,
    prefix: &[&str],
    own: &Path,
    head: Option<&str>,
    of: &Path,
) -> Result<Option<String>, String> {
    if absent(own)? {
        return Ok(None);
    }
    let folder = aterm_tempfile::Builder::new()
        .prefix("aterm-gone-index-")
        .tempdir()
        .map_err(|e| format!("a private index folder could not be made: {e}"))?;
    let index = folder.path().join("index");
    crate::metadata_io::copy_regular_file(own, &index)
        .map_err(|e| format!("{} could not be copied: {e}", own.display()))?;
    let tree = line(
        run(
            git,
            checkout,
            &with(prefix, &["write-tree"]),
            Some(&index),
            &[],
        ),
        "write its index as a tree (a path is unmerged, or an object it names is missing)",
    )?;
    let head_tree = match head {
        Some(head) => Some(line(
            run(
                git,
                checkout,
                &with(
                    prefix,
                    &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
                ),
                None,
                &[],
            ),
            "read its HEAD's tree",
        )?),
        None => None,
    };
    if head_tree.as_deref() == Some(tree.as_str()) {
        return Ok(None);
    }
    let at = match head {
        Some(head) => line(
            run(
                git,
                checkout,
                &with(prefix, &["log", "-1", "--format=%ct", head]),
                None,
                &[],
            ),
            "date its HEAD",
        )?,
        None => "0".to_owned(),
    };
    let date = format!("@{at} +0000");
    let who = [
        ("GIT_AUTHOR_NAME", "aterm"),
        ("GIT_AUTHOR_EMAIL", "aterm@localhost.invalid"),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", "aterm"),
        ("GIT_COMMITTER_EMAIL", "aterm@localhost.invalid"),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let message = format!(
        "aterm: the index of {} at {}\n\nIts folder was gone; the disk watch kept what its \
         index staged before git's entry for it went (DC-13b).",
        of.display(),
        head.map_or("no commit", short)
    );
    let mut args = with(prefix, &["commit-tree", "--no-gpg-sign", tree.as_str()]);
    if let Some(head) = head {
        args.extend(["-p", head]);
    }
    args.extend(["-m", message.as_str()]);
    line(
        run(git, checkout, &args, None, &who),
        "write its index as a commit",
    )
    .map(Some)
}

/// `prefix` and then `args`, as one git command line.
fn with<'a>(prefix: &[&'a str], args: &[&'a str]) -> Vec<&'a str> {
    let mut all = prefix.to_vec();
    all.extend_from_slice(args);
    all
}

/// What kind of object each of `oids` is in the repository `prefix` selects (none: the
/// checkout's): `commit`, `tag`, `tree` or `blob`, `None` where it holds no such object.
fn kinds<'a>(
    git: &dyn Git,
    checkout: &Path,
    prefix: &[&str],
    oids: impl Iterator<Item = &'a String>,
) -> Result<BTreeMap<String, Option<String>>, String> {
    let input: String = oids.map(|oid| format!("{oid}\n")).collect();
    if input.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut args = prefix.to_vec();
    args.extend(["cat-file", "--batch-check=%(objectname) %(objecttype)"]);
    let listed = git.read_history(checkout, &args, input.as_bytes(), None, read_until(git));
    if listed.code != Some(0) {
        return Err(format!(
            "git could not say what its objects are: {}",
            listed.err.trim()
        ));
    }
    let mut out = BTreeMap::new();
    for row in listed.out.lines() {
        let (oid, kind) = row
            .split_once(' ')
            .ok_or_else(|| format!("git said {row} of an object"))?;
        out.insert(oid.to_owned(), (kind != "missing").then(|| kind.to_owned()));
    }
    Ok(out)
}

/// Whether `kinds` calls `oid` a commit.
fn commit(kinds: &BTreeMap<String, Option<String>>, oid: &str) -> bool {
    kinds.get(oid).and_then(Option::as_deref) == Some("commit")
}

/// The root of the private module store `store` of `reg` its refs and reflogs do not name
/// ([`history::unrooted`] reads those): its index as a commit over its HEAD where the two
/// differ, written into the store. An operation under way in it needs nothing here: the
/// registry's walk refuses the folder it keeps (`rebase-merge/`, …) as metadata it does not
/// know, so such a store keeps its entry.
fn store_extra(
    git: &dyn Git,
    checkout: &Path,
    reg: &Registration,
    store: &Path,
) -> Result<Vec<String>, String> {
    let git_dir = format!("--git-dir={}", arg(store)?);
    let work_tree = format!("--work-tree={}", arg(checkout)?);
    let prefix = [git_dir.as_str(), work_tree.as_str()];
    let head = head_of(git, checkout, &prefix)?;
    let of = reg.folder.join(
        store
            .strip_prefix(reg.entry.join("modules"))
            .unwrap_or(store),
    );
    Ok(index_commit(
        git,
        checkout,
        &prefix,
        &store.join("index"),
        head.as_deref(),
        &of,
    )?
    .into_iter()
    .collect())
}

/// The repository's commits `roots` reach that no lasting name holds (`origin/main` where it
/// stands, a branch, a tag, a pin under `refs/orphans/` other than `own`'s), reduced to those
/// no other of them reaches: the commits that need a pin.
fn unheld_tips(
    git: &dyn Git,
    checkout: &Path,
    roots: &BTreeSet<String>,
    own: &str,
) -> Result<Vec<String>, String> {
    if roots.is_empty() {
        return Ok(Vec::new());
    }
    let main = run(
        git,
        checkout,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "refs/remotes/origin/main^{commit}",
        ],
        None,
        &[],
    );
    let exclude = format!("--exclude=refs/orphans/{own}/*");
    let mut args = vec![
        "rev-list",
        "--parents",
        "--not",
        exclude.as_str(),
        "--glob=refs/orphans/*",
        "--branches",
        "--tags",
    ];
    if main.code == Some(0) {
        args.push("refs/remotes/origin/main");
    }
    args.extend(["--not", "--stdin"]);
    let input: String = roots.iter().map(|r| format!("{r}\n")).collect();
    let listed = git.read_history(checkout, &args, input.as_bytes(), None, read_until(git));
    if listed.code != Some(0) {
        return Err(format!(
            "git could not say which of its commits no lasting name holds: {}",
            listed.err.trim()
        ));
    }
    let mut lacking = BTreeSet::new();
    let mut parents = BTreeSet::new();
    for row in listed.out.lines() {
        let mut oids = row.split(' ');
        if let Some(commit) = oids.next() {
            lacking.insert(commit);
        }
        parents.extend(oids);
    }
    Ok(roots
        .iter()
        .filter(|r| lacking.contains(r.as_str()) && !parents.contains(r.as_str()))
        .cloned()
        .collect())
}

/// The commits of `roots`, held in the private module store `store`, that no other of them
/// reaches there: fetching those into the canonical store brings the rest.
fn store_tips(
    git: &dyn Git,
    checkout: &Path,
    store: &Path,
    roots: &[String],
) -> Result<Vec<String>, String> {
    let git_dir = format!("--git-dir={}", arg(store)?);
    let work_tree = format!("--work-tree={}", arg(checkout)?);
    let input: String = roots.iter().map(|r| format!("{r}^@\n")).collect();
    let listed = git.read_history(
        checkout,
        &[&git_dir, &work_tree, "rev-list", "--stdin"],
        input.as_bytes(),
        None,
        read_until(git),
    );
    if listed.code != Some(0) {
        return Err(format!(
            "git could not read its submodule store {}: {}",
            store.display(),
            listed.err.trim()
        ));
    }
    let below: BTreeSet<&str> = listed.out.lines().map(str::trim).collect();
    Ok(roots
        .iter()
        .filter(|r| !below.contains(r.as_str()))
        .cloned()
        .collect())
}

/// The pins `reg` needs: one in the repository for each object it alone holds (a commit no
/// other of them reaches, or any root that is no commit: an annotated tag, a tree or a blob,
/// which `rev-list` would peel or pass over), and one in a canonical module store for each
/// commit a private module store of it holds that store lacks. Each submodule pin's store is
/// returned beside it, for its fetch.
fn plan(
    git: &dyn Git,
    checkout: &Path,
    reg: &Registration,
) -> Result<Vec<(Pin, Option<PathBuf>)>, String> {
    let own = ref_name(&reg.folder);
    let mut named = Named::of(&reg.entry)?;
    if let Some(head) = &reg.head {
        named.roots.insert(head.clone());
    }
    if let Some(index) = index_commit(
        git,
        checkout,
        &[],
        &reg.entry.join("index"),
        reg.head.as_deref(),
        &reg.folder,
    )? {
        named.roots.insert(index);
    }
    let kinds = kinds(git, checkout, &[], named.roots.iter().chain(&named.noted))?;
    let mut commits = BTreeSet::new();
    let mut others = Vec::new();
    for root in &named.roots {
        match kinds.get(root).and_then(Option::as_deref) {
            Some("commit") => {
                commits.insert(root.clone());
            }
            Some(_) => others.push(root.clone()),
            None => return Err(format!("it names {root}, which the repository lacks")),
        }
    }
    commits.extend(
        named
            .noted
            .iter()
            .filter(|oid| commit(&kinds, oid))
            .cloned(),
    );
    let pin = |oid: String| Pin {
        repo: checkout.to_path_buf(),
        name: format!("refs/orphans/{own}/{}", short(&oid)),
        oid,
    };
    let mut pins: Vec<(Pin, Option<PathBuf>)> = unheld_tips(git, checkout, &commits, &own)?
        .into_iter()
        .chain(others)
        .map(|oid| (pin(oid), None))
        .collect();
    let own_pins = format!("refs/orphans/{own}/");
    let mut extra = |store: &Path| store_extra(git, checkout, reg, store);
    for unrooted in history::unrooted(
        git,
        checkout,
        &reg.folder,
        &reg.entry,
        Some(&own_pins),
        &mut extra,
    )? {
        for oid in store_tips(git, checkout, &unrooted.store, &unrooted.roots)? {
            pins.push((
                Pin {
                    repo: unrooted.witness.clone(),
                    name: format!("refs/orphans/{own}/{}", short(&oid)),
                    oid,
                },
                Some(unrooted.store.clone()),
            ));
        }
    }
    if pins.len() > MAX_PINS {
        return Err(format!(
            "it holds {} objects nothing else does, more than one pass pins ({MAX_PINS})",
            pins.len()
        ));
    }
    Ok(pins)
}

/// Read `entry`, one entry of `common`'s `worktrees`: its registration, `None` where its folder
/// stands, is away or is locked (it stays, and nothing is said of it).
fn registration(
    git: &dyn Git,
    checkout: &Path,
    common: &Path,
    entry: &Path,
    home: Option<&Path>,
    placing: Option<&Placing>,
) -> Result<Option<Registration>, (Registration, String)> {
    let unread = |why: String| {
        (
            Registration {
                entry: entry.to_path_buf(),
                folder: PathBuf::new(),
                head: None,
            },
            why,
        )
    };
    let gitfile = written_path(entry, "gitdir").map_err(unread)?;
    let folder = match (gitfile.parent(), gitfile.file_name()) {
        (Some(folder), Some(name)) if name == ".git" => folder.to_path_buf(),
        _ => {
            return Err(unread(format!(
                "its gitdir names {}, which is no worktree's .git",
                gitfile.display()
            )));
        }
    };
    if !gone_folder(&folder, home, placing) {
        return Ok(None);
    }
    if !absent(&entry.join("locked")).map_err(unread)? {
        return Ok(None);
    }
    let mut reg = Registration {
        entry: entry.to_path_buf(),
        folder,
        head: None,
    };
    let names = written_path(entry, "commondir").and_then(|named| {
        let named = named
            .canonicalize()
            .map_err(|e| format!("{} could not be resolved: {e}", named.display()))?;
        if named == common {
            Ok(())
        } else {
            Err(format!(
                "its commondir names {}, not {}",
                named.display(),
                common.display()
            ))
        }
    });
    if let Err(why) = names {
        return Err((reg, why));
    }
    let git_dir = match arg(entry) {
        Ok(at) => format!("--git-dir={at}"),
        Err(why) => return Err((reg, why)),
    };
    match head_of(git, checkout, &[&git_dir]) {
        Ok(head) => reg.head = head,
        Err(why) => return Err((reg, why)),
    }
    Ok(Some(reg))
}

/// Pin what `reg`, whose folder is gone, alone holds, and drop its entry: the module doc's
/// order, each step read again before the next.
fn settle(
    git: &dyn Git,
    checkout: &Path,
    reg: &Registration,
    home: Option<&Path>,
    placing: Option<&Placing>,
    custody: &mut dyn Custody,
    fence: &super::fence::Fence<'_>,
) -> Result<Vec<Pin>, String> {
    let gitdir = small(&reg.entry.join("gitdir"))?;
    let planned = plan(git, checkout, reg)?;
    let pins: Vec<Pin> = planned.iter().map(|(pin, _)| pin.clone()).collect();
    if !pins.is_empty() && !custody.pinning(reg, &pins) {
        return Err("the ledger took no row of its pins, so nothing was pinned".to_owned());
    }
    for (pin, store) in &planned {
        match store {
            None => create_ref(git, checkout, &[], &pin.name, &pin.oid)?,
            Some(store) => {
                root_module_commit(git, checkout, &pin.repo, store, &pin.oid, &pin.name)?;
            }
        }
    }
    custody.dropping(reg, &pins)?;
    // The pins stand, and nothing it holds moved since they were planned.
    let again: Vec<Pin> = plan(git, checkout, reg)?
        .into_iter()
        .map(|(pin, _)| pin)
        .filter(|pin| pin.repo == checkout)
        .collect();
    let main: Vec<&Pin> = pins.iter().filter(|pin| pin.repo == checkout).collect();
    if again.iter().collect::<Vec<_>>() != main {
        return Err("what it holds changed while it was pinned".to_owned());
    }
    for pin in &pins {
        let mut args = Vec::new();
        let git_dir;
        if pin.repo != checkout {
            git_dir = format!("--git-dir={}", arg(&pin.repo)?);
            args.push(git_dir.as_str());
        }
        args.extend(["rev-parse", "--verify", "--quiet", &pin.name]);
        let read = run(git, &pin.repo, &args, None, &[]);
        if read.code != Some(0) || read.out.trim() != pin.oid {
            return Err(format!("its pin {} does not stand", pin.name));
        }
    }
    history::clear(git, checkout, &reg.folder, Some(&reg.entry))?;
    // Last, as near the removal as it can be: still gone, still unlocked, still git's entry for
    // that folder, and no process in either.
    if !gone_folder(&reg.folder, home, placing) {
        return Err(format!(
            "{} is no longer gone, so its entry stays",
            reg.folder.display()
        ));
    }
    if !absent(&reg.entry.join("locked"))? {
        return Err("it was locked while it was pinned".to_owned());
    }
    if small(&reg.entry.join("gitdir"))? != gitdir {
        return Err("git's entry was rewritten while it was pinned".to_owned());
    }
    if !fence.clear_all(&reg.entry, &[&reg.entry, &reg.folder]) {
        return Err("active process use could not be ruled out".to_owned());
    }
    let left = super::remove::tree(&reg.entry);
    if left.whole() {
        Ok(pins)
    } else {
        Err(super::remove::stayed_words(&reg.entry, left.stayed))
    }
}

/// Drop each registration of `checkout` (a main checkout) whose folder is gone, one at a time,
/// once what it alone holds is pinned (the module doc), `home` and `placing` saying where a
/// folder is gone rather than away ([`checkout::gone_placed`]).
///
/// # Errors
/// Why none of `checkout`'s registrations could be read.
pub fn drop_gone(
    git: &dyn Git,
    checkout: &Path,
    home: Option<&Path>,
    placing: Option<&Placing>,
    custody: &mut dyn Custody,
) -> Result<Pass, String> {
    drop_gone_on(
        git,
        checkout,
        home,
        placing,
        custody,
        &super::fence::Fence::default(),
    )
}

/// [`drop_gone`] with a process probe a headless fixture supplies.
///
/// # Errors
/// As [`drop_gone`].
#[cfg(any(test, debug_assertions))]
pub fn drop_gone_with_processes(
    git: &dyn Git,
    checkout: &Path,
    home: Option<&Path>,
    placing: Option<&Placing>,
    custody: &mut dyn Custody,
    processes: super::fence::Processes<'_>,
) -> Result<Pass, String> {
    drop_gone_on(
        git,
        checkout,
        home,
        placing,
        custody,
        &super::fence::Fence::new(processes),
    )
}

fn drop_gone_on(
    git: &dyn Git,
    checkout: &Path,
    home: Option<&Path>,
    placing: Option<&Placing>,
    custody: &mut dyn Custody,
    fence: &super::fence::Fence<'_>,
) -> Result<Pass, String> {
    let dot_git = checkout.join(".git");
    if !std::fs::symlink_metadata(&dot_git).is_ok_and(|m| m.is_dir()) {
        return Err(format!("{} is not a main checkout", checkout.display()));
    }
    let common = dot_git
        .canonicalize()
        .map_err(|e| format!("{} could not be resolved: {e}", dot_git.display()))?;
    let registry = common.join("worktrees");
    let listing = match std::fs::read_dir(&registry) {
        Ok(listing) => listing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pass::default()),
        Err(e) => return Err(format!("{} could not be read: {e}", registry.display())),
    };
    let mut entries = Vec::new();
    for item in listing {
        let item = item.map_err(|e| format!("{} could not be read: {e}", registry.display()))?;
        if item.file_type().is_ok_and(|t| t.is_dir()) {
            entries.push(item.path());
        }
        if entries.len() >= MAX_ENTRIES {
            break;
        }
    }
    entries.sort();
    let mut pass = Pass::default();
    for entry in entries {
        if !custody.more() {
            pass.unfinished = Some("the pass stopped before every entry was judged".to_owned());
            break;
        }
        let reg = match registration(git, checkout, &common, &entry, home, placing) {
            Ok(Some(reg)) => reg,
            Ok(None) => continue,
            Err((reg, why)) => {
                pass.kept.push((reg, why));
                continue;
            }
        };
        match settle(git, checkout, &reg, home, placing, custody, fence) {
            Ok(pins) => pass.dropped.push((reg, pins)),
            Err(why) => pass.kept.push((reg, why)),
        }
    }
    Ok(pass)
}

#[cfg(all(test, unix))]
#[path = "build_cache_gone_entries_tests.rs"]
mod tests;
