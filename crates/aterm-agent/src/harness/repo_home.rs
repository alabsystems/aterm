// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The checkouts aterm's setup clones into a home ([`read_home`]): each `~/<name>` whose own
//! `.git` folder's `origin` is `github.com/<owner>/<name>`, as the repository stage clones every
//! repository it can read. The fleet survey carries what this read last found
//! ([`latest`]), so a dispatcher can place a worker on a Mac that holds the checkout it needs.
//!
//! Every git runs under the repository probe's own rules (no fsmonitor, no hooks, no system or
//! global configuration, no optional locks, no prompt, each read under the hang detector), and the
//! reads that walk history run offline: a survey never fetches, so a checkout's distance behind
//! its origin's main is as of its last fetch, whose time the read gives too ([`HomeRepo::fetched_ms`]).

use std::cell::Cell;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::{
    Diagnostics, Reads, git, git_lossy, git_offline_answer, is_head, main_ref, unattended_dir,
    work_status,
};

/// The most checkouts one read lists ([`HomeRepos::cut`] counts those past it).
pub const HOME_REPOS_MAX: usize = 128;

/// A read older than this is started again when the survey asks for it ([`refresh`]).
pub const REFRESH: Duration = Duration::from_secs(10 * 60);

/// One checkout in the home ([`read_home`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeRepo {
    /// Its folder's name in the home, which is its repository's name on GitHub.
    pub name: String,
    /// Its folder, as the home spells it (`<home>/<name>`).
    pub path: PathBuf,
    /// The commit `HEAD` names, full lowercase hex; `None` where no commit is checked out yet or
    /// it could not be read.
    pub head: Option<String>,
    /// The branch checked out; `None` while `HEAD` is detached or could not be read.
    pub branch: Option<String>,
    /// The remote default branch [`Self::behind`] counts against (`origin/main`); `None` where
    /// it has none.
    pub main_ref: Option<String>,
    /// Commits on [`Self::main_ref`] that `HEAD` lacks, as of the last fetch; `None` where either
    /// is missing or git could not count them.
    pub behind: Option<u32>,
    /// When its origin's main was last fetched, in Unix milliseconds: `FETCH_HEAD`'s time where
    /// it records that branch from `origin` (a fetch of another remote or branch says nothing of
    /// it), or where that is older or missing the last change of [`Self::main_ref`]'s reflog (a
    /// fetch that brought nothing new writes none, so this is never later than the fetch).
    /// `None` where neither is there.
    pub fetched_ms: Option<u64>,
    /// Whether its working tree changes or adds a file git does not ignore; `None` where its
    /// status could not be read.
    pub dirty: Option<bool>,
}

/// What one read of a home found ([`read_home`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeRepos {
    /// When the read ended, in Unix milliseconds.
    pub read_ms: u64,
    /// The checkouts, by name.
    pub repos: Vec<HomeRepo>,
    /// The folders with a `.git` of their own past [`HOME_REPOS_MAX`], not read: a name the read
    /// does not list may be one of them.
    pub cut: usize,
}

impl HomeRepos {
    /// The checkout named `name` (as APFS compares a name: ignoring ASCII case).
    #[must_use]
    pub fn named(&self, name: &str) -> Option<&HomeRepo> {
        self.repos
            .iter()
            .find(|repo| repo.name.eq_ignore_ascii_case(name))
    }
}

/// Whether `name` is one GitHub could name a repository by, and one a read looks into: ASCII
/// letters, digits, `-`, `_` and `.`, at most 100, not hidden (a folder whose name starts with a
/// dot is never read, `~/.Trash` among them).
#[must_use]
pub fn repo_name_ok(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// The owner and name of the GitHub repository `url` names, as an `origin` spells it
/// (`https://github.com/<owner>/<name>[.git]`, or over SSH) or a `FETCH_HEAD` line does (git
/// writes the latter with no user and no `.git`: `github.com:<owner>/<name>` for one over SSH),
/// where it names one with no credential in it.
fn github_repo(url: &str) -> Option<(&str, &str)> {
    let url = url.trim().trim_end_matches('/');
    let path = [
        "https://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
        "ssh://git@github.com:22/",
        "ssh://github.com/",
        "github.com:",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (!owner.is_empty()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && repo_name_ok(name))
    .then_some((owner, name))
}

/// The checkouts aterm's setup clones into `home` ([the module](self)): each folder directly in
/// it (not a link, not hidden, none macOS guards, none on a network, FUSE or automounted volume)
/// holding a `.git` folder of its own (a linked worktree's or a submodule's `.git` file is no
/// clone) whose `origin` names `github.com/<owner>/<that folder's name>`, at most
/// [`HOME_REPOS_MAX`] of them by name. Each git read has `limit` from the end of the one before
/// it, and none starts while `held` says new starts are held back; a field a read could not make
/// is `None`. `None` where `home` is not absolute or, as its links resolve, not on a local volume
/// or in a folder macOS guards, the volume table or the listing could not be read whole, or a
/// folder with a `.git` could not be told a clone or not: a read that cannot say what the home
/// holds says nothing of it.
#[must_use]
pub fn read_home(home: &Path, limit: Duration, held: &dyn Fn() -> bool) -> Option<HomeRepos> {
    if !home.is_absolute() || home.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    // The volume table before any look: a folder on a network, FUSE or automounted volume is
    // never looked into (a look there can wait on a server), and a table read in part cannot say
    // which those are.
    read_home_in(home, &aterm_sysprobe::volumes::table(), limit, held)
}

/// [`read_home`] with the volume table `table` read.
fn read_home_in(
    home: &Path,
    table: &aterm_sysprobe::volumes::Table,
    limit: Duration,
    held: &dyn Fn() -> bool,
) -> Option<HomeRepos> {
    let local = |dir: &Path| {
        aterm_sysprobe::volumes::mount_of(dir, table).is_some_and(|m| m.local && !m.automounted)
    };
    if !table.complete {
        return None;
    }
    // The home as its links resolve, each step judged local before it is looked at: a home that
    // is a link onto a network, FUSE or automounted volume is never listed.
    let real = crate::harness::disk::idle::resolve_local(home, table)?;
    if !local(&real) || atpkg::protected::under_protected_root(home, &real) {
        return None;
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&real).ok()? {
        let entry = entry.ok()?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let dir = home.join(&name);
        if !repo_name_ok(&name)
            || atpkg::protected::under_protected_root(home, &dir)
            || !local(&real.join(&name))
        {
            continue;
        }
        // The listing's own kind, from the home's folder: no look crosses into the entry.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if std::fs::symlink_metadata(dir.join(".git")).is_ok_and(|meta| meta.is_dir()) {
            names.push(name);
        }
    }
    names.sort_unstable();
    let cut = names.len().saturating_sub(HOME_REPOS_MAX);
    names.truncate(HOME_REPOS_MAX);
    // A git that cannot run reads every origin as none: the read could not tell a clone.
    if !names.is_empty() {
        let reads = reads(home, limit, held)?;
        git(home, &["--version"], &reads)?;
    }
    let mut repos = Vec::new();
    for name in &names {
        repos.extend(read_one(home, name, table, limit, held)?);
    }
    Some(HomeRepos {
        read_ms: aterm_time::unix_millis().unwrap_or(0),
        repos,
        cut,
    })
}

/// The checkout `<home>/<name>` ([`read_home`]): `Some(None)` where it is no clone of
/// `github.com/<owner>/<name>`, and `None` where the read cannot say whether the home holds it:
/// its folder did not resolve as one the probe reads ([`unattended_dir`]: [`read_home`] already
/// left out those it never enters, so this one changed under the read or could not be resolved),
/// or its `origin` could not be read (a read the hang detector cut, or one `held` stopped).
fn read_one(
    home: &Path,
    name: &str,
    table: &aterm_sysprobe::volumes::Table,
    limit: Duration,
    held: &dyn Fn() -> bool,
) -> Option<Option<HomeRepo>> {
    let reads = reads(home, limit, held)?;
    let path = home.join(name);
    let root = unattended_dir(&path, Some(home), table)?;
    let origin = git(&root, &["config", "--get", "remote.origin.url"], &reads);
    if reads.cut.get() {
        return None;
    }
    let Some(origin) = origin
        .as_deref()
        .and_then(github_repo)
        .filter(|(_, named)| named.eq_ignore_ascii_case(name))
    else {
        return Some(None);
    };
    let head = git(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], &reads)
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| is_head(h));
    let branch = git_lossy(
        &root,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        &reads,
        Diagnostics::Permit,
    )
    .map(|b| b.trim().to_string())
    .filter(|b| !b.is_empty() && !b.contains('`') && !b.to_ascii_lowercase().contains(FLEET_CODE));
    let main = main_ref(&root, &reads);
    let main_head = main
        .as_deref()
        .filter(|full| full.starts_with("refs/"))
        .and_then(|full| {
            git_offline_answer(&root, &["rev-parse", "--verify", "--quiet", full], &reads)
        })
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| is_head(h));
    let behind = match (&head, main.as_deref()) {
        (Some(_), Some(full)) => git_offline_answer(
            &root,
            &["rev-list", "--count", &format!("HEAD..{full}")],
            &reads,
        )
        .and_then(|n| crate::parse_uint::<u32>(n.trim())),
        _ => None,
    };
    let dirty = work_status(&root, table, &reads).map(|(status, _)| {
        status
            .split('\0')
            .any(|entry| entry.len() > 3 && !entry.starts_with("!!"))
    });
    Some(Some(HomeRepo {
        name: name.to_owned(),
        fetched_ms: fetched_ms(
            &root.join(".git"),
            main.as_deref(),
            main_head.as_deref(),
            origin,
        ),
        path,
        head,
        branch,
        main_ref: main.map(|full| {
            full.strip_prefix("refs/remotes/")
                .unwrap_or(&full)
                .to_owned()
        }),
        behind,
        dirty,
    }))
}

/// One checkout's reads under `home`, each with `limit` from the end of the one before it, none
/// while `held` ([`Reads`]).
fn reads<'a>(home: &'a Path, limit: Duration, held: &'a dyn Fn() -> bool) -> Option<Reads<'a>> {
    Some(Reads {
        deadline: Cell::new(Instant::now().checked_add(limit)?),
        each: Some(limit),
        home,
        xdg: None,
        cut: Cell::new(false),
        starts: Cell::new(0),
        held: Some(held),
        #[cfg(test)]
        refused: None,
    })
}

/// A fleet join code's prefix: a branch named with one, or with a code span, is not said.
const FLEET_CODE: &str = "atf1-";

/// The most of a `FETCH_HEAD` [`fetched_ms`] reads: one past it is no record of a fetch it reads.
const FETCH_HEAD_MAX: u64 = 1024 * 1024;

/// The most of a reflog's end [`logged_ms`] reads for its last entry.
const REFLOG_TAIL: u64 = 64 * 1024;

/// A time on this Mac's clock in milliseconds since 1970, `None` before it.
fn unix_ms(at: std::time::SystemTime) -> Option<u64> {
    let since = at.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// When `origin`'s (owner, name) main ref `main` (in full, `refs/remotes/origin/main`, whose
/// commit is now `main_head`) was last fetched into the repository whose git folder is
/// `git_dir` ([`HomeRepo::fetched_ms`]): the later of the main ref's last reflog entry
/// ([`logged_ms`]) and a `FETCH_HEAD` that records that ref's commit fetched from `origin`
/// (a fetch that found nothing new writes the latter alone).
fn fetched_ms(
    git_dir: &Path,
    main: Option<&str>,
    main_head: Option<&str>,
    origin: (&str, &str),
) -> Option<u64> {
    let main = main.filter(|full| {
        full.starts_with("refs/")
            && Path::new(full)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
    });
    let reflog = main.and_then(|full| logged_ms(&git_dir.join("logs").join(full)));
    let branch = main.and_then(|full| full.strip_prefix("refs/remotes/origin/"));
    let fetch_head = branch.zip(main_head).and_then(|(branch, head)| {
        use std::io::Read as _;
        let (file, meta) = aterm_uds::regular_file::open(&git_dir.join("FETCH_HEAD")).ok()?;
        if meta.len() > FETCH_HEAD_MAX {
            return None;
        }
        let mut text =
            String::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
        file.take(FETCH_HEAD_MAX).read_to_string(&mut text).ok()?;
        fetch_head_names(&text, branch, head, origin).then(|| unix_ms(meta.modified().ok()?))?
    });
    fetch_head.max(reflog)
}

/// When the last entry of the reflog at `path` was written: the time the entry records
/// (`<old> <new> <name> <<email>> <seconds> <zone>\t<message>`), never later than the file's
/// own time. A rewrite (`git reflog expire`) moves the file's time and keeps the entries'; an
/// entry a `GIT_COMMITTER_DATE` dated ahead moves its own and not the file's.
fn logged_ms(path: &Path) -> Option<u64> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (mut file, meta) = aterm_uds::regular_file::open(path).ok()?;
    let from = meta.len().saturating_sub(REFLOG_TAIL);
    file.seek(SeekFrom::Start(from)).ok()?;
    let cap = usize::try_from(meta.len().min(REFLOG_TAIL).saturating_add(1)).unwrap_or(0);
    let mut tail = Vec::with_capacity(cap);
    file.take(REFLOG_TAIL).read_to_end(&mut tail).ok()?;
    let mut tail = tail.as_slice();
    if from > 0 {
        // The read began inside an entry: only one after its first line end is whole.
        let end = tail.iter().position(|&b| b == b'\n')?;
        tail = &tail[end + 1..];
    }
    let line = tail
        .split(|&b| b == b'\n')
        .rev()
        .find(|line| !line.is_empty())?;
    let who = line.split(|&b| b == b'\t').next()?;
    let mut words = who.rsplit(|&b| b == b' ');
    let zone = words.next()?;
    let seconds = words.next()?;
    let zone_ok = zone.len() == 5
        && matches!(zone[0], b'+' | b'-')
        && zone[1..].iter().all(u8::is_ascii_digit);
    if !zone_ok || seconds.is_empty() || !seconds.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let logged = std::str::from_utf8(seconds)
        .ok()?
        .parse::<u64>()
        .ok()?
        .checked_mul(1000)?;
    Some(logged.min(unix_ms(meta.modified().ok()?)?))
}

/// Whether a `FETCH_HEAD`'s `text` records `branch` at commit `head` fetched from the GitHub
/// repository `origin` (owner, name): a line `<head>\t[not-for-merge]\tbranch '<branch>' of
/// <url>` whose url names it.
fn fetch_head_names(text: &str, branch: &str, head: &str, origin: (&str, &str)) -> bool {
    let said = format!("branch '{branch}' of ");
    text.lines().any(|line| {
        let mut fields = line.splitn(3, '\t');
        fields
            .next()
            .is_some_and(|sha| sha.eq_ignore_ascii_case(head))
            && fields
                .nth(1)
                .and_then(|what| what.strip_prefix(&said))
                .and_then(github_repo)
                .is_some_and(|(owner, name)| {
                    owner.eq_ignore_ascii_case(origin.0) && name.eq_ignore_ascii_case(origin.1)
                })
    })
}

/// The last read of a home that finished, and the home it read ([`latest`]).
static LATEST: Mutex<Option<(PathBuf, HomeRepos)>> = Mutex::new(None);

/// Whether a read started by [`refresh`] runs now.
static READING: AtomicBool = AtomicBool::new(false);

/// The last read of `home` that finished in this process ([`refresh`] starts them), `None`
/// before one has.
#[must_use]
pub fn latest(home: &Path) -> Option<HomeRepos> {
    LATEST
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|(read, _)| read == home)
        .map(|(_, repos)| repos.clone())
}

/// Start a read of `home` ([`read_home`], each git under the hang detector, none while `held`)
/// beside the caller, where none runs and [`latest`] has none of it read within [`REFRESH`]. A
/// read that could not say what the home holds leaves the last one standing.
pub fn refresh(home: &Path, held: fn() -> bool) {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let refresh_ms = u64::try_from(REFRESH.as_millis()).unwrap_or(u64::MAX);
    // A read the clock now puts in the future is no recent read: its clock stepped back.
    if latest(home).is_some_and(|last| {
        now.checked_sub(last.read_ms)
            .is_some_and(|age| age < refresh_ms)
    }) {
        return;
    }
    if READING.swap(true, Ordering::AcqRel) {
        return;
    }
    /// Lets the next read start however this one ends, a panic included.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            READING.store(false, Ordering::Release);
        }
    }
    let home = home.to_path_buf();
    let started = std::thread::Builder::new()
        .name("home-repos".to_owned())
        .spawn(move || {
            let _done = Done;
            if let Some(read) = read_home(&home, super::PROBE_LIMIT, &held) {
                *LATEST.lock().unwrap_or_else(PoisonError::into_inner) = Some((home, read));
            }
        });
    if started.is_err() {
        READING.store(false, Ordering::Release);
    }
}

/// The lock every test that reads or writes this process's one [`LATEST`] or [`READING`] holds,
/// in any crate, so one test's read never replaces another's while it looks.
#[cfg(any(test, debug_assertions))]
pub fn one_test_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static REPO_READ_TESTS: Mutex<()> = Mutex::new(());
    REPO_READ_TESTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Whether a read [`refresh`] started runs now: a test waits this out before it gives the last
/// read an age ([`plant_latest`]), since a running read replaces it when it ends.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn reading_now() -> bool {
    READING.load(Ordering::Acquire)
}

/// Make `read` the last read of `home` that finished, as a read does ([`latest`]): a test's way
/// to give the last read an age.
#[cfg(any(test, debug_assertions))]
pub fn plant_latest(home: &Path, read: HomeRepos) {
    *LATEST.lock().unwrap_or_else(PoisonError::into_inner) = Some((home.to_path_buf(), read));
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "repo_home_tests.rs"]
mod tests;
