// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Selection-local A19 records for the release's early source judgment.
//! These never answer a whole-workspace qualify question.

use std::path::{Path, PathBuf};

use crate::ledger::GitRunner;
use aterm_uds::spawnfd::Command;

/// The identity namespace of a selected read policy.
#[derive(Clone)]
enum EnginePolicySource {
    Repository(agh::Slug),
    #[cfg(test)]
    LocalFixture,
}

/// One policy selected for the three release-input readers. Its object location is
/// transport; its repository or explicit fixture identity and commit are semantic inputs.
#[derive(Clone)]
pub struct EngineInput {
    operational: PathBuf,
    source: EnginePolicySource,
    git_dir: PathBuf,
    commit: String,
}

impl EngineInput {
    pub(crate) fn selected(
        operational: &Path,
        authority: &agh::Slug,
        git_dir: PathBuf,
        commit: String,
    ) -> Result<Self, String> {
        Self::new(
            operational,
            EnginePolicySource::Repository(authority.clone()),
            git_dir,
            commit,
        )
    }

    /// A measurement's explicit local read policy, never an authenticated repository identity.
    /// Its key namespace cannot match a production repository policy's key.
    #[cfg(test)]
    pub(crate) fn local_fixture(operational: &Path) -> Result<Self, String> {
        let mut command = crate::ledger::object_git(operational).map_err(|e| e.to_string())?;
        command.args(["rev-parse", "--absolute-git-dir", "HEAD^{commit}"]);
        let output = aterm_verify::engine::bounded(&mut command).map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        let text = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        let lines: Vec<_> = text.lines().collect();
        if lines.len() != 2 {
            return Err("local policy fixture did not name one Git directory and commit".into());
        }
        Self::new(
            operational,
            EnginePolicySource::LocalFixture,
            PathBuf::from(lines[0]),
            lines[1].to_owned(),
        )
    }

    fn new(
        operational: &Path,
        source: EnginePolicySource,
        git_dir: PathBuf,
        commit: String,
    ) -> Result<Self, String> {
        if !operational.is_absolute() || !git_dir.is_absolute() {
            return Err("release-input engine requires absolute locations".into());
        }
        let selected = Self {
            operational: operational.to_owned(),
            source,
            git_dir,
            commit,
        };
        selected.validate()?;
        Ok(selected)
    }

    fn validate(&self) -> Result<(), String> {
        let named = aterm_verify::engine::Named::Pinned {
            git_dir: self.git_dir.clone(),
            commit: self.commit.clone(),
        };
        aterm_verify::engine::Commit::read(&named, &aterm_verify::engine::bounded, &|_, _| {
            Err("a selected release-input engine never imports another policy".into())
        })
        .map(|_| ())
        .map_err(|error| error.why)
    }

    pub(crate) fn apply(&self, command: &mut Command) {
        command
            .env("PUBLICATION_ENGINE", &self.operational)
            .env(aterm_verify::engine::GIT_DIR_ENV, &self.git_dir)
            .env(aterm_verify::engine::COMMIT_ENV, &self.commit);
    }

    fn environment(
        &self,
        command: &Command,
    ) -> Result<std::collections::BTreeMap<Vec<u8>, Vec<u8>>, String> {
        self.validate()?;
        let mut environment = crate::check_content::effective_environment(command);
        let encoded = crate::check_content::os_bytes;
        for (name, expected) in [
            ("PUBLICATION_ENGINE", self.operational.as_os_str()),
            (aterm_verify::engine::GIT_DIR_ENV, self.git_dir.as_os_str()),
            (
                aterm_verify::engine::COMMIT_ENV,
                std::ffi::OsStr::new(&self.commit),
            ),
        ] {
            let key = encoded(std::ffi::OsStr::new(name));
            if environment.get(&key) != Some(&encoded(expected)) {
                return Err(format!(
                    "release-input child does not name its selected engine ({name})"
                ));
            }
        }
        // Only these two validated transport locators differ between a retained snapshot
        // and its owned copy. Commit and authority stay bound, as does every other variable.
        for name in ["PUBLICATION_ENGINE", aterm_verify::engine::GIT_DIR_ENV] {
            environment.remove(&encoded(std::ffi::OsStr::new(name)));
        }
        Ok(environment)
    }

    fn bind(&self, part: &mut impl FnMut(&[u8])) {
        match &self.source {
            EnginePolicySource::Repository(repository) => {
                part(b"repository");
                part(repository.to_string().as_bytes());
            }
            #[cfg(test)]
            EnginePolicySource::LocalFixture => part(b"local-fixture"),
        }
        part(self.commit.as_bytes());
    }
}

/// One stable input checkout for dry and real runs, quiet for fseventsd and Spotlight.
pub(crate) fn directory(release: &Path) -> PathBuf {
    release
        .join(aterm_tempfile::QUIET)
        .join("input-checkout.noindex")
}

const INPUT_SUBMODULE: &str = "vendor/astream";

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckoutPending {
    format: u8,
    repository: PathBuf,
    root: PathBuf,
    before: Option<String>,
    target: String,
    child: Option<ChildPending>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildPending {
    before: Option<String>,
    target: String,
    gitdir: Option<PathBuf>,
    // Bind the configured origin without putting a credential-bearing URL in the receipt.
    origin: Option<String>,
}

fn pending_path(root: &Path) -> PathBuf {
    root.with_file_name("input-checkout-pending.json")
}

fn git_read(git: &dyn GitRunner, args: &[&str]) -> Result<String, String> {
    let out = git.git_read(args).map_err(|e| e.to_string())?;
    if out.success() {
        String::from_utf8(out.stdout).map_err(|e| e.to_string())
    } else {
        Err(out.stderr_utf8())
    }
}

fn git_run(git: &dyn GitRunner, args: &[&str]) -> Result<(), String> {
    let out = git.git(args).map_err(|e| e.to_string())?;
    if out.success() {
        Ok(())
    } else {
        Err(out.stderr_utf8())
    }
}

fn object_id(text: &str) -> bool {
    aterm_codec::hex::is_lower(text, 40) || aterm_codec::hex::is_lower(text, 64)
}

fn git_path(git: &dyn GitRunner, args: &[&str]) -> Result<PathBuf, String> {
    Ok(PathBuf::from(git_read(git, args)?.trim_end_matches('\n')))
}

fn gitlink(git: &dyn GitRunner, tip: &str) -> Result<Option<String>, String> {
    let entry = git_read(git, &["ls-tree", "-z", tip, "--", INPUT_SUBMODULE])?;
    if entry.is_empty() {
        return Ok(None);
    }
    let Some((header, path)) = entry.strip_suffix('\0').and_then(|s| s.split_once('\t')) else {
        return Err("input submodule entry is malformed".into());
    };
    let fields: Vec<_> = header.split(' ').collect();
    if path != INPUT_SUBMODULE || fields.len() != 3 || !object_id(fields[2]) {
        return Err("input submodule entry is malformed".into());
    }
    if fields[0] == "160000" && fields[1] == "commit" {
        Ok(Some(fields[2].into()))
    } else if fields[1] == "tree" || fields[1] == "blob" {
        Ok(None)
    } else {
        Err("input submodule is not an exact gitlink".into())
    }
}

fn owned_head(git: &dyn GitRunner, root: &Path) -> Result<Option<String>, String> {
    let meta = match root.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("input checkout is not a regular directory".into());
    }
    let physical = root.canonicalize().map_err(|e| e.to_string())?;
    let listed = git_read(git, &["worktree", "list", "--porcelain", "-z"])?;
    if !listed
        .split('\0')
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|path| {
            Path::new(path)
                .canonicalize()
                .is_ok_and(|path| path == physical)
        })
    {
        return Err("existing input checkout is not owned by this repository".into());
    }
    let head = git_read(&crate::ledger::GitCli::new(root), &["rev-parse", "HEAD"])?;
    let head = head.trim();
    if !object_id(head) {
        return Err("input checkout has no exact commit".into());
    }
    Ok(Some(head.into()))
}

fn parent_clean(git: &dyn GitRunner) -> Result<(), String> {
    // The entire index must match HEAD, including the gitlink. Only the child's worktree
    // HEAD may temporarily differ, and its receipt is checked separately below.
    git_run(
        git,
        &[
            "diff-index",
            "--cached",
            "--quiet",
            "--ignore-submodules=none",
            "HEAD",
            "--",
        ],
    )
    .map_err(|_| "input checkout has tracked edits; no work was discarded".to_owned())?;
    let args: &[&str] = if gitlink(git, "HEAD")?.is_some() {
        &[
            "diff-files",
            "--quiet",
            "--ignore-submodules=none",
            "--",
            ".",
            ":(exclude)vendor/astream",
        ]
    } else {
        &["diff-files", "--quiet", "--ignore-submodules=none", "--"]
    };
    git_run(git, args).map_err(|_| "input checkout has tracked edits; no work was discarded".into())
}

/// One side of a path: absent, or its mode and object as a tree records them.
type Side = Option<(String, String)>;

/// The paths of a raw `-z` diff (`diff-tree`, `diff-index`, `diff-files`) with their two sides.
fn raw_sides(text: &str) -> Result<std::collections::BTreeMap<String, (Side, Side)>, String> {
    let side =
        |mode: &str, object: &str| (mode != "000000").then(|| (mode.to_owned(), object.to_owned()));
    let mut fields = text.split('\0');
    let mut sides = std::collections::BTreeMap::new();
    while let Some(header) = fields.next().filter(|header| !header.is_empty()) {
        let parts: Vec<_> = header.trim_start_matches(':').split(' ').collect();
        let path = fields.next().ok_or("input checkout diff is malformed")?;
        let [before, after, old, new, _] = parts[..] else {
            return Err("input checkout diff is malformed".into());
        };
        sides.insert(path.to_owned(), (side(before, old), side(after, new)));
    }
    Ok(sides)
}

/// Git's executable bit: the owner's.
#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// What the working tree holds at each of `paths` under `top`, as a tree would record it: a
/// regular file's mode and object (Git's clean filters applied, as for the index), a symlink's
/// target, or absent (a directory or nothing). Anything else is a mode no tree holds.
fn working_sides(git: &dyn GitRunner, top: &Path, paths: &[&str]) -> Result<Vec<Side>, String> {
    let mut sides = Vec::with_capacity(paths.len());
    let mut files = Vec::new();
    for path in paths {
        let meta = match top.join(path).symlink_metadata() {
            Ok(meta) => meta,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                sides.push(None);
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        if meta.is_symlink() {
            let target = std::fs::read_link(top.join(path)).map_err(|e| e.to_string())?;
            let out = git
                .git_input(
                    &["hash-object", "-t", "blob", "--no-filters", "--stdin"],
                    target.as_os_str().as_encoded_bytes(),
                )
                .map_err(|e| e.to_string())?;
            if !out.success() {
                return Err(out.stderr_utf8());
            }
            sides.push(Some(("120000".into(), out.stdout_utf8().trim().to_owned())));
        } else if meta.is_dir() {
            sides.push(None);
        } else if meta.is_file() && !path.contains('\n') {
            let mode = if executable(&meta) {
                "100755"
            } else {
                "100644"
            };
            files.push((sides.len(), path));
            sides.push(Some((mode.into(), String::new())));
        } else {
            sides.push(Some(("foreign".into(), String::new())));
        }
    }
    if !files.is_empty() {
        let list: String = files.iter().map(|(_, path)| format!("{path}\n")).collect();
        let out = git
            .git_input(&["hash-object", "--stdin-paths"], list.as_bytes())
            .map_err(|e| e.to_string())?;
        if !out.success() {
            return Err(out.stderr_utf8());
        }
        let text = out.stdout_utf8();
        let objects: Vec<_> = text.lines().collect();
        if objects.len() != files.len() {
            return Err("input checkout hashing named a different number of files".into());
        }
        for ((at, _), object) in files.iter().zip(objects) {
            if let Some((_, slot)) = &mut sides[*at] {
                object.clone_into(slot);
            }
        }
    }
    Ok(sides)
}

/// Refuse while the lock Git takes on `git`'s index stands, naming it. Git holds that lock for a
/// whole checkout and removes it when it stops of its own accord (a failure, a ^C), so a lock
/// that stands is a Git still running in `top`, or one killed outright (`SIGKILL`, the Mac losing
/// power), which leaves its index unwritten and its lock in place. Nothing here can tell those
/// two apart: a Git a killed release started outlives it. Only a person who knows no Git runs
/// there removes the lock, and the transition then resumes.
fn index_unlocked(git: &dyn GitRunner, top: &Path) -> Result<(), String> {
    let lock = git_path(
        git,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "index.lock",
        ],
    )?;
    match lock.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
        Ok(_) => Err(format!(
            "the input checkout {} is locked by Git's {}: a Git command there is still running, \
             or one was killed outright and left its lock; nothing was changed. Once no Git \
             runs in {}, remove {} and run again: the interrupted checkout then resumes",
            top.display(),
            lock.display(),
            top.display(),
            lock.display()
        )),
    }
}

/// Whether `top`'s checkout holds what a checkout of `target` from `before` leaves there when it
/// is cut short, and nothing else. `before` is the tree its index and files stood at when that
/// checkout began: its HEAD, for Git's `checkout`, which writes HEAD last; the empty tree, for a
/// first checkout, which had none. A Git killed outright left its index lock, and nothing is
/// judged until a person removes it ([`index_unlocked`]). Git writes a checkout's files first,
/// then its index, so a checkout cut short leaves files of both trees under the old index, and
/// Git removes a file before it writes its replacement, so the path it was writing holds
/// nothing. That residue is recognised path by path: every path where the index or the working
/// tree differs from `before` is one the two trees disagree on, and holds exactly `before`'s or
/// `target`'s state there, or nothing (whose bytes either tree holds); a byte that is neither is
/// someone's work, and nothing is changed (`Ok(false)`). Residue is described in the index as
/// it stands on disk (each entry an object of `before` or `target`; an empty path at
/// `before`'s), so the checkout of `target` that follows (Git's `checkout`, or for a first
/// checkout of the parent its one-tree read of HEAD, [`admit_parent`]) carries it to `target`
/// with no file forced and nothing discarded. A gitlink's working tree is its submodule's and is
/// judged there.
fn interrupted_checkout(
    git: &dyn GitRunner,
    top: &Path,
    before: &str,
    target: &str,
) -> Result<bool, String> {
    index_unlocked(git, top)?;
    let raw = |args: &[&str]| git_read(git, args).and_then(|text| raw_sides(&text));
    let gitlink = |side: &Side| side.as_ref().is_some_and(|(mode, _)| mode == "160000");
    let changed = raw(&["diff-tree", "-r", "-z", "--no-renames", before, target])?;
    let staged = raw(&[
        "diff-index",
        "--cached",
        "-z",
        "--no-renames",
        "--ignore-submodules=none",
        before,
        "--",
    ])?;
    if staged
        .iter()
        .any(|(path, (_, index))| changed.get(path).is_none_or(|(_, theirs)| theirs != index))
    {
        return Ok(false);
    }
    // Only stat data changes here: the comparisons below then read content, not timestamps.
    // Without `-q`, Git names what stops it (a quiet refresh exits 128 and says nothing); a path
    // whose content changed only exits 1.
    let refresh = git
        .git(&["update-index", "--refresh"])
        .map_err(|e| e.to_string())?;
    if refresh.status > 1 {
        return Err(format!(
            "the input checkout's index could not be refreshed in {}: {}",
            top.display(),
            refresh.stderr_utf8().trim()
        ));
    }
    let unstaged = raw(&[
        "diff-files",
        "-z",
        "--no-renames",
        "--ignore-submodules=none",
    ])?;
    if unstaged.iter().any(|(path, (index, _))| {
        !gitlink(index)
            && changed
                .get(path)
                .is_none_or(|(old, new)| gitlink(old) || gitlink(new))
    }) {
        return Ok(false);
    }
    let files: Vec<_> = changed
        .iter()
        .filter(|(_, (old, new))| !gitlink(old) && !gitlink(new))
        .collect();
    let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    let mut describe = Vec::new();
    for ((path, (old, new)), present) in files.iter().zip(working_sides(git, top, &paths)?) {
        if present.is_some() && present != *old && present != *new {
            return Ok(false);
        }
        // A path that holds nothing keeps (or gets back) `before`'s entry, which the checkout
        // writes over as it does every path still at `before`; one Git removed from under a
        // `before` entry it would otherwise refuse to write as a staged deletion.
        let wanted = present.as_ref().or(old.as_ref());
        let index = staged
            .get(*path)
            .map_or(old.as_ref(), |(_, index)| index.as_ref());
        if wanted != index {
            describe.push(match wanted {
                Some((mode, object)) => format!("{mode} {object}\t{path}\0"),
                None => format!("0 {}\t{path}\0", "0".repeat(before.len())),
            });
        }
    }
    if !describe.is_empty() {
        let out = git
            .git_input(
                &["update-index", "-z", "--index-info"],
                describe.concat().as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        if !out.success() {
            return Err(out.stderr_utf8());
        }
        git.git(&["update-index", "-q", "--refresh"])
            .map_err(|e| e.to_string())?;
    }
    Ok(true)
}

/// Admit the input checkout, at `head` in `pending`'s transition, to the checkout of its target
/// that follows: a clean one as it is, and where this run resumes the transition (`resuming`),
/// one that transition's own checkout cut short ([`interrupted_checkout`]), its residue then
/// described in the index. That checkout is Git's `checkout` from `before`, which writes HEAD
/// last, so its residue sits under `before`'s HEAD: files of the target, whether `before` holds
/// their paths or not, and paths emptied. For a first checkout it is `git worktree add`, which
/// removes what it made when it fails or is signalled. Killed outright once it pointed HEAD at
/// the target (before that, [`point_unborn_checkout`]), it leaves the target's HEAD over no index,
/// some of the target's files, and the lock of the index its checkout was writing, which is
/// refused until a person removes it ([`index_unlocked`]); the files are then completed here by
/// Git's one-tree read of HEAD, which writes only the paths that hold nothing. Anything else is
/// work, refused with nothing changed: tracked work here, and untracked bytes in the target's
/// way by Git's checkout, which names them.
fn admit_parent(
    own: &dyn GitRunner,
    root: &Path,
    pending: &CheckoutPending,
    head: &str,
    resuming: bool,
) -> Result<(), String> {
    if head != pending.target {
        if resuming && interrupted_checkout(own, root, head, &pending.target)? {
            return Ok(());
        }
        return parent_clean(own);
    }
    let Err(edits) = parent_clean(own) else {
        return Ok(());
    };
    if !(resuming
        && pending.before.is_none()
        && git_read(own, &["ls-files", "--cached", "-z"])?.is_empty()
        && interrupted_checkout(own, root, &empty_tree(own)?, &pending.target)?)
    {
        return Err(edits);
    }
    git_run(
        own,
        &[
            "-c",
            "submodule.recurse=false",
            "read-tree",
            "-m",
            "-u",
            "HEAD",
        ],
    )
}

/// A first checkout's `git worktree add` registers the new checkout and writes its `.git` file
/// before it points that checkout's HEAD at the target (until then the HEAD is missing, or, from
/// some Git versions, a placeholder of zeros), and only then writes its files and index. Killed
/// outright in between, it left a checkout Git cannot open, over nothing but its `.git` file.
/// Where `root` is such a checkout of `git`'s repository, point its HEAD at `target` as the add
/// would have next, through the repository's name for it (`worktrees/<id>/HEAD`); the first
/// checkout then completes ([`admit_parent`]). Any other `root` is left as it is, for
/// [`owned_head`] to judge.
fn point_unborn_checkout(git: &dyn GitRunner, root: &Path, target: &str) -> Result<(), String> {
    match root.symlink_metadata() {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    }
    if !empty_directory(root, true)? {
        return Ok(());
    }
    let pointer = match crate::durable::read_regular_text(&root.join(".git"), 4096) {
        Ok(pointer) => pointer,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let Some(admin) = pointer.trim_end_matches('\n').strip_prefix("gitdir: ") else {
        return Ok(());
    };
    let worktrees = git_path(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .join("worktrees");
    let (Ok(admin), Ok(worktrees)) = (root.join(admin).canonicalize(), worktrees.canonicalize())
    else {
        return Ok(());
    };
    let Some(id) = admin
        .file_name()
        .and_then(|id| id.to_str())
        .filter(|_| admin.parent() == Some(worktrees.as_path()))
    else {
        return Ok(());
    };
    let reference = format!("worktrees/{id}/HEAD");
    let head = git
        .git_read(&["rev-parse", "--verify", "-q", &reference])
        .map_err(|e| e.to_string())?
        .stdout_utf8();
    if head.trim().bytes().any(|byte| byte != b'0') {
        return Ok(());
    }
    git_run(git, &["update-ref", "--no-deref", &reference, target])
}

/// The empty tree's object in `git`'s repository: the `before` of a first checkout.
fn empty_tree(git: &dyn GitRunner) -> Result<String, String> {
    let out = git
        .git_input(&["mktree", "-z"], b"")
        .map_err(|e| e.to_string())?;
    if out.success() {
        Ok(out.stdout_utf8().trim().to_owned())
    } else {
        Err(out.stderr_utf8())
    }
}

fn child_gitdir(git: &dyn GitRunner) -> Result<PathBuf, String> {
    let directory = git_path(git, &["rev-parse", "--absolute-git-dir"])?;
    let expected = git_path(
        git,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "modules/vendor/astream",
        ],
    )?;
    let relative = expected
        .strip_prefix(&directory)
        .map_err(|_| "input module gitdir is outside its worktree")?;
    let mut at = directory;
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return Err("input module gitdir has a nonlocal component".into());
        }
        at.push(part.as_os_str());
        match at.symlink_metadata() {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Ok(_) => return Err("input module gitdir crosses a foreign entry".into()),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(expected)
}

fn origin_digest(git: &dyn GitRunner, args: &[&str]) -> Result<String, String> {
    let origin = git_read(git, args)?;
    if origin.trim().is_empty() {
        return Err("input submodule has no origin".into());
    }
    let mut hash = aterm_digest::Sha256::new();
    hash.update(origin.trim_end_matches('\n').as_bytes());
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

fn empty_directory(path: &Path, git_file: bool) -> Result<bool, String> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        if !git_file || entry.file_name() != ".git" {
            return Ok(false);
        }
    }
    Ok(true)
}

fn child_repository(root: &Path, expected: &Path) -> Result<crate::ledger::GitCli, String> {
    if child_gitdir(&crate::ledger::GitCli::new(root))? != expected {
        return Err("input submodule gitdir changed during its transition".into());
    }
    let path = root.join(INPUT_SUBMODULE);
    let pointer =
        crate::durable::read_regular_text(&path.join(".git"), 4096).map_err(|e| e.to_string())?;
    let Some(pointer) = pointer.trim_end_matches('\n').strip_prefix("gitdir: ") else {
        return Err("input submodule has no owned gitdir pointer".into());
    };
    if pointer.is_empty()
        || pointer.chars().any(|c| matches!(c, '\n' | '\r' | '\0'))
        || path
            .join(pointer)
            .canonicalize()
            .map_err(|e| e.to_string())?
            != expected.canonicalize().map_err(|e| e.to_string())?
    {
        return Err("input submodule is not the owned module repository".into());
    }
    Ok(crate::ledger::GitCli::new(path))
}

fn canonical_slot(path: &Path) -> Result<PathBuf, String> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path
            .parent()
            .ok_or("input submodule has no parent")?
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(path.file_name().ok_or("input submodule has no name")?)),
        Err(e) => Err(e.to_string()),
    }
}

fn module_origin(root: &Path, expected: &Path) -> Result<Option<String>, String> {
    let own = crate::ledger::GitCli::new(root);
    if child_gitdir(&own)? != expected {
        return Err("input submodule gitdir changed during its transition".into());
    }
    let meta = match expected.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("input submodule gitdir is not an owned directory".into());
    }
    let directory = expected.to_str().ok_or("non-UTF-8 submodule gitdir")?;
    let root_path = root.to_str().ok_or("non-UTF-8 input checkout path")?;
    let worktree = git_read(
        &own,
        &[
            "--git-dir",
            directory,
            "--work-tree",
            root_path,
            "config",
            "--get",
            "core.worktree",
        ],
    )?;
    if canonical_slot(&expected.join(worktree.trim_end_matches('\n')))?
        != canonical_slot(&root.join(INPUT_SUBMODULE))?
    {
        return Err("input submodule gitdir belongs to a different worktree".into());
    }
    Ok(Some(origin_digest(
        &own,
        &[
            "--git-dir",
            directory,
            "--work-tree",
            root_path,
            "config",
            "--get",
            "remote.origin.url",
        ],
    )?))
}

/// The input submodule's state against its recorded transition: `Ok(None)` while it is still an
/// initialization with nothing materialized, `Ok(Some(head))` at a clean `before` or `target`.
/// Where a receipt records this transition (`receipt`) and it is not yet `complete`, the
/// residue of an interrupted checkout of `target` ([`interrupted_checkout`]) counts as no work:
/// from `before`, and for a first checkout from the clone's default HEAD, whose files it never
/// wrote. Anything else in it is work, and refuses with nothing discarded.
fn child_state(
    root: &Path,
    child: &ChildPending,
    complete: bool,
    receipt: bool,
) -> Result<Option<String>, String> {
    let path = root.join(INPUT_SUBMODULE);
    if let Ok(meta) = path.symlink_metadata()
        && (!meta.is_dir() || meta.file_type().is_symlink())
    {
        return Err("input submodule is not a regular directory".into());
    }
    if empty_directory(&path, false)? {
        if child.before.is_some() || complete {
            return Err("input submodule is unexpectedly absent".into());
        }
        if let Some(expected) = &child.gitdir
            && let Some(origin) = module_origin(root, expected)?
        {
            if child.origin.as_ref() != Some(&origin) {
                return Err("existing input module has a different origin".into());
            }
            let own = crate::ledger::GitCli::new(root);
            if !git_read(
                &own,
                &[
                    "--git-dir",
                    expected.to_str().ok_or("non-UTF-8 submodule gitdir")?,
                    "--work-tree",
                    root.to_str().ok_or("non-UTF-8 input checkout path")?,
                    "ls-files",
                    "--stage",
                    "-z",
                ],
            )?
            .is_empty()
            {
                return Err("existing input module contains an unexpected index".into());
            }
        }
        return Ok(None);
    }
    let expected = child
        .gitdir
        .as_ref()
        .ok_or("input submodule appeared before it was admitted")?;
    let own = child_repository(root, expected)?;
    let origin = module_origin(root, expected)?.ok_or("input submodule gitdir is missing")?;
    if child.origin.as_ref() != Some(&origin) {
        return Err("input submodule origin changed during its transition".into());
    }
    let status = || {
        git_read(
            &own,
            &[
                "status",
                "--porcelain",
                "-z",
                "--no-renames",
                "--untracked-files=all",
                "--ignored=matching",
                "--ignore-submodules=none",
            ],
        )
    };
    // Once residue is described in the index ([`interrupted_checkout`]) only tracked paths may
    // differ, each staged or emptied by Git: an untracked or ignored file, or any other change
    // the index does not hold, is not the checkout's.
    let only_residue = |status: &str| {
        status
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .all(|entry| {
                entry.get(..2).is_some_and(|code| {
                    !code.starts_with(['?', '!']) && (code.ends_with(' ') || code.ends_with('D'))
                })
            })
    };
    // Git's initial clone is --no-checkout. An interrupted clone can have a default HEAD,
    // but may not have indexed or materialized someone else's checkout.
    if !complete
        && child.before.is_none()
        && empty_directory(&path, true)?
        && git_read(&own, &["ls-files", "--stage", "-z"])?.is_empty()
    {
        return Ok(None);
    }
    let head = git_read(&own, &["rev-parse", "HEAD"])?;
    let head = head.trim();
    let dirty = !status()?.is_empty();
    let interrupted = receipt && !complete && dirty;
    // A first checkout cut short leaves the clone's default HEAD over files of `target` alone:
    // still an initialization, which completes from an unborn HEAD ([`finish_empty_clone`]).
    // Anything else under that HEAD is work, whichever commit the clone's default is.
    if interrupted && child.before.is_none() && object_id(head) {
        if interrupted_checkout(&own, &path, &empty_tree(&own)?, &child.target)?
            && only_residue(&status()?)
        {
            return Ok(None);
        }
        if head != child.target {
            return Err("input submodule contains work; no work was discarded".into());
        }
    }
    if !object_id(head)
        || (head != child.target && (complete || child.before.as_deref() != Some(head)))
    {
        return Err("input submodule HEAD is outside its recorded transition".into());
    }
    if dirty
        && !(interrupted
            && head != child.target
            && interrupted_checkout(&own, &path, head, &child.target)?
            && only_residue(&status()?))
    {
        return Err("input submodule contains work; no work was discarded".into());
    }
    Ok(Some(head.into()))
}

fn save_pending(path: &Path, pending: &CheckoutPending) -> Result<(), String> {
    let bytes = aterm_json::to_vec(pending).map_err(|e| e.to_string())?;
    if bytes.len() > 16 * 1024 {
        return Err("input checkout transition exceeds its bound".into());
    }
    crate::durable::write_atomic_durable(path, &bytes, 0o600).map_err(|e| e.to_string())
}

fn finish_empty_clone(root: &Path, child: &ChildPending) -> Result<bool, String> {
    let path = root.join(INPUT_SUBMODULE);
    if child.before.is_some()
        || empty_directory(&path, false)?
        || child_state(root, child, false, true)?.is_some()
    {
        return Ok(false);
    }
    let module = child_repository(
        root,
        child.gitdir.as_ref().ok_or("missing input module gitdir")?,
    )?;
    if git_read(
        &module,
        &["cat-file", "-e", &format!("{}^{{commit}}", child.target)],
    )
    .is_err()
    {
        return Ok(false);
    }
    let unborn = format!("refs/heads/aterm-input-pending-{}", child.target);
    let existing = module
        .git_read(&["show-ref", "--verify", "--quiet", &unborn])
        .map_err(|e| e.to_string())?;
    if existing.status != 1 {
        return Err("input initialization branch is unavailable; no work was changed".into());
    }
    let head = module
        .git_read(&["symbolic-ref", "-q", "HEAD"])
        .map_err(|e| e.to_string())?;
    if head.success() {
        if !head.stdout_utf8().trim().starts_with("refs/heads/") {
            return Err("empty input clone has an unexpected HEAD".into());
        }
    } else if head.status != 1 || git_read(&module, &["rev-parse", "HEAD"])?.trim() != child.target
    {
        return Err("empty input clone has an unexpected detached HEAD".into());
    }
    // A failed --no-checkout clone has a default HEAD but no index or files. Git otherwise
    // treats that empty index as staged deletions. Keep the default branch intact and use
    // an absent branch only while completing the admitted empty checkout. An interruption
    // here remains an empty initialization; no branch ref or existing file is removed.
    git_run(&module, &["symbolic-ref", "HEAD", &unborn])?;
    git_run(
        &module,
        &[
            "checkout",
            "--detach",
            "--no-overwrite-ignore",
            &child.target,
        ],
    )?;
    child_state(root, child, true, true)?;
    Ok(true)
}

/// Maintain the exclusively claimed input checkout and hydrate its exact gitlink together.
/// A failed hydration keeps a bounded transition receipt so a retry can distinguish our
/// clean old pin from edits. No checkout, reset, clean or submodule operation is forced.
pub fn checkout(git: &dyn GitRunner, root: &Path, tip: &str) -> Result<(), String> {
    checkout_with(git, root, tip, |own| {
        git_run(
            own,
            &[
                "submodule",
                "update",
                "--init",
                "--checkout",
                INPUT_SUBMODULE,
            ],
        )
    })
}

fn checkout_with(
    git: &dyn GitRunner,
    root: &Path,
    tip: &str,
    mut hydrate: impl FnMut(&crate::ledger::GitCli) -> Result<(), String>,
) -> Result<(), String> {
    if !object_id(tip) {
        return Err("input checkout needs an exact commit".into());
    }
    let parent = root.parent().ok_or("input checkout has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let physical = parent
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(root.file_name().ok_or("input checkout has no name")?);
    let repository = git_path(
        git,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .canonicalize()
    .map_err(|e| e.to_string())?;
    let receipt = pending_path(root);
    loop {
        let loaded = match crate::durable::read_regular_bytes(&receipt, 16 * 1024) {
            Ok(bytes) => {
                Some(aterm_json::from_slice::<CheckoutPending>(&bytes).map_err(|e| e.to_string())?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        // A receipt read here is a transition an earlier run entered and did not finish.
        let resuming = loaded.is_some();
        let mut pending = if let Some(pending) = loaded {
            if pending.format != 1
                || pending.repository != repository
                || pending.root != physical
                || !object_id(&pending.target)
                || pending.before.as_ref().is_some_and(|s| !object_id(s))
                || pending.child.as_ref().is_some_and(|c| {
                    !object_id(&c.target)
                        || c.before.as_ref().is_some_and(|s| !object_id(s))
                        || c.origin
                            .as_ref()
                            .is_some_and(|s| !aterm_codec::hex::is_lower(s, 64))
                        || (c.before.is_some()
                            && (pending.before.is_none()
                                || c.gitdir.is_none()
                                || c.origin.is_none()))
                })
                || gitlink(git, &pending.target)?
                    != pending.child.as_ref().map(|c| c.target.clone())
            {
                return Err(
                    "input checkout transition is incompatible; no work was changed".into(),
                );
            }
            if let Some(child) = &pending.child
                && let Some(before) = &child.before
                && gitlink(git, pending.before.as_deref().unwrap())?.as_ref() != Some(before)
            {
                return Err("input checkout transition has no matching prior gitlink".into());
            }
            pending
        } else {
            let before = owned_head(git, root)?;
            let target = git_read(
                git,
                &["rev-parse", "--verify", &format!("{tip}^{{commit}}")],
            )?
            .trim()
            .to_owned();
            let child = if let Some(target_pin) = gitlink(git, &target)? {
                let mut child = ChildPending {
                    before: None,
                    target: target_pin,
                    gitdir: None,
                    origin: None,
                };
                if let Some(head) = &before {
                    let own = crate::ledger::GitCli::new(root);
                    parent_clean(&own)?;
                    child.gitdir = Some(child_gitdir(&own)?);
                    if !empty_directory(&root.join(INPUT_SUBMODULE), false)? {
                        child.before = gitlink(git, head)?;
                        if child.before.is_none() {
                            return Err("input submodule was not admitted by the prior tree".into());
                        }
                        let module = child_repository(root, child.gitdir.as_ref().unwrap())?;
                        child.origin = Some(origin_digest(
                            &module,
                            &["config", "--get", "remote.origin.url"],
                        )?);
                        // Before a new transition, even a clean manually moved HEAD is refused.
                        let state = child_state(root, &child, false, false)?;
                        if state != child.before {
                            return Err("input submodule is off its prior gitlink".into());
                        }
                    }
                }
                Some(child)
            } else {
                if let Some(head) = &before {
                    parent_clean(&crate::ledger::GitCli::new(root))?;
                    if gitlink(git, head)?.is_some() {
                        return Err("input checkout cannot retire an initialized submodule".into());
                    }
                }
                None
            };
            if before.as_ref() == Some(&target)
                && child
                    .as_ref()
                    .is_none_or(|c| c.before.as_ref() == Some(&c.target))
            {
                return Ok(());
            }
            let pending = CheckoutPending {
                format: 1,
                repository: repository.clone(),
                root: physical.clone(),
                before,
                target,
                child,
            };
            save_pending(&receipt, &pending)?;
            pending
        };
        if resuming && pending.before.is_none() {
            point_unborn_checkout(git, root, &pending.target)?;
        }
        let head = owned_head(git, root)?;
        if head.as_deref() != Some(pending.target.as_str()) && head != pending.before {
            return Err("input checkout HEAD is outside its recorded transition".into());
        }
        if let Some(at) = &head {
            let own = crate::ledger::GitCli::new(root);
            admit_parent(&own, root, &pending, at, resuming)?;
            if let Some(child) = &pending.child {
                child_state(root, child, false, true)?;
            }
            if head.as_deref() != Some(pending.target.as_str()) {
                git_run(
                    &own,
                    &[
                        "-c",
                        "submodule.recurse=false",
                        "checkout",
                        "--no-overwrite-ignore",
                        "--detach",
                        &pending.target,
                    ],
                )?;
            }
        } else {
            git_run(
                git,
                &[
                    "-c",
                    "submodule.recurse=false",
                    "worktree",
                    "add",
                    "--detach",
                    root.to_str().ok_or("non-UTF-8 input checkout path")?,
                    &pending.target,
                ],
            )?;
        }
        let own = crate::ledger::GitCli::new(root);
        if let Some(child) = &mut pending.child {
            if child.gitdir.is_none() {
                if !empty_directory(&root.join(INPUT_SUBMODULE), false)? {
                    return Err("input submodule appeared before initialization".into());
                }
                child.gitdir = Some(child_gitdir(&own)?);
            }
            git_run(&own, &["submodule", "init", "--", INPUT_SUBMODULE])?;
            let origin = origin_digest(&own, &["config", "--get", "submodule.vendor/astream.url"])?;
            if child
                .origin
                .as_ref()
                .is_some_and(|expected| expected != &origin)
            {
                return Err("input submodule configured origin changed".into());
            }
            child.origin = Some(origin);
            save_pending(&receipt, &pending)?;
            child_state(root, pending.child.as_ref().unwrap(), false, true)?;
            // A first checkout cut short completes from its unborn HEAD after Git's update,
            // whether that update refused the residue or carried part of it to the pin.
            let hydration = hydrate(&own);
            let initialized = finish_empty_clone(root, pending.child.as_ref().unwrap())?;
            if let Err(why) = hydration
                && !initialized
            {
                return Err(format!(
                    "{why}; input hydration is pending and will resume on retry"
                ));
            }
            child_state(root, pending.child.as_ref().unwrap(), true, true)?;
        }
        if owned_head(git, root)?.as_deref() != Some(pending.target.as_str()) {
            return Err("input checkout changed during hydration".into());
        }
        parent_clean(&own)?;
        std::fs::remove_file(&receipt).map_err(|e| e.to_string())?;
        crate::durable::sync_dir(parent).map_err(|e| e.to_string())?;
        if pending.target == tip {
            return Ok(());
        }
        // A newer selection does not reinterpret residue from an older one: settle that
        // exact transition first, then admit the new target with a fresh clean snapshot.
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Attempt {
    tree: String,
    seal: String,
}

/// The immediately preceding input attempt for this release base. Its A19 package rows are
/// only candidates; a failed or unfinished attempt cannot resurrect an older success.
pub fn previous_items(
    records: &Path,
    base: &str,
) -> Result<Vec<aterm_verify::input_content::Proof>, String> {
    let at = records.join(format!(
        "previous-{}.json",
        crate::check_record::seal_dir(base)
    ));
    let text = match std::fs::read_to_string(at) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let previous: Attempt = aterm_json::from_str(&text).map_err(|e| e.to_string())?;
    if !aterm_codec::hex::is_lower(&previous.tree, 40)
        && !aterm_codec::hex::is_lower(&previous.tree, 64)
    {
        return Err("previous input judgment has an invalid tree".into());
    }
    crate::check_record::input_item_candidates(records, &previous.tree, &previous.seal)
}

/// Remember the current attempt before dispatch, so an interrupted or failed run also replaces
/// the predecessor. A19 retains its begun/FAIL record independently of this pointer.
pub fn attempted(records: &Path, base: &str, tree: &str, seal: &str) -> Result<(), String> {
    let text = aterm_json::to_string(&Attempt {
        tree: tree.into(),
        seal: seal.into(),
    })
    .map_err(|e| e.to_string())?;
    crate::durable::write_atomic_durable(
        &records.join(format!(
            "previous-{}.json",
            crate::check_record::seal_dir(base)
        )),
        text.as_bytes(),
        0o600,
    )
    .map_err(|e| e.to_string())
}

/// Create and claim one record directory without waiting. A concurrent dry or real run must
/// not overwrite its writer's begun record. The guard unlocks explicitly before closing.
pub fn claim(directory: &Path) -> Result<aterm_uds::file_lock::LockFile, String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    aterm_uds::file_lock::LockFile::take(
        &directory.join("input.lock"),
        std::time::Instant::now(),
        "this release-input judgment is already running",
    )
    .map_err(|e| e.to_string())
}

/// This child names its own explicit lane. An outer targo's live authority endpoint and
/// the coding agent's invocation metadata do not belong in the source-check environment.
pub fn detach_launcher(command: &mut Command) {
    let names: std::collections::BTreeSet<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| {
            name.to_str()
                .is_some_and(|name| name.starts_with("TRUST_TARGO_") || name.starts_with("CODEX_"))
        })
        .collect();
    for name in names {
        command.env_remove(name);
    }
}

/// Explain a key without retaining environment values. These digests make a missed reuse
/// attributable to a changed input rather than an opaque change of the combined seal.
pub fn describe(command: &Command, seal: &str, engine: &EngineInput) -> Result<String, String> {
    let _validated = engine.environment(command)?;
    let digest = |bytes: &[u8]| {
        let mut hash = aterm_digest::Sha256::new();
        hash.update(bytes);
        aterm_codec::hex::encode_lower(&hash.finalize())
    };
    let mut env: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    for (name, value) in command.get_envs() {
        if let Some(value) = value {
            env.insert(name.to_owned(), value.to_owned());
        } else {
            env.remove(name);
        }
    }
    let mut parts: std::collections::BTreeMap<String, String> = env
        .into_iter()
        .map(|(name, value)| {
            (
                format!("env:{}", name.to_string_lossy()),
                digest(value.as_encoded_bytes()),
            )
        })
        .collect();
    parts.insert("build-inputs".into(), digest(seal.as_bytes()));
    match &engine.source {
        EnginePolicySource::Repository(repository) => {
            parts.insert("engine:policy-source".into(), "repository".into());
            parts.insert(
                "engine:authority".into(),
                digest(repository.to_string().as_bytes()),
            );
        }
        #[cfg(test)]
        EnginePolicySource::LocalFixture => {
            parts.insert("engine:policy-source".into(), "local-fixture".into());
        }
    }
    parts.insert("engine:commit".into(), engine.commit.clone());
    parts.insert(
        "driver".into(),
        crate::artifact::file_sha256(&std::env::current_exe().map_err(|e| e.to_string())?)?,
    );
    aterm_json::to_string(&parts).map_err(|e| e.to_string())
}

/// The tree-independent execution context of preflight readers. File content belongs in
/// each reader's key; external configuration, policy and the finalized environment do not.
pub fn context(
    root: &Path,
    engine: &EngineInput,
    seal: &str,
    command: &Command,
) -> Result<String, String> {
    let driver =
        crate::artifact::file_sha256(&std::env::current_exe().map_err(|e| e.to_string())?)?;
    let mut hash = aterm_digest::Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(b"aterm-preflight-context-v2");
    engine.bind(&mut part);
    part(
        crate::check_content::context_with_environment(
            &driver,
            seal,
            command,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
            engine.environment(command)?,
        )
        .as_bytes(),
    );
    for directory in root.ancestors() {
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "trustfmt.toml",
            ".trustfmt.toml",
            "rustfmt.toml",
            ".rustfmt.toml",
            "clippy.toml",
            ".clippy.toml",
        ] {
            file(&mut part, &directory.join(name))?;
        }
    }
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
    {
        for name in ["config", "config.toml"] {
            file(&mut part, &home.join(name))?;
        }
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

/// Bind the tree, discovered input, complete command environment, executable and build seal.
/// Configuration outside the tree and the publication policy have their own identities.
pub fn identity(
    git: &dyn GitRunner,
    root: &Path,
    engine: &EngineInput,
    tip: &str,
    base: &str,
    seal: &str,
    command: &Command,
) -> Result<(String, String), String> {
    let tree = crate::check_record::tree_of(git, tip)?;
    let heads = git
        .git(&["rev-parse", "HEAD", &format!("{tip}^{{commit}}")])
        .map_err(|e| e.to_string())?;
    let names = heads.stdout_utf8();
    let names: Vec<_> = names.split_whitespace().collect();
    if !heads.success() || names.len() != 2 || names[0] != names[1] {
        return Err("release-input checkout moved away from the judged commit".into());
    }
    let status = git
        .git(&["status", "--porcelain", "--untracked-files=no"])
        .map_err(|e| e.to_string())?;
    if !status.success() || !status.stdout_utf8().is_empty() {
        return Err("release-input checkout has tracked changes".into());
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(b"aterm-release-input-v2");
    engine.bind(&mut part);
    part(tree.as_bytes());
    part(base.as_bytes());
    let base_commit = git
        .git(&["rev-parse", "--verify", &format!("{base}^{{commit}}")])
        .map_err(|e| e.to_string())?;
    if !base_commit.success() {
        return Err(base_commit.stderr_utf8());
    }
    part(base_commit.stdout_utf8().as_bytes());
    part(seal.as_bytes());
    let image = std::env::current_exe().map_err(|e| e.to_string())?;
    let driver = crate::artifact::file_sha256(&image)?;
    part(driver.as_bytes());
    part(
        crate::check_content::context_with_environment(
            &driver,
            seal,
            command,
            crate::check_content::ReleaseHalf::Tree,
            crate::check_content::Admission::WaitDefault,
            engine.environment(command)?,
        )
        .as_bytes(),
    );
    let input = aterm_verify::release_input::discover(
        root,
        &std::env::var_os("PATH").unwrap_or_default(),
        base,
        tip,
    )?;
    for check in aterm_verify::release_input::CHECKS {
        part(check.name().as_bytes());
    }
    for path in &input.paths {
        part(path.as_bytes());
    }
    for commit in &input.commits {
        part(commit.as_bytes());
    }
    // A19's tree key assumes a clean checkout. Include all nonignored additions too:
    // fmt and Cargo can read them even though they are absent from the commit tree.
    let listed = git
        .git(&["ls-files", "--others", "--exclude-standard", "-z"])
        .map_err(|e| e.to_string())?;
    if !listed.success() {
        return Err(listed.stderr_utf8());
    }
    for name in listed.stdout_utf8().split('\0').filter(|s| !s.is_empty()) {
        part(name.as_bytes());
        part(crate::artifact::file_sha256(&root.join(name))?.as_bytes());
    }
    for directory in root.ancestors() {
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "trustfmt.toml",
            ".trustfmt.toml",
            "rustfmt.toml",
            ".rustfmt.toml",
            "clippy.toml",
            ".clippy.toml",
        ] {
            file(&mut part, &directory.join(name))?;
        }
    }
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
    {
        for name in ["config", "config.toml"] {
            file(&mut part, &home.join(name))?;
        }
    }
    Ok((tree, aterm_codec::hex::encode_lower(&hash.finalize())))
}

fn file(part: &mut impl FnMut(&[u8]), path: &Path) -> Result<(), String> {
    part(path.as_os_str().as_encoded_bytes());
    match std::fs::symlink_metadata(path) {
        Ok(_) => part(crate::artifact::file_sha256(path)?.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => part(b"absent"),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    }
    Ok(())
}

#[cfg(test)]
#[path = "input_record_tests.rs"]
mod tests;
