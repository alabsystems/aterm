// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One converge pass's record of each checkout's facts (FL-164): what three batched gits tell
//! of a checkout, read once a pass and read again only where a file they come from changed.
//!
//! [`Facts::of`] fills a checkout's record with one `rev-parse` of every query but HEAD
//! ([`REV_PARSE`]), one `config --null --show-scope --show-origin --get-regexp` over [`KEYS`]
//! ([`config_listing`]) and one `for-each-ref` of the refs HEAD and `refs/remotes/origin/HEAD`
//! name ([`ref_listing`]). It reads those two symbolic refs from their files, and asks
//! `symbolic-ref` only where a file cannot say (one it cannot read, a reftable repository's).
//! `ls-remote --get-url origin` runs once a record, where a reader asks for it
//! ([`Facts::effective_url`]), and what a commit declares of a required gitlink (its pin and its
//! URL, from the commit's tree and `.gitmodules`) is read once a record too ([`Facts::gitlink`]).
//! Every reader of a checkout's facts takes them from here: the standing read of
//! [`crate::init_github`], [`super::local`], [`super::tracks_origin`], the storage checks, the
//! hooks and filters a command turns off, and the fetched default branch
//! ([`crate::clone_tuning`]'s reads of both included).
//!
//! Before each use, the record is checked against the metadata of every file its facts come
//! from ([`Facts::watch`]): the checkout's `.git` and the HEAD, config, index, shallow, ref,
//! replacement and alternates files in it, the ref files HEAD and origin's HEAD name, the
//! account's and the system's git config files, and the git that read it with the config files
//! of its own build ([`program_files`]). Any change, a file that cannot be observed as it was
//! included, reads the record again, so a guard that runs between a pass's reads (before a
//! fetch, before a merge) judges what is there now. A record whose facts come from anywhere else
//! (a config value from a file it does not watch, a conditional include, an include of a file it
//! does not watch, a `.git` that is no plain directory, a reftable store, a file that changed
//! while it was read) is never reused: each reader reads it afresh ([`bound`]). An include of a
//! file it watches is one more file it watches: a managed GNU Git's own system file includes
//! `/etc/gitconfig` so. What a worktree's files hold is never recorded: `status` runs before each
//! network read and each merge ([`super::local`]). A config file git reads that names no key of
//! [`KEYS`] and is none of the files watched (one a variable of git's environment names,
//! `GIT_CONFIG_SYSTEM` say) is not watched, and neither is a git build another selects while a
//! pass runs: either is read by the next pass, which starts with no record
//! ([`crate::port::CheckoutRecords::new_pass`]).

use super::{NotFetched, Programs, git, oid, run, stdout};
use crate::model::User;
use crate::port::{Cmd, System};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// What the record's `rev-parse` asks, in the order git prints the answers: whether `-C`'s
/// folder is in a worktree, its path below the top (an empty line at it), the top, whether the
/// repository is shallow, and where its index and its alternates file are (relative to the
/// folder for its own `.git`, else absolute). HEAD is asked of no `rev-parse` here: one HEAD
/// with no commit would fail the whole batch.
pub(crate) const REV_PARSE: [&str; 9] = [
    "rev-parse",
    "--is-inside-work-tree",
    "--show-prefix",
    "--show-toplevel",
    "--is-shallow-repository",
    "--git-path",
    "index",
    "--git-path",
    "objects/info/alternates",
];

/// The config keys the record lists, in git's canonical spelling (section and variable in
/// lower case): origin's URL, each branch's upstream, the keys that make a partial clone, the
/// ref storage, each hook and filter driver ([`Programs`]), each URL rewrite (which `ls-remote
/// --get-url` applies) and each include, so that every file naming one is a file the record
/// either watches or is not reused for.
pub(crate) const KEYS: &str = r"^(remote\.origin\.url|branch\..*\.(remote|merge)|extensions\.(partialclone|refstorage)|remote\..*\.(promisor|partialclonefilter)|(hook|filter)\..*\.[^.]+|url\..*\.(insteadof|pushinsteadof)|include\.path|includeif\..*\.path)$";

/// The `for-each-ref` format: each ref's object, that object's type (which reads it, so a ref to
/// an object this repository lacks fails the read), and the ref's name.
pub(crate) const REF_FORMAT: &str = "--format=%(objectname) %(objecttype) %(refname)";

/// The longest symbolic-ref file read.
const REF_FILE_BYTES: usize = 4096;

/// The record's `rev-parse` in `path` ([`REV_PARSE`]).
pub(crate) fn rev_parse(sys: &dyn System, user: &User, path: &Path) -> Cmd {
    git(sys, user, Some(path)).args(REV_PARSE)
}

/// The record's config listing in `path`: every [`KEYS`] key with its scope and the file it is
/// in, in the C locale, where git's regular expressions match bytes (under a UTF-8 locale `.`
/// matches no byte that is not UTF-8, so a key named in such bytes would go unlisted while git
/// still read it).
pub(crate) fn config_listing(sys: &dyn System, user: &User, path: &Path) -> Cmd {
    git(sys, user, Some(path)).env("LC_ALL", "C").args([
        "config",
        "--null",
        "--show-scope",
        "--show-origin",
        "--get-regexp",
        KEYS,
    ])
}

/// The record's ref read in `path`: [`REF_FORMAT`] of each of `refs`.
pub(crate) fn ref_listing(sys: &dyn System, user: &User, path: &Path, refs: &[String]) -> Cmd {
    git(sys, user, Some(path))
        .args(["for-each-ref", REF_FORMAT])
        .args(refs.iter().map(String::as_str))
}

/// Where a checkout's HEAD points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Head {
    /// At a ref (`refs/heads/main`), whose commit the record's refs hold where there is one.
    Symbolic(String),
    /// Detached: the commit HEAD names, where it is one this repository has.
    Detached(Option<String>),
}

/// One entry of the record's config listing.
#[derive(Debug)]
struct Entry {
    /// The scope git read it in (`local`, `global`, `system`, `worktree`, `command`).
    scope: Vec<u8>,
    /// Where git read it (`file:.git/config`, `command line:`).
    origin: Vec<u8>,
    /// Its key, canonical.
    key: Vec<u8>,
    /// Its value; none for a key written with no `=`.
    value: Option<Vec<u8>>,
}

/// What a symbolic-ref file says: another ref, or an object.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RefFile {
    Symbolic(String),
    Direct(String),
}

/// One checkout's facts, as [`Facts::of`] read them.
#[derive(Debug)]
pub(crate) struct Facts {
    /// The folder is the top of a worktree: inside it, at no path below the top, the top an
    /// absolute path.
    top: bool,
    /// Whether the repository is shallow: true unless git said `false`.
    shallow: bool,
    /// Where its index is.
    index: PathBuf,
    /// Where its alternates file is.
    alternates: PathBuf,
    /// Every [`KEYS`] entry, in git's order.
    config: Vec<Entry>,
    /// Where HEAD points.
    head: Head,
    /// The ref `refs/remotes/origin/HEAD` names, none where it names none (or no such ref is
    /// there), or why it cannot be read.
    origin_head: Result<Option<String>, String>,
    /// Each ref the record read: its object, and whether that is a commit.
    refs: BTreeMap<String, (String, bool)>,
    /// `ls-remote --get-url origin`, once a reader asked ([`Self::effective_url`]).
    effective: OnceLock<Result<String, String>>,
    /// What a commit declares of a gitlink, by the commit and the gitlink's path: its pinned
    /// commit and its declared URL, once a reader read them ([`Self::gitlink`]).
    gitlinks: Mutex<BTreeMap<(String, String), (String, String)>>,
    /// The files the facts come from.
    watch: Vec<PathBuf>,
    /// Their metadata as the facts were read, where the record may be reused.
    seen: Option<[u8; 32]>,
}

impl Facts {
    /// `path`'s facts as they stand now, for `user`: the pass's record where its files are as
    /// they were when it was read ([`Self::watch`]), else read again with its three gits (and
    /// kept, where [`System::checkout_records`] keeps a record and it may be reused).
    ///
    /// # Errors
    /// A git could not run, or failed, or answered what no git answers.
    pub(crate) fn of(sys: &dyn System, user: &User, path: &Path) -> Result<Arc<Self>, String> {
        let records = sys.checkout_records();
        if let Some(records) = &records
            && let Some(kept) = records.get(user.uid, path)
            && kept
                .seen
                .is_some_and(|seen| observe(sys, &kept.watch) == seen)
        {
            return Ok(kept);
        }
        let read = match Self::read(sys, user, path) {
            Ok(read) => Arc::new(read),
            Err(why) => {
                if let Some(records) = &records {
                    records.forget(user.uid, path);
                }
                return Err(why);
            }
        };
        if let Some(records) = &records {
            if read.seen.is_some() {
                records.put(user.uid, path, Arc::clone(&read));
            } else {
                records.forget(user.uid, path);
            }
        }
        Ok(read)
    }

    /// Read `path`'s facts afresh.
    fn read(sys: &dyn System, user: &User, path: &Path) -> Result<Self, String> {
        let admin = path.join(".git");
        // Only a `.git` that is a directory of its own names its files where this looks.
        let plain = sys.stat(&admin).is_ok_and(|stat| stat.dir && !stat.symlink)
            && matches!(
                sys.stat(&admin.join("commondir")),
                Err(error) if error.kind() == io::ErrorKind::NotFound
            );
        let head_path = admin.join("HEAD");
        let origin_path = admin.join("refs/remotes/origin/HEAD");
        let head_file = if plain {
            ref_file(sys, &head_path)
        } else {
            None
        };
        let origin_file = if plain {
            ref_file(sys, &origin_path)
        } else {
            None
        };
        // The git every read here runs: its program and its build's own config files are
        // watched beside the account's and the system's.
        let batch_cmd = rev_parse(sys, user, path);
        let mut watched: BTreeSet<PathBuf> =
            layout(&admin, Path::new(&user.home)).into_iter().collect();
        watched.extend(program_files(sys, &batch_cmd.program));
        for file in [&head_file, &origin_file] {
            if let Some(RefFile::Symbolic(reference)) = file {
                watched.extend(ref_paths(&admin, reference));
            }
        }
        let watch: Vec<PathBuf> = watched.into_iter().collect();
        let before = observe(sys, &watch);

        let batch = run(sys, &batch_cmd, "rev-parse")?;
        if !batch.success() {
            return Err("git rev-parse failed in the checkout".into());
        }
        let text = String::from_utf8(batch.stdout)
            .map_err(|_| "git rev-parse answered bytes that are not UTF-8".to_owned())?;
        let lines: Vec<&str> = text.split_terminator('\n').collect();
        let [inside, prefix, toplevel, shallow, index, alternates] = lines.as_slice() else {
            return Err("git rev-parse answered unexpectedly in the checkout".into());
        };
        let top = *inside == "true" && prefix.is_empty() && Path::new(*toplevel).is_absolute();
        let shallow = *shallow != "false";
        let index = path.join(*index);
        let alternates = path.join(*alternates);

        let listing_cmd = config_listing(sys, user, path);
        let same_git = listing_cmd.program == batch_cmd.program;
        let listing = run(sys, &listing_cmd, "config")?;
        let config = match listing.status {
            Some(0) => entries(&listing.stdout)
                .ok_or("git config answered unexpectedly in the checkout")?,
            Some(1) if listing.stdout.is_empty() => Vec::new(),
            _ => return Err("cannot read the checkout's Git config".into()),
        };
        let reftable = config.iter().any(|entry| {
            entry.key == b"extensions.refstorage"
                && entry.value.as_deref() == Some(b"reftable".as_slice())
        });

        let head = match &head_file {
            Some(RefFile::Symbolic(reference)) if !reftable => Head::Symbolic(reference.clone()),
            Some(RefFile::Direct(object)) if !reftable => {
                Head::Detached(commit_here(sys, user, path, object)?)
            }
            _ => ask_head(sys, user, path)?,
        };
        let (origin_head, origin_asked) = match &origin_file {
            Some(RefFile::Symbolic(reference)) if !reftable => (Ok(Some(reference.clone())), false),
            Some(RefFile::Direct(_)) if !reftable => (Ok(None), false),
            _ => (ask_origin_head(sys, user, path), true),
        };

        let mut wanted: Vec<String> = Vec::new();
        if let Head::Symbolic(reference) = &head {
            wanted.push(reference.clone());
        }
        if let Ok(Some(reference)) = &origin_head
            && plain_ref(reference)
            && !wanted.contains(reference)
        {
            wanted.push(reference.clone());
        }
        // No pattern at all would list every ref.
        let refs = if wanted.is_empty() {
            BTreeMap::new()
        } else {
            let listed = run(sys, &ref_listing(sys, user, path, &wanted), "for-each-ref")?;
            if !listed.success() {
                return Err("cannot read the checkout's refs".into());
            }
            parse_refs(&listed.stdout, &wanted)
                .ok_or("git for-each-ref answered unexpectedly in the checkout")?
        };

        let reusable = plain
            && !reftable
            && head_file.is_some()
            && !(origin_asked && matches!(origin_head, Ok(Some(_))))
            && index == admin.join("index")
            && alternates == admin.join("objects/info/alternates")
            && same_git
            && config.iter().all(|entry| bound(path, entry, &watch))
            // HEAD and origin's HEAD as they were read, and every watched file as it was before
            // the reads: nothing changed while they ran.
            && ref_file(sys, &head_path) == head_file
            && ref_file(sys, &origin_path) == origin_file
            && observe(sys, &watch) == before;
        Ok(Self {
            top,
            shallow,
            index,
            alternates,
            config,
            head,
            origin_head,
            refs,
            effective: OnceLock::new(),
            gitlinks: Mutex::default(),
            watch,
            seen: reusable.then_some(before),
        })
    }

    /// Whether the folder is the top of a worktree.
    pub(crate) fn top(&self) -> bool {
        self.top
    }

    /// Whether the repository is shallow (or git did not say it is not).
    pub(crate) fn shallow(&self) -> bool {
        self.shallow
    }

    /// Whether its config makes it a partial clone, one a read of an object it lacks fetches
    /// lazily: `extensions.partialclone`, or a promisor remote's `promisor` or
    /// `partialclonefilter`, whatever its name (in any bytes).
    pub(crate) fn partial(&self) -> bool {
        self.config.iter().any(|entry| {
            entry.key == b"extensions.partialclone"
                || (entry.key.starts_with(b"remote.")
                    && (entry.key.ends_with(b".promisor")
                        || entry.key.ends_with(b".partialclonefilter")))
        })
    }

    /// Where its index is.
    pub(crate) fn index(&self) -> &Path {
        &self.index
    }

    /// Where its alternates file is.
    pub(crate) fn alternates(&self) -> &Path {
        &self.alternates
    }

    /// Each value of `key` (canonical), in git's order; a key written with no `=` is an empty
    /// value.
    pub(crate) fn values(&self, key: &str) -> Vec<String> {
        self.config
            .iter()
            .filter(|entry| entry.key == key.as_bytes())
            .map(|entry| {
                entry
                    .value
                    .as_deref()
                    .map(|value| String::from_utf8_lossy(value).into_owned())
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Each hook and filter driver its config names ([`Programs`]).
    ///
    /// # Errors
    /// One is named in bytes that are not UTF-8, which no override can name: a command there
    /// could start it.
    pub(crate) fn programs(&self) -> Result<Programs, String> {
        let mut named = Programs::default();
        for entry in &self.config {
            let hook = entry.key.starts_with(b"hook.");
            if !hook && !entry.key.starts_with(b"filter.") {
                continue;
            }
            let key = std::str::from_utf8(&entry.key).map_err(|_| {
                "its config names a hook or filter whose name is not UTF-8, which aterm cannot \
                 turn off"
                    .to_owned()
            })?;
            // The section ends at the first dot and the variable starts after the last, so the
            // name between them keeps any dot of its own, and may be empty.
            let Some((_, rest)) = key.split_once('.') else {
                continue;
            };
            let Some((name, _)) = rest.rsplit_once('.') else {
                continue;
            };
            if hook {
                named.hooks.insert(name.to_owned());
            } else {
                named.filters.insert(name.to_owned());
            }
        }
        Ok(named)
    }

    /// Where HEAD points.
    pub(crate) fn head(&self) -> &Head {
        &self.head
    }

    /// The commit HEAD names, where it is one this repository has.
    pub(crate) fn head_commit(&self) -> Option<&str> {
        match &self.head {
            Head::Symbolic(reference) => self.commit_of(reference),
            Head::Detached(commit) => commit.as_deref(),
        }
    }

    /// The commit `reference` names, where the record read that ref and it names a commit.
    pub(crate) fn commit_of(&self, reference: &str) -> Option<&str> {
        self.refs
            .get(reference)
            .filter(|(_, commit)| *commit)
            .map(|(oid, _)| oid.as_str())
    }

    /// Whether the record read `reference` (it names a ref HEAD or origin's HEAD names).
    pub(crate) fn read_ref(&self, reference: &str) -> bool {
        match &self.head {
            Head::Symbolic(head) if head == reference => true,
            _ => matches!(&self.origin_head, Ok(Some(origin)) if origin == reference),
        }
    }

    /// Every commit a ref the record read names.
    pub(crate) fn commits(&self) -> BTreeSet<String> {
        self.refs
            .values()
            .filter(|(_, commit)| *commit)
            .map(|(oid, _)| oid.clone())
            .collect()
    }

    /// Whether the checkout is an unfinished branch: HEAD names a branch with no ref at all
    /// (its name), or a commit here (none).
    ///
    /// # Errors
    /// HEAD is detached at no commit here, names no branch, or names a branch whose ref is
    /// there with no commit: a missing object, detached HEAD or unreadable ref is not an empty
    /// checkout.
    pub(crate) fn unborn(&self) -> Result<Option<String>, String> {
        if self.head_commit().is_some() {
            return Ok(None);
        }
        let Head::Symbolic(reference) = &self.head else {
            return Err("the checkout has no readable symbolic branch".into());
        };
        let Some(branch) = reference
            .strip_prefix("refs/heads/")
            .filter(|branch| !branch.is_empty())
        else {
            return Err("the checkout has no readable symbolic branch".into());
        };
        if self.refs.contains_key(reference) {
            return Err("the checkout's branch exists without a readable commit".into());
        }
        Ok(Some(branch.to_owned()))
    }

    /// Whether HEAD is on `branch` and that branch tracks origin's branch of the same name:
    /// `branch.<branch>.remote` is `origin` and `branch.<branch>.merge` that branch, each once.
    pub(crate) fn tracks_origin(&self, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        self.head == Head::Symbolic(reference.clone())
            && self.values(&format!("branch.{branch}.remote")) == ["origin"]
            && self.values(&format!("branch.{branch}.merge")) == [reference]
    }

    /// Origin's default branch and its tip as this checkout last fetched them
    /// (`refs/remotes/origin/HEAD`, which a clone writes, and the branch it names): no network
    /// read.
    ///
    /// # Errors
    /// The checkout records no default branch of origin's, or not its tip
    /// ([`NotFetched::Unrecorded`]), or its record of them cannot be read.
    pub(crate) fn fetched_head(&self) -> Result<(String, String), NotFetched> {
        let default = match &self.origin_head {
            Err(why) => return Err(NotFetched::from(why.clone())),
            Ok(None) => {
                return Err(NotFetched::Unrecorded {
                    why: "this checkout records no default branch of origin's \
                          (refs/remotes/origin/HEAD), so whether it is behind cannot be read \
                          without the network"
                        .to_owned(),
                    act: "remote set-head origin --auto",
                });
            }
            Ok(Some(reference)) => match reference.strip_prefix("refs/remotes/origin/") {
                Some(branch) if !branch.is_empty() => branch.to_owned(),
                _ => {
                    return Err(NotFetched::from(
                        "refs/remotes/origin/HEAD names no branch of origin's".to_owned(),
                    ));
                }
            },
        };
        match self.commit_of(&format!("refs/remotes/origin/{default}")) {
            Some(tip) => Ok((default, tip.to_owned())),
            None => Err(NotFetched::Unrecorded {
                why: format!(
                    "refs/remotes/origin/HEAD names origin/{default}, which this checkout does not \
                     hold, so whether it is behind cannot be read without the network"
                ),
                act: "fetch origin",
            }),
        }
    }

    /// The URL git reaches origin at in `path` (`ls-remote --get-url origin`: origin's URL as
    /// any `url.*.insteadOf` rewrites it), read once a record.
    ///
    /// # Errors
    /// The read could not run, or failed.
    pub(crate) fn effective_url(
        &self,
        sys: &dyn System,
        user: &User,
        path: &Path,
    ) -> Result<String, String> {
        self.effective
            .get_or_init(|| {
                let ran = run(
                    sys,
                    &git(sys, user, Some(path)).args(["ls-remote", "--get-url", "origin"]),
                    "ls-remote",
                )?;
                if !ran.success() {
                    return Err("git ls-remote failed in the checkout".into());
                }
                Ok(stdout(&ran))
            })
            .clone()
    }

    /// What commit `revision` of this repository declares of the gitlink at `path`, its pinned
    /// commit and its declared URL, where a reader of this record kept them
    /// ([`Self::keep_gitlink`]).
    pub(crate) fn gitlink(&self, revision: &str, path: &str) -> Option<(String, String)> {
        self.gitlinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(revision.to_owned(), path.to_owned()))
            .cloned()
    }

    /// Keep what commit `revision` declares of the gitlink at `path`, `pin` and `url`, for the
    /// next reader of this record. A commit's tree is what its name says, so what it declares
    /// stands while the record does: a replacement (a `refs/replace` ref, `info/grafts`), the
    /// way git reads a commit's name as another tree, changes a file the record watches
    /// ([`layout`]), as does `core.useReplaceRefs` written in one of its config files, and the
    /// next [`Self::of`] reads a record that keeps none.
    pub(crate) fn keep_gitlink(&self, revision: &str, path: &str, pin: &str, url: &str) {
        self.gitlinks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                (revision.to_owned(), path.to_owned()),
                (pin.to_owned(), url.to_owned()),
            );
    }

    /// The files the facts come from, whose metadata [`Self::of`] compares before it reuses
    /// them.
    #[cfg(test)]
    pub(crate) fn watch(&self) -> &[PathBuf] {
        &self.watch
    }

    /// Whether a later [`Self::of`] may reuse this record.
    #[cfg(test)]
    pub(crate) fn reusable(&self) -> bool {
        self.seen.is_some()
    }
}

/// The files of a checkout whose `.git` directory is `admin` that its facts come from, and the
/// config files of `home`'s account and the system that git reads wherever it runs.
fn layout(admin: &Path, home: &Path) -> Vec<PathBuf> {
    let mut watch = vec![admin.to_owned()];
    for name in [
        "HEAD",
        "config",
        "config.worktree",
        "commondir",
        "index",
        "packed-refs",
        "shallow",
        "objects/info",
        "objects/info/alternates",
        "refs",
        "refs/heads",
        "refs/remotes",
        "refs/remotes/origin",
        "refs/remotes/origin/HEAD",
        "refs/replace",
        "info/grafts",
        "reftable",
        "reftable/tables.list",
    ] {
        watch.push(admin.join(name));
    }
    watch.extend(config_files(home));
    watch
}

/// The config files git reads wherever it runs: the system's (`/etc/gitconfig`) and those of
/// `home`'s account (`~/.gitconfig`, `~/.config/git/config`).
pub(crate) fn config_files(home: &Path) -> [PathBuf; 3] {
    [
        PathBuf::from("/etc/gitconfig"),
        home.join(".gitconfig"),
        home.join(".config/git/config"),
    ]
}

/// The files the git at `program` reads its settings from beside [`config_files`], and the
/// program: its build's own under its prefix (`etc/gitconfig`, the system file of a git built
/// with a prefix of its own, as aterm's managed builds are, and `share/git-core/gitconfig`, which
/// the Command Line Tools' git reads), named both as `program` names them and where it resolves,
/// which is how a git that finds its prefix from its own path names them.
pub(crate) fn program_files(sys: &dyn System, program: &Path) -> Vec<PathBuf> {
    let real = sys.canonicalize(program).ok();
    let mut files = Vec::new();
    for program in std::iter::once(program).chain(real.as_deref().filter(|real| *real != program)) {
        if let Some(prefix) = program.parent().and_then(Path::parent) {
            files.push(prefix.join("etc/gitconfig"));
            files.push(prefix.join("share/git-core/gitconfig"));
        }
        files.push(program.to_owned());
    }
    files
}

/// The loose file of `reference` in `admin`, and each folder on the way to it.
fn ref_paths(admin: &Path, reference: &str) -> Vec<PathBuf> {
    let mut at = admin.to_owned();
    reference
        .split('/')
        .map(|part| {
            at.push(part);
            at.clone()
        })
        .collect()
}

/// A digest of what each of `watch` is now: where it resolves, and its owner, mode, kind,
/// identity and change times there, or why it cannot be observed. A record of what was read
/// from those files is reused only while this stands as it did before the read: this record's
/// ([`Facts::of`]) and the credential route's ([`crate::git_credential::Records`]).
pub(crate) fn observe(sys: &dyn System, watch: &[PathBuf]) -> [u8; 32] {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm checkout facts/1\0");
    let mut field = |bytes: &[u8]| {
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    };
    for path in watch {
        field(path.as_os_str().as_encoded_bytes());
        match sys.canonicalize(path) {
            Ok(real) => {
                field(real.as_os_str().as_encoded_bytes());
                match sys.scoped_path_metadata(&real) {
                    Ok(seen) => {
                        field(&seen.version);
                        field(format!("{:?}", seen.stat).as_bytes());
                    }
                    Err(error) => field(format!("{:?}", error.kind()).as_bytes()),
                }
            }
            Err(error) => field(format!("unresolved {:?}", error.kind()).as_bytes()),
        }
    }
    digest.finalize()
}

/// What the symbolic-ref file at `file` says, where it is a regular file (no link) and says
/// one thing: none where it cannot be read or says something else. Its owner and mode are no
/// part of what git reads in it.
fn ref_file(sys: &dyn System, file: &Path) -> Option<RefFile> {
    let stat = sys.stat(file).ok()?;
    if stat.dir || stat.symlink || stat.len > REF_FILE_BYTES as u64 {
        return None;
    }
    let bytes = sys.read_file(file).ok()?;
    if bytes.len() > REF_FILE_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let line = text.strip_suffix('\n').unwrap_or(text);
    if line.contains(['\n', '\r', '\0']) {
        return None;
    }
    if let Some(reference) = line.strip_prefix("ref: ") {
        return plain_ref(reference).then(|| RefFile::Symbolic(reference.to_owned()));
    }
    oid(line).then(|| RefFile::Direct(line.to_owned()))
}

/// Whether `reference` is a ref name a read may use as a pattern and a path: under `refs/`,
/// each part non-empty and not starting with `.`, with no character git refuses in a ref name
/// or reads as a pattern.
fn plain_ref(reference: &str) -> bool {
    reference.starts_with("refs/")
        && !reference.contains("@{")
        && reference.split('/').all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && !part.ends_with(".lock")
                && !part.contains("..")
                && part.bytes().all(|byte| {
                    byte > b' '
                        && byte != 0x7f
                        && !matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
                })
        })
}

/// HEAD as git reads it, where its file cannot say: `symbolic-ref`, and for a detached HEAD
/// whether it names a commit here.
fn ask_head(sys: &dyn System, user: &User, path: &Path) -> Result<Head, String> {
    let symbolic = run(
        sys,
        &git(sys, user, Some(path)).args(["symbolic-ref", "--quiet", "HEAD"]),
        "symbolic-ref",
    )?;
    match symbolic.status {
        Some(0) => {
            let reference = stdout(&symbolic);
            if plain_ref(&reference) {
                Ok(Head::Symbolic(reference))
            } else {
                Err("the checkout's HEAD names no readable ref".into())
            }
        }
        Some(1) => Ok(Head::Detached(commit_here(sys, user, path, "HEAD")?)),
        _ => Err("cannot read the checkout's HEAD".into()),
    }
}

/// The commit `object` (a detached HEAD's file's object, or `HEAD` itself) names, where it is
/// one this repository has (`rev-parse --verify --quiet <object>^{commit}`, which with
/// [`git`]'s `GIT_NO_LAZY_FETCH=1` never fetches one a partial clone lacks).
fn commit_here(
    sys: &dyn System,
    user: &User,
    path: &Path,
    object: &str,
) -> Result<Option<String>, String> {
    let ran = run(
        sys,
        &git(sys, user, Some(path)).args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{object}^{{commit}}"),
        ]),
        "rev-parse",
    )?;
    match ran.status {
        Some(0) if oid(&stdout(&ran)) => Ok(Some(stdout(&ran))),
        Some(1) => Ok(None),
        _ => Err("cannot read the checkout's HEAD".into()),
    }
}

/// `refs/remotes/origin/HEAD` as git reads it, where its file cannot say.
fn ask_origin_head(sys: &dyn System, user: &User, path: &Path) -> Result<Option<String>, String> {
    let ran = run(
        sys,
        &git(sys, user, Some(path)).args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]),
        "symbolic-ref",
    )?;
    match ran.status {
        Some(0) => Ok(Some(stdout(&ran))),
        Some(1) => Ok(None),
        _ => Err("cannot read the checkout's refs/remotes/origin/HEAD".into()),
    }
}

/// Each entry of a `config --null --show-scope --show-origin` listing: none where the listing
/// is not one.
fn entries(listing: &[u8]) -> Option<Vec<Entry>> {
    let mut fields = listing.split(|byte| *byte == 0);
    let mut entries = Vec::new();
    loop {
        let scope = fields.next()?;
        // The listing ends with a NUL: the empty field after it is the last.
        if scope.is_empty() {
            return fields.next().is_none().then_some(entries);
        }
        let origin = fields.next()?;
        let pair = fields.next()?;
        let (key, value) = match pair.iter().position(|byte| *byte == b'\n') {
            Some(at) => (pair[..at].to_vec(), Some(pair[at + 1..].to_vec())),
            None => (pair.to_vec(), None),
        };
        entries.push(Entry {
            scope: scope.to_vec(),
            origin: origin.to_vec(),
            key,
            value,
        });
    }
}

/// Whether `key` includes another file (`include.path`, `includeIf.<condition>.path`).
fn include(key: &[u8]) -> bool {
    key == b"include.path" || (key.starts_with(b"includeif.") && key.ends_with(b".path"))
}

/// Whether the config listing's `entry`, read in `path`, is bound to the files `watch` names, so
/// a record that holds it may be reused: it was read from one of them, not in the command
/// line's scope, and it includes no file but one of them ([`included`]).
fn bound(path: &Path, entry: &Entry, watch: &[PathBuf]) -> bool {
    entry.scope != b"command"
        && config_file(path, &entry.origin).is_some_and(|file| watch.contains(&file))
        && (!include(&entry.key)
            || included(entry).is_some_and(|file| watch.iter().any(|kept| kept == file)))
}

/// The file an unconditional include (`include.path`) names, where its value is an absolute
/// path, which git reads as it is: none for any other entry, for a conditional include
/// (`includeIf.<condition>.path`), whose condition a record cannot judge, and for a path git
/// resolves against something else (`~/`, `%(prefix)/`, a path relative to the file that names
/// it).
fn included(entry: &Entry) -> Option<&Path> {
    if entry.key != b"include.path" {
        return None;
    }
    let file = Path::new(std::str::from_utf8(entry.value.as_deref()?).ok()?);
    file.is_absolute().then_some(file)
}

/// The file an entry read from `origin` is in, a relative one under `path` (where git ran):
/// none for an entry read from no file.
fn config_file(path: &Path, origin: &[u8]) -> Option<PathBuf> {
    let file = std::str::from_utf8(origin.strip_prefix(b"file:")?).ok()?;
    let file = Path::new(file);
    Some(if file.is_absolute() {
        file.to_owned()
    } else {
        path.join(file)
    })
}

/// Each of `wanted` a `for-each-ref` listing names: its object and whether that is a commit.
fn parse_refs(listing: &[u8], wanted: &[String]) -> Option<BTreeMap<String, (String, bool)>> {
    let text = std::str::from_utf8(listing).ok()?;
    let mut refs = BTreeMap::new();
    for line in text.lines() {
        let (object, rest) = line.split_once(' ')?;
        let (kind, name) = rest.split_once(' ')?;
        if !oid(object) {
            return None;
        }
        if wanted.iter().any(|reference| reference == name) {
            refs.insert(name.to_owned(), (object.to_owned(), kind == "commit"));
        }
    }
    Some(refs)
}

#[cfg(test)]
#[path = "facts_tests.rs"]
pub(crate) mod tests;
