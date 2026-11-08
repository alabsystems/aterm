// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clone missing GitHub repositories into `~` and fast-forward clean worktrees that are behind.

use super::{Github, auth_id, auth_ready, get};
use crate::checkout::{AcquisitionFailure, Origin, Stands, git, github_origin as origin_matches};
use crate::github_pages::Page;
use crate::init_observe::{LastReading, Reading};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, FileId, Scratch, Secret, System};
use aterm_capacity::admit::gib;
use aterm_capacity::reading::Volume;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PAGE_SIZE: usize = 30;
const CLONE_BUDGET: Duration = Duration::from_secs(1800);

/// What every clone's staging directory in a home is named after (`Scratch::new_in`'s prefix
/// for the label `github-clone`).
const STAGING: &str = ".aterm-machines-github-clone-";

/// The mark a clone holds in its staging while it runs ([`clone_one_with`]): a flock, which the
/// kernel lets go when the process ends however it ends.
const OWNER: &str = ".owner";

/// The name [`OWNER`] is locked under before it is renamed into place.
const MARKING: &str = ".owner.new";

pub(super) fn item(user: &User, config: &Github, recorded: Vec<PathBuf>) -> Box<dyn Item> {
    Box::new(Repositories {
        user: user.clone(),
        config: Github {
            account: config.account.clone(),
            owner: config.owner.clone(),
            token_file: config.token_file.clone(),
            token_key: config.token_key.clone(),
        },
        recorded,
        hooks: std::sync::Mutex::new(Vec::new()),
        reading: LastReading::default(),
    })
}

struct Repositories {
    user: User,
    config: Github,
    recorded: Vec<PathBuf>,
    /// What the last apply's clones made of their hook installers ([`Cloned::hooks`]), which
    /// the read-back's row says.
    hooks: std::sync::Mutex<Vec<String>>,
    reading: LastReading,
}

struct Scan {
    /// Every selected repository, including a destination that must stay untouched.
    all: usize,
    missing: Vec<Repo>,
    existing: Vec<(Repo, PathBuf, Stands)>,
    dependencies: Vec<(Repo, PathBuf)>,
    recorded: usize,
    /// Repositories excluded by the inventory's folder-selection policy.
    notes: Vec<String>,
    /// Selected destinations that remain unfinished while preserving their existing contents.
    blocked: Vec<String>,
}

const NAMED: usize = 4;

impl Scan {
    fn moving(&self) -> usize {
        self.existing
            .iter()
            .filter(|(_, _, stands)| stands.moves())
            .count()
    }

    fn kept(&self) -> Option<String> {
        let mut kept = self.notes.clone();
        kept.extend(self.blocked.iter().cloned());
        kept.extend(
            self.existing
                .iter()
                .filter_map(|(repo, _, stands)| match stands {
                    Stands::Kept(why) => Some(format!("~/{}: {why}", repo.name)),
                    _ => None,
                }),
        );
        named(&kept)
    }
}

impl Item for Repositories {
    fn id(&self) -> String {
        format!("init.github.repositories@{}", self.user.name)
    }
    fn family(&self) -> &'static str {
        "github"
    }
    fn title(&self) -> String {
        format!(
            "Every accessible github.com/{} repository cloned into ~",
            self.config.owner
        )
    }
    fn after(&self) -> Vec<String> {
        vec![auth_id(&self.user)]
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        // What the last apply's clones made of their hook installers: whatever this read says,
        // the switch turned off or the profile changed since included, its row says that too.
        let hooks = std::mem::take(
            &mut *self
                .hooks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let read = (|| -> Probe {
            self.reading.set(None);
            if matches!(super::settings_managed(sys, &self.user), Ok(false)) {
                return Probe::NotApplicable {
                    why: "repository setup is disabled in init.toml".into(),
                };
            }
            match super::source::Stamp::matches_config(sys, &self.user, &self.config) {
                Ok(true) => {}
                Ok(false) => {
                    return Probe::Blocked {
                        why: "the configured repository profile changed before probing".into(),
                    };
                }
                Err(why) => return Probe::Unusable { why },
            }
            let token = match auth_ready(sys, &self.user, &self.config) {
            Err(why) => return Probe::Unusable { why },
            Ok(None) => return Probe::Waits {
                on: auth_id(&self.user),
                why: "enumerate private and public repositories after GitHub authentication is configured".into(),
            },
            Ok(Some(token)) => token,
        };
            let scan = match self.scan(sys, &token) {
                Err(why) => return Probe::Unusable { why },
                Ok(scan) => scan,
            };
            let present = scan.existing.len() + scan.recorded;
            self.reading.set(Some(Reading::Repositories {
                present,
                count: scan.all,
                signed_in: true,
            }));
            if scan.missing.is_empty() && scan.dependencies.is_empty() && scan.moving() == 0 {
                if let Some(why) = named(&scan.blocked) {
                    return Probe::Person {
                        step: "github.repositories".into(),
                        how: format!(
                            "{present} of {} repositories ready; preserved destinations block completion: {why}",
                            scan.all,
                        ),
                    };
                }
                let mut now = format!("{} matching worktrees present", scan.all);
                if scan.recorded > 0 {
                    now.push_str(&format!(
                        "; {} recorded in [develop], which the develop items align",
                        scan.recorded
                    ));
                }
                match scan.kept() {
                    Some(kept) => now.push_str(&format!("; left as they are: {kept}")),
                    None => now.push_str("; the others up to date with GitHub"),
                }
                Probe::Ok { now }
            } else {
                Probe::Change {
                    from: format!(
                        "{} of {} repositories missing from ~{}; {} clean worktree(s) behind GitHub{}",
                        scan.missing.len(),
                        scan.all,
                        unfit(sys, &self.user, &scan.missing),
                        scan.moving(),
                        if scan.dependencies.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "; {} checkout(s) need required dependencies",
                                scan.dependencies.len()
                            )
                        }
                    ),
                    to: "clone missing repositories with full history; fast-forward each clean \
                     worktree that is behind; preserve every other worktree"
                        .into(),
                }
            }
        })();
        with_hooks(read, &hooks)
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }

    fn apply_guarded(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        let source = super::source::Stamp::read(sys, &self.user)?;
        let current = || {
            authorized()?;
            if !super::settings_managed(sys, &self.user)? {
                return Err("repository setup was disabled in init.toml while cloning".into());
            }
            if !super::source::Stamp::matches_config(sys, &self.user, &self.config)? {
                return Err("the configured repository profile changed before cloning".into());
            }
            if !source.is_current(sys, &self.user)? {
                return Err("repository input source changed while cloning".into());
            }
            Ok(())
        };
        current()?;
        let Some(token) = auth_ready(sys, &self.user, &self.config)? else {
            return Err("GitHub authentication changed; no repositories cloned".into());
        };
        let scan = self.scan(sys, &token)?;
        let home = Path::new(&self.user.home);
        let guard = CloneGuard::new(&current);
        let Cloned {
            left,
            mut failed,
            hooks,
            ..
        } = clone_all(sys, &self.user, &scan.missing, Some(&guard));
        let room = || short_of_room(sys.volume(home).as_ref(), &left, INIT_AGAIN);
        // A failed row is read back by nothing: it says what the clones that went made of their
        // hook installers, after why it failed, a guard that stopped the rest included; a done
        // one leaves that to the read-back's row.
        if let Err(why) = current() {
            return Err(stopped_after_clones(why, failed, room(), &hooks));
        }
        for (repo, path) in scan.dependencies {
            if let Err(why) = current() {
                return Err(stopped_after_clones(why, failed, room(), &hooks));
            }
            if let Err(why) = super::dependencies::repair(sys, &self.user, &path, &repo, &current) {
                failed.push(format!("~/{}: {why}", repo.name));
            }
        }
        for (repo, path, stands) in scan.existing {
            if !stands.moves() {
                continue;
            }
            if let Err(why) = current() {
                return Err(stopped_after_clones(why, failed, room(), &hooks));
            }
            if let Err(unread) =
                crate::checkout::align_guarded(sys, &self.user, &path, &repo.origin(), &current)
            {
                failed.push(format!("~/{}: {}", repo.name, unread.why));
            }
        }
        let short = room();
        if failed.is_empty() && short.is_none() {
            *self
                .hooks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = hooks;
            return Ok(());
        }
        // A failure the guard caused (the last step it stopped) says why it stopped first.
        if let Err(why) = current() {
            return Err(stopped_after_clones(why, failed, short, &hooks));
        }
        failed.extend(short);
        failed.extend(hooks);
        Err(failed.join("; "))
    }
}

/// How many of `missing` the disk holding `user`'s home has no room for now ([`no_room`]), as
/// the plan adds it; empty when every one fits or the volume cannot be read.
fn unfit(sys: &dyn System, user: &User, missing: &[Repo]) -> String {
    let volume = sys.volume(Path::new(&user.home));
    match missing
        .iter()
        .filter(|repo| no_room(volume.as_ref(), repo).is_some())
        .count()
    {
        0 => String::new(),
        unfit => format!(", {unfit} of them with no room on the disk now"),
    }
}

impl Repositories {
    fn scan(&self, sys: &dyn System, token: &Secret) -> Result<Scan, String> {
        admit_home(sys, &self.user)?;
        let listed = enumerate(sys, &self.user, &self.config, token)?;
        let Partition {
            missing,
            present,
            dependencies,
            notes,
            git_waits,
        } = partition(sys, &self.user, &listed.repositories)?;
        if let Some(why) = named(&git_waits) {
            return Err(why);
        }
        let mut aligned = Vec::new();
        let mut recorded = 0;
        for (repo, path) in present {
            if self.recorded.contains(&path) {
                recorded += 1;
                continue;
            }
            let stands = crate::checkout::read(sys, &self.user, &path, &repo.origin())
                .unwrap_or_else(|unread| Stands::Kept(unread.why));
            aligned.push((repo, path, stands));
        }
        Ok(Scan {
            all: listed.repositories.len(),
            missing,
            existing: aligned,
            dependencies,
            recorded,
            notes: listed.notes,
            blocked: notes,
        })
    }
}

pub(super) fn aterm_checkouts(
    sys: &dyn System,
    user: &User,
    config: &Github,
    token: &Secret,
) -> Result<Vec<PathBuf>, String> {
    admit_home(sys, user)?;
    let listed = enumerate(sys, user, config, token)?;
    Ok(partition(sys, user, &listed.repositories)?
        .present
        .into_iter()
        .filter(|(_, path)| {
            crate::init_develop::workspace(sys, user, path).is_ok_and(|found| found.is_some())
        })
        .map(|(_, path)| path)
        .collect())
}

/// Where each listed repository stands in `~`: missing, present as its own worktree, or left
/// untouched for the reason its note gives (a folder in the way, another origin, a standard
/// home folder). A present repository includes all explicitly required dependencies; a
/// matching parent whose gitlinks need work remains unfinished in `dependencies`.
#[derive(Clone, Debug, Default)]
pub(super) struct Partition {
    pub(super) missing: Vec<Repo>,
    pub(super) present: Vec<(Repo, PathBuf)>,
    pub(super) dependencies: Vec<(Repo, PathBuf)>,
    pub(super) notes: Vec<String>,
    /// Dependencies whose local inspection waits for a capable Git, without touching them.
    pub(super) git_waits: Vec<String>,
}

/// `lines` joined, the first [`NAMED`] of them and how many more; `None` when there are none.
pub(super) fn named(lines: &[String]) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    let more = lines.len().saturating_sub(NAMED);
    let mut line = lines
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if more > 0 {
        line.push_str(&format!("; and {more} more"));
    }
    Some(line)
}

/// The folders macOS keeps in every home, several behind a privacy consent: a repository named
/// like one (in any case) is never cloned there, and git never runs in it.
pub(super) const HOME_FOLDERS: [&str; 9] = [
    "Applications",
    "Desktop",
    "Documents",
    "Downloads",
    "Library",
    "Movies",
    "Music",
    "Pictures",
    "Public",
];

pub(super) fn partition(sys: &dyn System, user: &User, all: &[Repo]) -> Result<Partition, String> {
    if all.is_empty() {
        admit_destination(sys, user)?;
    } else {
        admit_home(sys, user)?;
    }
    let home = Path::new(&user.home);
    let mut standing = Partition::default();
    for repo in all {
        if HOME_FOLDERS
            .iter()
            .any(|folder| folder.eq_ignore_ascii_case(&repo.name))
        {
            standing.notes.push(format!(
                "~/{} is a standard home folder; left untouched",
                repo.name
            ));
            continue;
        }
        let path = home.join(&repo.name);
        match sys.stat(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                standing.missing.push(repo.clone());
            }
            Ok(stat) if stat.dir && !stat.symlink => {
                match verify_existing(sys, user, &repo.owner, repo, &path) {
                    Ok(()) => match super::dependencies::ready(sys, user, &path, repo) {
                        Ok(true) => standing.present.push((repo.clone(), path)),
                        Ok(false) => standing.dependencies.push((repo.clone(), path)),
                        Err(super::dependencies::Unready::Git(why)) => {
                            standing.git_waits.push(format!("~/{}: {why}", repo.name));
                        }
                        Err(super::dependencies::Unready::Preserved(why)) => {
                            standing.notes.push(format!("~/{}: {why}", repo.name));
                        }
                    },
                    Err(why) => standing.notes.push(why),
                }
            }
            Ok(_) => standing.notes.push(format!(
                "~/{} is occupied or linked; left untouched",
                repo.name
            )),
            Err(_) => standing
                .notes
                .push(format!("cannot inspect ~/{}; left untouched", repo.name)),
        }
    }
    Ok(standing)
}

pub(super) struct Refreshed {
    pub(super) moved: bool,
    pub(super) failed: Vec<String>,
    pub(super) network_failed: bool,
}

/// Refresh only ordinary, locally eligible checkouts from a previously admitted
/// private inventory. Cached refs establish eligibility, never remote freshness.
/// Every preservation decision precedes a remote read; Git's existing guarded
/// alignment owns the fetch and fast-forward. Up to [`LANES`] independent stores
/// run together; unknown or shared storage keeps the serial path.
pub(super) fn refresh_existing(
    sys: &dyn System,
    user: &User,
    standing: &Partition,
    recorded: &BTreeSet<PathBuf>,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Refreshed, String> {
    struct Job<'a> {
        index: usize,
        repo: &'a Repo,
        path: &'a Path,
        physical: Option<[FileId; 3]>,
    }
    struct Batch<'a> {
        pending: std::collections::VecDeque<Job<'a>>,
        stopped: bool,
        fault: Option<(usize, String)>,
    }
    current()?;
    let mut paths = BTreeSet::new();
    let mut subjects = BTreeSet::new();
    let mut independent = true;
    let mut jobs = std::collections::VecDeque::new();
    for (index, (repo, path)) in standing
        .present
        .iter()
        .chain(&standing.dependencies)
        .enumerate()
    {
        if recorded.contains(path) || !paths.insert(path.clone()) {
            continue;
        }
        let physical = refresh_subjects(sys, path).ok();
        if let Some(physical) = &physical {
            for identity in physical {
                independent &= subjects.insert((identity.dev, identity.ino, identity.born_ns));
            }
        } else {
            independent = false;
        }
        jobs.push_back(Job {
            index,
            repo,
            path,
            physical,
        });
    }
    // Identity is scheduling evidence alone. Unknown or shared storage retains
    // the old serial behavior; no valid checkout is omitted or newly refused.
    let lanes = if independent {
        jobs.len().min(LANES)
    } else {
        1
    };
    if lanes <= 1 {
        for job in &mut jobs {
            job.physical = None;
        }
    }
    let batch = std::sync::Mutex::new(Batch {
        pending: jobs,
        stopped: false,
        fault: None,
    });
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..lanes {
            scope.spawn(|| {
                loop {
                    let job = {
                        let mut batch = batch
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if batch.stopped {
                            return;
                        }
                        let Some(job) = batch.pending.pop_front() else {
                            return;
                        };
                        job
                    };
                    let admitted = || {
                        let checked = current().and_then(|()| {
                            if job.physical.is_some_and(|expected| {
                                refresh_subjects(sys, job.path).ok() != Some(expected)
                            }) {
                                Err(format!(
                                    "~/{}: checkout storage changed during refresh",
                                    job.repo.name
                                ))
                            } else {
                                Ok(())
                            }
                        });
                        if let Err(why) = &checked {
                            let mut batch = batch
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            batch.stopped = true;
                            if batch
                                .fault
                                .as_ref()
                                .is_none_or(|(index, _)| job.index < *index)
                            {
                                batch.fault = Some((job.index, why.clone()));
                            }
                        }
                        checked
                    };
                    let result = refresh_one(sys, user, job.repo, job.path, &admitted);
                    if result.as_ref().is_err_and(|unread| unread.offline) {
                        batch
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .stopped = true;
                    }
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((job.index, job.repo, result));
                }
            });
        }
    });
    let final_current = current();
    if let Some((_, why)) = batch
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .fault
    {
        return Err(why);
    }
    final_current?;
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    results.sort_by_key(|(index, _, _)| *index);
    let mut refreshed = Refreshed {
        moved: false,
        failed: Vec::new(),
        network_failed: false,
    };
    for (_, repo, result) in results {
        match result {
            Ok(Some(Stands::Behind { .. })) => refreshed.moved = true,
            Ok(_) => {}
            Err(unread) => {
                refreshed.network_failed |= unread.network;
                refreshed
                    .failed
                    .push(format!("~/{}: {}", repo.name, unread.why));
            }
        }
    }
    Ok(refreshed)
}

fn refresh_subjects(sys: &dyn System, path: &Path) -> io::Result<[FileId; 3]> {
    Ok([
        sys.file_id(path)?,
        sys.file_id(&path.join(".git"))?,
        sys.file_id(&path.join(".git/objects"))?,
    ])
}

fn refresh_one(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    path: &Path,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Option<Stands>, crate::checkout::Unread> {
    current()?;
    // Linked worktrees and foreign/linked object storage are preserved, as
    // are deliberately shallow or partial clones. Inspect no working files.
    if [
        path.to_owned(),
        path.join(".git"),
        path.join(".git/objects"),
    ]
    .iter()
    .any(|path| {
        !sys.stat(path)
            .is_ok_and(|stat| stat.dir && !stat.symlink && stat.uid == user.uid)
    }) || super::dependencies::complete_storage(sys, user, path).is_err()
    {
        return Ok(None);
    }
    let origin = repo.origin();
    let branch = match crate::checkout::read_fetched(sys, user, path, &origin) {
        Ok(Stands::Kept(_)) | Err(crate::checkout::NotFetched::Unrecorded { .. }) => {
            return Ok(None);
        }
        Err(crate::checkout::NotFetched::Unread(unread)) => return Err(unread),
        Ok(
            Stands::Current { branch, .. }
            | Stands::Behind { branch, .. }
            | Stands::Unfetched { branch, .. },
        ) => branch,
    };
    if !crate::checkout::tracks_origin(sys, user, path, &branch) {
        return Ok(None);
    }
    // At each of align's checks (its start, before its fetch, before its merge): what this
    // read admitted still holds, the storage included (a partial clone made since would fetch
    // lazily in the merge).
    // [`refresh_existing`] names the repository on each failure line; these say what changed.
    let admitted = || {
        current()?;
        if !crate::checkout::tracks_origin(sys, user, path, &branch) {
            return Err("upstream tracking changed during refresh; work preserved".to_owned());
        }
        if super::dependencies::complete_storage(sys, user, path).is_err() {
            return Err(
                "its object storage changed during refresh (now shallow, partial or \
                        borrowed, or unreadable); work preserved"
                    .to_owned(),
            );
        }
        Ok(())
    };
    crate::checkout::align_guarded(sys, user, path, &origin, &admitted).map(Some)
}

/// Admit the account's destination without requiring a tool for an empty selection.
pub(super) fn admit_destination(sys: &dyn System, user: &User) -> Result<(), String> {
    let home = Path::new(&user.home);
    let stat = sys
        .stat(home)
        .map_err(|_| "cannot inspect the repository destination home")?;
    if !home.is_absolute() || !stat.dir || stat.symlink || stat.uid != user.uid {
        return Err("repository destination must be this user's own home directory".into());
    }
    Ok(())
}

pub(super) fn admit_home(sys: &dyn System, user: &User) -> Result<(), String> {
    admit_destination(sys, user)?;
    if !git_installed(sys) {
        return Err(no_git(sys).into());
    }
    Ok(())
}

/// Why [`admit_home`] waits for managed Git or a native Git installation.
pub(super) fn no_git(sys: &dyn System) -> &'static str {
    if sys.macos() {
        "Git is not ready yet; the default toolset installs it automatically, and Apple's \
         Command Line Tools or Xcode also provide Git; no repositories changed"
    } else {
        "neither managed Git nor /usr/bin/git is ready; the distribution's git package can also \
         provide Git; no repositories changed"
    }
}

/// Whether the git every clone runs is installed ([`crate::items::github::installed_git`]).
pub(super) fn git_installed(sys: &dyn System) -> bool {
    crate::items::github::installed_git(sys).is_some()
}

/// The room a clone takes per byte of GitHub's `size`: 54 repositories GitHub sized at 10 GiB
/// took 29 GiB cloned (2026-10-25), one of them 4.7 times its size.
const CLONE_ROOM_PER_SIZE: u64 = 5;

/// What a pass that left repositories for room ([`short_of_room`]) tells the person to do once
/// there is room, as `aterm init` says it.
pub(super) const INIT_AGAIN: &str = "run aterm init again";

/// `repo` as a pass that leaves it uncloned names it, when its clone would take `volume` below
/// the free space the window's disk watch keeps on it ([`Volume::floor`]); `None` when it fits,
/// or when the volume could not be read. A size GitHub gives as `0` (an empty repository, or one
/// it has not sized yet) counts as 1 KiB, so no clone runs on a volume already at its floor.
pub(super) fn no_room(volume: Option<&Volume>, repo: &Repo) -> Option<String> {
    no_room_beside(volume, repo, 0)
}

/// The room [`no_room`] estimates `repo`'s clone takes, in bytes.
fn need(repo: &Repo) -> u64 {
    repo.size_kib
        .max(1)
        .saturating_mul(1024)
        .saturating_mul(CLONE_ROOM_PER_SIZE)
}

/// [`no_room`] beside clones that will take `reserved` bytes of `volume` first.
fn no_room_beside(volume: Option<&Volume>, repo: &Repo, reserved: u64) -> Option<String> {
    let volume = volume?;
    let need = need(repo);
    if need <= volume.above_floor().saturating_sub(reserved) {
        return None;
    }
    Some(if need >= aterm_capacity::floor::GIB / 10 {
        format!("~/{} (about {})", repo.name, gib(need))
    } else {
        format!("~/{}", repo.name)
    })
}

/// What a pass that left `left` uncloned for room ([`no_room`]) says, with `volume`'s free space,
/// the free space it keeps and `then`, what to do once there is room; `None` when it left none.
pub(super) fn short_of_room(
    volume: Option<&Volume>,
    left: &[String],
    then: &str,
) -> Option<String> {
    if left.is_empty() {
        return None;
    }
    let disk = volume.map_or_else(String::new, |volume| {
        format!(
            "; {} has {} free and keeps {} free",
            volume.mount.display(),
            gib(volume.free),
            gib(volume.floor)
        )
    });
    let count = match left.len() {
        1 => "1 repository".to_owned(),
        n => format!("{n} repositories"),
    };
    Some(format!(
        "{count} not cloned, for want of disk space: {}{disk}; free space, then {then}",
        left.join(", ")
    ))
}

#[derive(Clone, Debug)]
pub(super) struct Repo {
    pub(super) owner: String,
    pub(super) name: String,
    pub(super) default_branch: Option<String>,
    /// GitHub's `size` for it in KiB (`0` where GitHub gives none, or has not sized it yet),
    /// which [`no_room`] estimates its clone's room from.
    pub(super) size_kib: u64,
    /// GitHub lists it as a fork of another repository.
    pub(super) fork: bool,
    /// GitHub lists it as archived (read-only).
    pub(super) archived: bool,
    /// Only these tracked gitlinks are required by the private inventory. An ordinary
    /// GitHub listing carries none, and no other submodule is acquired implicitly.
    pub(super) required_submodules: Vec<String>,
}

impl Repo {
    fn origin(&self) -> Origin {
        Origin(vec![crate::checkout::Remote::Github {
            owner: self.owner.clone(),
            name: self.name.clone(),
        }])
    }
}

fn component(value: &str, owner: bool) -> bool {
    !value.is_empty()
        && value.len() <= (if owner { 39 } else { 100 })
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || (!owner && matches!(b, b'_' | b'.')))
}

fn branch(value: &str) -> bool {
    !value.is_empty()
        && value != "@"
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.ends_with('.')
        && !value.contains("..")
        && !value.contains("@{")
        && !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
        && value
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

fn text(sys: &dyn System, cmd: &Cmd, why: &str) -> Result<String, String> {
    let ran = sys.run_complete_stdout(cmd).map_err(|_| why.to_owned())?;
    if !ran.success() {
        return Err(why.into());
    }
    String::from_utf8(ran.stdout).map_err(|_| why.to_owned())
}

fn enumerate(
    sys: &dyn System,
    user: &User,
    config: &Github,
    token: &Secret,
) -> Result<Inventory, String> {
    if !component(&config.owner, true) || !component(&config.account, true) {
        return Err("invalid configured GitHub owner or account".into());
    }
    let base = if config.owner.eq_ignore_ascii_case(&config.account) {
        "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc"
            .to_owned()
    } else {
        let (owner, _) = get(sys, user, token, &format!("/users/{}", config.owner))?;
        if owner
            .get("login")
            .and_then(Value::as_str)
            .is_none_or(|login| !login.eq_ignore_ascii_case(&config.owner))
            || owner.get("type").and_then(Value::as_str) != Some("Organization")
        {
            return Err("a different repository owner must be a GitHub organization; personal private repositories cannot be enumerated completely this way".into());
        }
        format!(
            "/orgs/{}/repos?type=all&per_page=30&sort=full_name&direction=asc",
            config.owner
        )
    };
    enumerate_pages(
        &base,
        Some(&config.owner),
        Some(&config.account),
        |endpoint| get(sys, user, token, endpoint).map_err(String::from),
    )
}

/// A GitHub listing read whole: the repositories to have in `~`, by name, and a note for each one
/// left out because another listed repository takes its folder.
#[derive(Debug)]
pub(super) struct Inventory {
    pub(super) repositories: Vec<Repo>,
    pub(super) notes: Vec<String>,
}

/// Every page of the listing at `base`, each repository owned by `owner` when one is named. Two
/// repositories whose names differ only in case want the same folder in `~`: the one `prefer`
/// (the signed-in account) owns keeps it, else the first GitHub lists, and the other is a note.
/// # Errors
/// A page could not be read whole, or a repository is malformed, foreign or listed twice.
pub(super) fn enumerate_pages(
    base: &str,
    owner: Option<&str>,
    prefer: Option<&str>,
    api: impl FnMut(&str) -> Result<Page, String>,
) -> Result<Inventory, String> {
    let values = crate::github_pages::all(crate::github_pages::API, base, PAGE_SIZE, api)?;
    inventory(&values, owner, prefer)
}

/// Validate one complete inventory, whether read from the private vault or an explicitly
/// configured GitHub source. Counts are derived from these entries, never a separate total.
pub(super) fn inventory(
    values: &[Value],
    owner: Option<&str>,
    prefer: Option<&str>,
) -> Result<Inventory, String> {
    inventory_filtered(values, owner, prefer, |_| true)
}

/// Validate every listed identity, then apply the intended scope before resolving home
/// folders. An excluded repository cannot take a folder away from an included one.
pub(super) fn inventory_filtered(
    values: &[Value],
    owner: Option<&str>,
    prefer: Option<&str>,
    include: fn(&Repo) -> bool,
) -> Result<Inventory, String> {
    let mut named: BTreeMap<String, Repo> = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let mut notes = Vec::new();
    let preferred =
        |repo: &Repo| prefer.is_some_and(|login| repo.owner.eq_ignore_ascii_case(login));
    for value in values {
        let repo = repo(value, owner)?;
        let id = value
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id != 0)
            .ok_or("repository identity is missing")?;
        if !ids.insert(id) {
            return Err(
                "a repository is listed twice in the GitHub inventory; no repositories changed"
                    .into(),
            );
        }
        if !include(&repo) {
            continue;
        }
        let folder = repo.name.to_ascii_lowercase();
        let (kept, left) = match named.remove(&folder) {
            None => (repo, None),
            Some(first) if preferred(&repo) && !preferred(&first) => (repo, Some(first)),
            Some(first) => (first, Some(repo)),
        };
        if let Some(left) = left {
            notes.push(format!(
                "github.com/{}/{}: ~/{} is github.com/{}/{}'s; not cloned",
                left.owner, left.name, kept.name, kept.owner, kept.name
            ));
        }
        named.insert(folder, kept);
    }
    Ok(Inventory {
        repositories: named.into_values().collect(),
        notes,
    })
}

fn repo(value: &Value, expected_owner: Option<&str>) -> Result<Repo, String> {
    let owner = value
        .get("owner")
        .and_then(|owner| owner.get("login"))
        .and_then(Value::as_str)
        .filter(|owner| component(owner, true))
        .ok_or("invalid GitHub repository owner")?;
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| component(name, false))
        .ok_or("invalid GitHub repository name")?;
    if expected_owner.is_some_and(|expected| !owner.eq_ignore_ascii_case(expected))
        || value
            .get("full_name")
            .and_then(Value::as_str)
            .is_none_or(|full| !full.eq_ignore_ascii_case(&format!("{owner}/{name}")))
        || value.get("private").and_then(Value::as_bool).is_none()
        || value
            .get("clone_url")
            .and_then(Value::as_str)
            .is_none_or(|url| !origin_matches(url, owner, name))
    {
        return Err(
            "GitHub repository identity or visibility does not match the configured owner".into(),
        );
    }
    let default_branch = match value.get("default_branch") {
        Some(Value::Null) => None,
        Some(Value::String(value)) if branch(value) => Some(value.clone()),
        _ => return Err("invalid or missing GitHub default branch".into()),
    };
    Ok(Repo {
        owner: owner.into(),
        name: name.into(),
        default_branch,
        size_kib: value.get("size").and_then(Value::as_u64).unwrap_or(0),
        fork: value.get("fork").and_then(Value::as_bool).unwrap_or(false),
        archived: value
            .get("archived")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        required_submodules: super::dependencies::paths(value.get("required_submodules"))?,
    })
}

fn git_read(sys: &dyn System, user: &User, path: &Path, args: &[&str]) -> Result<String, String> {
    text(
        sys,
        &git(sys, user, Some(path)).args(args.iter().copied()),
        "cannot verify the existing Git worktree; left untouched",
    )
}

/// Whether the repository at `path` borrows objects from another store: its
/// `objects/info/alternates` is there, or what is at that path cannot be read. The path git
/// computes reads no object, so no lazy fetch can start: the read runs on every git, with no
/// `--no-lazy-fetch`, which a git before 2.44 refuses.
///
/// # Errors
/// git could not name the path.
pub(super) fn alternate_store(sys: &dyn System, user: &User, path: &Path) -> Result<bool, String> {
    let alternates = git_read(
        sys,
        user,
        path,
        &["rev-parse", "--git-path", "objects/info/alternates"],
    )?;
    let alternates = Path::new(alternates.trim());
    let alternates = if alternates.is_absolute() {
        alternates.to_owned()
    } else {
        path.join(alternates)
    };
    Ok(!matches!(sys.stat(&alternates), Err(error) if error.kind() == io::ErrorKind::NotFound))
}

pub(super) fn verify_existing(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    path: &Path,
) -> Result<(), String> {
    let unread = || {
        format!(
            "cannot verify the existing Git worktree ~/{}; left untouched",
            repo.name
        )
    };
    // One run answers all three: `true`, the path below the top level (an empty line at it),
    // and the top level.
    let read = git_read(
        sys,
        user,
        path,
        &[
            "rev-parse",
            "--is-inside-work-tree",
            "--show-prefix",
            "--show-toplevel",
        ],
    )
    .map_err(|_| unread())?;
    let mut lines = read.split('\n');
    if lines.next() != Some("true")
        || lines.next().is_none_or(|prefix| !prefix.trim().is_empty())
        || lines
            .next()
            .is_none_or(|top| !Path::new(top.trim()).is_absolute())
    {
        return Err(format!(
            "~/{} is not a top-level Git worktree; left untouched",
            repo.name
        ));
    }
    let origin = git_read(
        sys,
        user,
        path,
        &["config", "--get-all", "remote.origin.url"],
    )
    .map_err(|_| unread())?;
    let origins = origin.lines().collect::<Vec<_>>();
    if origins.len() != 1 || !origin_matches(origins[0], owner, &repo.name) {
        return Err(format!(
            "~/{} has a different or credential-bearing GitHub origin; left untouched",
            repo.name
        ));
    }
    Ok(())
}

/// The keys that make a repository a partial clone, one a read of an object it lacks fetches
/// lazily: `extensions.partialclone`, and a promisor remote's `promisor` or
/// `partialclonefilter`, whatever its name.
const PARTIAL_KEYS: &str =
    r"^(extensions\.partialclone|remote\..*\.(promisor|partialclonefilter))$";

/// The listing [`verify_integrity`] runs: `git`, a command already in the repository, asked for
/// [`PARTIAL_KEYS`] in the C locale, where git's regular expressions match bytes (under a UTF-8
/// locale, the account's, `.` matches no byte that is not UTF-8, so a promisor remote named in
/// such bytes would go unlisted while git still fetched through it).
pub(super) fn partial_listing(git: Cmd) -> Cmd {
    git.env("LC_ALL", "C")
        .args(["config", "--get-regexp", PARTIAL_KEYS])
}

pub(super) fn verify_integrity(sys: &dyn System, user: &User, path: &Path) -> Result<(), String> {
    if git_read(sys, user, path, &["rev-parse", "--is-shallow-repository"])?.trim() != "false" {
        return Err("the new clone is shallow; not published".into());
    }
    let partial = sys
        .run_complete_stdout(&partial_listing(git(sys, user, Some(path))))
        .map_err(|_| "cannot inspect partial-clone configuration")?;
    if partial.status != Some(1) || !partial.stdout.is_empty() || !partial.stderr.is_empty() {
        return Err(
            "the new clone has partial or unreadable clone configuration; not published".into(),
        );
    }
    // The clone itself checked that every object it fetched is connected (git's
    // `check_connected`), so no `fsck` reads the objects again.
    Ok(())
}

#[derive(Debug)]
struct Remote {
    head: Option<String>,
    branch: Option<String>,
    refs: BTreeMap<String, String>,
}

pub(super) fn oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn remote(text: &str) -> Result<Remote, String> {
    let (mut head, mut default, mut refs) = (None, None, BTreeMap::new());
    for line in text.lines() {
        let (value, name) = line
            .split_once('\t')
            .ok_or("unrecognized remote Git reference")?;
        if let Some(reference) = value.strip_prefix("ref: ") {
            if name != "HEAD"
                || !reference.starts_with("refs/heads/")
                || !branch(&reference[11..])
                || default.replace(reference[11..].into()).is_some()
            {
                return Err("invalid remote default branch".into());
            }
        } else if !oid(value) {
            return Err("invalid remote Git object identity".into());
        } else if name == "HEAD" {
            if head.replace(value.into()).is_some() {
                return Err("duplicate remote HEAD".into());
            }
        } else {
            let name = name.strip_suffix("^{}").unwrap_or(name);
            if !name.starts_with("refs/heads/") && !name.starts_with("refs/tags/") {
                return Err("unexpected remote Git reference".into());
            }
            if !branch(
                name.strip_prefix("refs/heads/")
                    .or_else(|| name.strip_prefix("refs/tags/"))
                    .unwrap_or(""),
            ) {
                return Err("invalid remote Git reference name".into());
            }
            if line.ends_with("^{}") {
                continue;
            }
            if refs.insert(name.into(), value.into()).is_some() {
                return Err("duplicate remote Git reference".into());
            }
        }
    }
    Ok(Remote {
        head,
        branch: default,
        refs,
    })
}

fn admit_endpoint(
    sys: &dyn System,
    command: &Cmd,
    owner: &str,
    repo: &Repo,
    url: &str,
) -> Result<String, String> {
    let expanded = text(
        sys,
        &command.clone().args(["ls-remote", "--get-url", "--", url]),
        "cannot inspect Git's effective GitHub repository URL",
    )?;
    let mut lines = expanded.lines();
    let effective = lines.next().unwrap_or_default();
    if !origin_matches(effective, owner, &repo.name) || lines.next().is_some() {
        return Err(
            "Git's effective URL does not name the configured GitHub repository; no clone started"
                .into(),
        );
    }
    Ok(effective.to_owned())
}

/// What every step of a guarded clone checks first: that the vault snapshot the clone reads is
/// still the current one.
pub(super) struct CloneGuard<'a> {
    current: &'a (dyn Fn() -> Result<(), String> + Sync),
}

impl<'a> CloneGuard<'a> {
    /// Clones that check `current` before every step.
    pub(super) fn new(current: &'a (dyn Fn() -> Result<(), String> + Sync)) -> Self {
        Self { current }
    }
}

/// The command a clone's network step runs: `ordinary` with aterm's own credential route
/// ([`crate::git_credential::pinned`]), once its effective URL reads as the listed repository's
/// ([`admit_endpoint`]). The person's URL rewrites stay theirs; no helper their files name runs.
pub(super) fn clone_command(
    sys: &dyn System,
    user: &User,
    ordinary: Cmd,
    owner: &str,
    repo: &Repo,
    url: &str,
) -> Result<Cmd, String> {
    admit_endpoint(sys, &ordinary, owner, repo, url)?;
    Ok(crate::git_credential::pinned(
        ordinary,
        super::helper(sys, user).as_ref(),
    ))
}

#[cfg(test)]
pub(super) fn clone_one_guarded(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Placed, String> {
    clone_one_with(
        sys,
        user,
        &repo.owner,
        repo,
        Some(&CloneGuard::new(current)),
        None,
    )
    .map_err(|why| why.to_string())
}

#[cfg(test)]
pub(super) fn clone_one(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
) -> Result<Placed, String> {
    clone_one_with(sys, user, owner, repo, None, None).map_err(|why| why.to_string())
}

/// How many clones run at once ([`clone_all`]).
const LANES: usize = 4;

/// What [`clone_all`] did.
#[derive(Default)]
pub(super) struct Cloned {
    /// The repositories it cloned, verified and published, in list order.
    pub(super) cloned: Vec<Repo>,
    /// The ones it left for want of room ([`no_room`]), as [`short_of_room`] names them.
    pub(super) left: Vec<String>,
    /// Why each clone that failed did, `~/<name>: <why>`, in list order.
    pub(super) failed: Vec<String>,
    pub(super) network_failed: bool,
    /// What each clone it published made of the hook installer it carries
    /// ([`install_hooks`]), `~/<name>: <outcome>`, in list order: a note, never a failure.
    pub(super) hooks: Vec<String>,
}

/// Where [`clone_one_with`] left its repository, once it is in `~`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placed {
    /// This clone published it: the setup made the worktree.
    Published,
    /// This clone published it, but its private staging directory, empty once the repository
    /// moved out of it, could not be removed: the worktree is the setup's all the same, and the
    /// row says what was left behind.
    PublishedStagingLeft,
    /// Another clone published it first (`sync.clone`, a second `aterm init`), and it reads as
    /// the repository's own worktree: this one made nothing there.
    InPlace,
}

/// The installer a repository carries for its own clones' hooks: the fleet's convention
/// (trust's and clean's), which points the clone's `core.hooksPath` at its tracked hooks and, in
/// trust, sets the clone's git client tuning (trust's GIT-CLIENT-TUNING-1).
pub(super) const HOOK_INSTALLER: &str = "scripts/install-git-hooks.sh";

/// The shell a [`HOOK_INSTALLER`] runs under, as its usage spells it (`bash <installer>`).
const BASH: &str = "/bin/bash";

/// How long a clone's [`HOOK_INSTALLER`] may run: minutes, because an installer may run git over
/// a large worktree (trust's moves the index to format 4). An installer still running then is
/// killed with its process group; one that has exited by then is done, and the budget bounds
/// nothing it left behind (a job it put in the background lives on, as it would from a shell).
const HOOK_INSTALLER_BUDGET: Duration = Duration::from_secs(300);

/// The longest outcome line [`install_hooks`] keeps of what an installer printed.
const HOOK_LINE: usize = 160;

/// Clone `missing` into `~`, up to [`LANES`] at once. The disk is read once: each clone runs only
/// while it fits beside the clones taken before it ([`no_room`]); the rest are left. A lane makes
/// a clone's staging directory as it takes the clone, under the queue's lock: the directories are
/// made in list order, and no more than [`LANES`] exist at once, so a pass cut short (a crash, a
/// `kill -9`) leaves at most that many behind. One clone that fails leaves the others to finish;
/// with `guard`, a vault snapshot that changed starts no further clone.
pub(super) fn clone_all(
    sys: &dyn System,
    user: &User,
    missing: &[Repo],
    guard: Option<&CloneGuard<'_>>,
) -> Cloned {
    let home = Path::new(&user.home);
    let volume = sys.volume(home);
    let mut done = Cloned::default();
    let mut reserved = 0_u64;
    let mut queue = std::collections::VecDeque::new();
    for repo in missing {
        if let Some(name) = no_room_beside(volume.as_ref(), repo, reserved) {
            done.left.push(name);
            continue;
        }
        reserved = reserved.saturating_add(need(repo));
        queue.push_back(repo);
    }
    let lanes = queue.len().min(LANES);
    let queue = std::sync::Mutex::new(queue);
    let results = std::sync::Mutex::new(Vec::new());
    let stopped = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..lanes {
            scope.spawn(|| {
                loop {
                    if stopped.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let (repo, staging) = {
                        let mut queue = queue
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let Some(repo) = queue.pop_front() else {
                            return;
                        };
                        (repo, Scratch::new_in(sys, home, "github-clone"))
                    };
                    // It reads the snapshot first and between its steps.
                    let parent = match staging {
                        Ok(staging) => {
                            clone_one_with(sys, user, &repo.owner, repo, guard, Some(staging))
                        }
                        Err(_) => Err(AcquisitionFailure::Local(
                            "cannot allocate private repository staging in this home".to_owned(),
                        )),
                    };
                    let published =
                        matches!(parent, Ok(Placed::Published | Placed::PublishedStagingLeft));
                    // Published with its staging left behind, whatever its dependencies do.
                    let staging_left = matches!(parent, Ok(Placed::PublishedStagingLeft));
                    let result = parent.and_then(|placed| {
                        let current = || guard.map_or(Ok(()), |guard| (guard.current)());
                        super::dependencies::repair(
                            sys,
                            user,
                            &home.join(&repo.name),
                            repo,
                            &current,
                        )?;
                        Ok(placed)
                    });
                    if result.is_err() && guard.is_some_and(|guard| (guard.current)().is_err()) {
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    // Only a clone this lane published: one found in place is another's.
                    let hooks = published
                        .then(|| install_hooks(sys, user, repo, guard))
                        .flatten();
                    results
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((repo, result, staging_left, hooks));
                }
            });
        }
    });
    let mut results = results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let order = |repo: &Repo| missing.iter().position(|listed| listed.name == repo.name);
    results.sort_by_key(|(repo, _, _, _)| order(repo));
    for (repo, result, staging_left, hooks) in results {
        match result {
            Ok(_) => done.cloned.push(repo.clone()),
            Err(why) => {
                done.network_failed |= why.is_network();
                done.failed.push(format!("~/{}: {why}", repo.name));
            }
        }
        if staging_left {
            done.failed.push(format!(
                "~/{}: repository published, but its private empty staging directory could not \
                 be removed",
                repo.name
            ));
        }
        done.hooks.extend(hooks);
    }
    done
}

/// Run the [`HOOK_INSTALLER`] of `~/<name>`, which this pass just published
/// ([`Placed::Published`], or [`Placed::PublishedStagingLeft`]), where the clone carries one: hook state is per clone and starts
/// unset, so a clone the setup makes has no hooks and no git tuning until its installer runs.
/// It runs once, as `user`, with [`BASH`] in the clone and the git the setup runs first on
/// `PATH`. Its outcome is a line for the row (`~/<name>: …`), never a failure of the clone or
/// of the setup. No clone the setup did not make reaches here, so no existing worktree runs
/// it; nor does an installer reached through a link (`~/<name>` itself, or a tracked `scripts`
/// link, which would run a file outside the clone) or a directory in its place, one inside a
/// required dependency's gitlink (another repository's file, which may be another owner's),
/// nor a guarded pass whose vault snapshot stopped being current (the check every step of a
/// guarded clone makes first). Selected required dependencies are attempted first. An ordinary
/// dependency failure does not suppress the new parent's advisory installer; an invalidated
/// source still does.
fn install_hooks(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    guard: Option<&CloneGuard<'_>>,
) -> Option<String> {
    let name = &repo.name;
    let clone = Path::new(&user.home).join(name);
    let outcome = match installer_in(sys, user, &clone, &repo.required_submodules)? {
        Err(why) => format!("its {HOOK_INSTALLER} {why}; not run"),
        Ok(()) => match guard.map_or(Ok(()), |guard| (guard.current)()) {
            Err(why) => format!("its {HOOK_INSTALLER} not run: {why}"),
            Ok(()) => hook_outcome(&sys.run(&installer_command(sys, user, &clone))),
        },
    };
    Some(format!("~/{name}: {outcome}"))
}

/// Whether `clone` carries a [`HOOK_INSTALLER`] of its own to run: `None` when it has none,
/// else `Ok` for a file that is neither a directory nor a link, reached from a `clone` folder
/// that is `user`'s own and no link through directories alone, outside every one of `required`
/// (the gitlinks the pass populated, whose files are another repository's), or why not. Each step
/// is read without following it ([`System::stat`] is an `lstat`), `clone` itself first, so a
/// link anywhere on the way is seen, one swapped in for the clone after its publish included.
/// [`Stat`](crate::port::Stat) tells no other kind apart, so a FIFO or socket there reads as a
/// file: a checkout git made holds none. What changes after this look (a link or a file planted
/// then) would beat any look here, since `bash` opens the path itself; the installer's time
/// budget bounds what such a file can do while it runs, and its outcome line says what it did.
fn installer_in(
    sys: &dyn System,
    user: &User,
    clone: &Path,
    required: &[String],
) -> Option<Result<(), &'static str>> {
    match sys.stat(clone) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(Err("cannot be inspected")),
        Ok(stat) if stat.symlink => return Some(Err("is reached through a link")),
        Ok(stat) if !stat.dir => return None,
        Ok(stat) if stat.uid != user.uid => {
            return Some(Err("is in a folder another account owns"));
        }
        Ok(_) => {}
    }
    let steps: Vec<_> = Path::new(HOOK_INSTALLER).components().collect();
    let mut at = clone.to_path_buf();
    for (index, step) in steps.iter().enumerate() {
        at.push(step);
        let file = index + 1 == steps.len();
        match sys.stat(&at) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(_) => return Some(Err("cannot be inspected")),
            Ok(stat) if stat.symlink => return Some(Err("is reached through a link")),
            // A file where a folder on the way goes: nothing can be under it, so no installer.
            Ok(stat) if !file && !stat.dir => return None,
            Ok(stat) if stat.dir == file => return Some(Err("is not a regular file")),
            Ok(_) => {}
        }
    }
    // Folded as the inventory folds its paths, for a case-insensitive volume.
    let installer = HOOK_INSTALLER.to_ascii_lowercase();
    let inside = |gitlink: &String| {
        let gitlink = gitlink.to_ascii_lowercase();
        installer == gitlink || installer.starts_with(&format!("{gitlink}/"))
    };
    if required.iter().any(inside) {
        return Some(Err("is inside a required dependency"));
    }
    Some(Ok(()))
}

/// The failed row of a pass its guard stopped once [`clone_all`] was done: `why`, then each
/// failure its clones met besides the stop itself (a clone the guard stopped says only
/// `~/<name>: <why>`, which is left out), the room it found short, and what the hook installers
/// of the clones it published did, so a stop never hides what went before it.
pub(super) fn stopped_after_clones(
    why: String,
    failed: Vec<String>,
    room: Option<String>,
    hooks: &[String],
) -> String {
    let besides: Vec<String> = failed
        .into_iter()
        .filter(|line| line.split_once(": ").is_none_or(|(_, said)| said != why))
        .collect();
    std::iter::once(why)
        .chain(besides)
        .chain(room)
        .chain(hooks.iter().cloned())
        .collect::<Vec<_>>()
        .join("; ")
}

/// `probe` with `hooks` (what the last apply's clones made of their hook installers,
/// [`Cloned::hooks`]) after its line, every one of them, whatever the read-back answers: an
/// installer runs once, when its clone is published, so the row after it is the only one that
/// can say how it went.
pub(super) fn with_hooks(probe: Probe, hooks: &[String]) -> Probe {
    if hooks.is_empty() {
        return probe;
    }
    let after = |line: String| format!("{line}; {}", hooks.join("; "));
    match probe {
        Probe::Ok { now } => Probe::Ok { now: after(now) },
        Probe::Change { from, to } => Probe::Change {
            from: after(from),
            to,
        },
        Probe::Person { step, how } => Probe::Person {
            step,
            how: after(how),
        },
        Probe::Blocked { why } => Probe::Blocked { why: after(why) },
        Probe::Waits { on, why } => Probe::Waits {
            on,
            why: after(why),
        },
        Probe::NotApplicable { why } => Probe::NotApplicable { why: after(why) },
        Probe::Deferred { why } => Probe::Deferred { why: after(why) },
        Probe::Unusable { why } => Probe::Unusable { why: after(why) },
    }
}

/// The run of `clone`'s [`HOOK_INSTALLER`] ([`install_hooks`]). On a Mac with no developer
/// tools it names a `DEVELOPER_DIR` that does not exist
/// ([`crate::items::github::NO_DEVELOPER_DIR`]): the setup clones before it installs the tools,
/// and an installer's `/usr/bin` shim (clean's runs `python3`) would otherwise put Apple's
/// install dialog in front of a person in the middle of it; the shim fails at once instead, and
/// the row says what the installer made of that.
pub(super) fn installer_command(sys: &dyn System, user: &User, clone: &Path) -> Cmd {
    let git = crate::items::github::git_program_for_user(sys, user);
    let bin = git.parent().unwrap_or_else(|| Path::new("/usr/bin"));
    let command = Cmd::new(BASH)
        .as_user(user)
        .current_dir(clone)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display()),
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .budget(HOOK_INSTALLER_BUDGET)
        .arg(HOOK_INSTALLER);
    if crate::items::github::shims_ask_for_tools(sys) {
        command.env("DEVELOPER_DIR", crate::items::github::NO_DEVELOPER_DIR)
    } else {
        command
    }
}

/// What a [`HOOK_INSTALLER`]'s run says, in one line: the first line it printed when it
/// succeeded, else how it ended with the first line of its errors, and that the clone stands.
/// What the installer printed is quoted and escaped as Rust's `Debug` writes a string (a control,
/// bidirectional, line-separator or combining character as its `\u{…}` escape), so the row's
/// reader sees where its words end and nothing in them reorders the row or starts another of its
/// notes; the escaped words are cut before the escape that would pass [`HOOK_LINE`] characters,
/// `…` after the quote saying so.
fn hook_outcome(ran: &Result<crate::port::Ran, crate::port::RunFailed>) -> String {
    let said = |text: &str| {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        // Each character is escaped as a string's `Debug` escapes it, and kept whole or not at
        // all, so the cut never ends inside an escape (a lone `\` would escape the quote).
        let (mut kept, mut shown, mut cut) = (String::new(), 0usize, false);
        for c in line.chars() {
            let one = format!("{:?}", c.to_string());
            let escape = one
                .strip_prefix('"')
                .and_then(|one| one.strip_suffix('"'))
                .unwrap_or(&one);
            let width = escape.chars().count();
            if shown + width > HOOK_LINE {
                cut = true;
                break;
            }
            kept.push_str(escape);
            shown += width;
        }
        Some(format!("\"{kept}\"{}", if cut { "…" } else { "" }))
    };
    match ran {
        Ok(ran) if ran.success() => match said(&ran.out()) {
            Some(said) => format!("ran its {HOOK_INSTALLER}: {said}"),
            None => format!("ran its {HOOK_INSTALLER}"),
        },
        Ok(ran) => {
            let how = ran.status.map_or_else(
                || "killed by a signal".to_owned(),
                |code| format!("exit {code}"),
            );
            match said(&ran.err()) {
                Some(said) => {
                    format!("its {HOOK_INSTALLER} failed ({how}: {said}); the clone stands")
                }
                None => format!("its {HOOK_INSTALLER} failed ({how}); the clone stands"),
            }
        }
        Err(failed) => {
            format!("its {HOOK_INSTALLER} did not run to the end ({failed}); the clone stands")
        }
    }
}

/// Clone `repo` into `~`, in `staging` when its place is already made ([`clone_all`]), else
/// in one made once the remote reads as listed.
fn clone_one_with(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    guard: Option<&CloneGuard<'_>>,
    staging: Option<Scratch<'_>>,
) -> Result<Placed, AcquisitionFailure> {
    let current = || guard.map_or(Ok(()), |guard| (guard.current)());
    current()?;
    let home = Path::new(&user.home);
    let destination = home.join(&repo.name);
    match sys.stat(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        // Another clone of it (`sync.clone`, a second `aterm init`) published it since the step
        // read what was missing: a worktree in place at this repository's origin is done.
        Ok(_) if verify_existing(sys, user, owner, repo, &destination).is_ok() => {
            return Ok(Placed::InPlace);
        }
        _ => {
            return Err(format!("~/{} appeared before cloning; left untouched", repo.name).into());
        }
    }
    let url = format!("https://github.com/{owner}/{}.git", repo.name);
    current()?;
    let command = clone_command(sys, user, git(sys, user, None), owner, repo, &url)?;
    let source = crate::checkout::acquisition_read(
        sys,
        &command.args([
            "ls-remote",
            "--symref",
            "--",
            &url,
            "HEAD",
            "refs/heads/*",
            "refs/tags/*",
        ]),
        "cannot read the GitHub repository's complete remote references",
    )?;
    let source = std::str::from_utf8(&source)
        .map_err(|_| "cannot read the GitHub repository's complete remote references")?;
    let source = remote(source)?;
    if let Some(head) = &source.head {
        // The private inventory selects this repository, not a permanent branch.
        // Its saved default may precede a rename or the repository's first commit.
        let expected = source
            .branch
            .as_ref()
            .ok_or("nonempty GitHub repository has no advertised default branch")?;
        if source.refs.get(&format!("refs/heads/{expected}")) != Some(head) {
            return Err("GitHub's advertised default branch does not match its HEAD".into());
        }
    } else if !source.refs.is_empty() {
        return Err("GitHub repository has references but no usable default HEAD".into());
    }
    // Refuse a changed route before allocating staging; check again after staging is
    // ready so the eventual network command does not reuse an earlier admission.
    current()?;
    clone_command(sys, user, git(sys, user, None), owner, repo, &url)?;
    let mut staging = match staging {
        Some(staging) => staging,
        None => Scratch::new_in(sys, home, "github-clone")
            .map_err(|_| "cannot allocate private repository staging in this home")?,
    };
    // Held until the staging is gone or this process ends, so a later run sweeps the staging of
    // a clone whose process was killed ([`sweep_staging`]), never one still cloning. The mark is
    // locked under another name and then renamed into place (a flock follows its file), so no
    // sweep ever finds it unlocked between its creation and its lock. One that cannot be renamed
    // leaves the staging without a mark, which a sweep leaves alone: the clone goes on.
    let marking = staging.path().join(MARKING);
    let _owner = sys
        .exclusive(&marking, Duration::ZERO)
        .map_err(|_| "cannot mark the private repository staging as this clone's")?;
    let _ = sys.rename(&marking, &staging.path().join(OWNER));
    let clone = staging.path().join("repo");
    current()?;
    let command = clone_command(sys, user, git(sys, user, None), owner, repo, &url)?
        .budget(CLONE_BUDGET)
        .args([
            "clone",
            "--no-single-branch",
            "--tags",
            "--origin",
            "origin",
            "--template=",
            "--",
            &url,
        ])
        .arg(&clone);
    crate::checkout::acquisition_read(
        sys,
        &command,
        "GitHub clone failed; existing destination left untouched",
    )?;
    verify_existing(sys, user, owner, repo, &clone)?;
    if alternate_store(sys, user, &clone)? {
        return Err("the new clone depends on an alternate object store; not published".into());
    }
    verify_new(sys, user, repo, &clone, &source)?;
    current()?;
    // Publishing is not repeated safely, so it holds the update boundary (init-on-by-default
    // design §6.7). A wait there for an update's handoff, up to minutes, checks `current` again
    // before the rename, as every step of a guarded clone does.
    let _boundary = sys.update_boundary().hold(
        aterm_types::update_boundary::GuardedItem::ClonePublishRename,
        &current,
    )?;
    if sys.rename_noreplace(&clone, &destination).is_err() {
        // Another clone of it published first: a worktree in place at this repository's origin
        // is done, and this one's staging goes. Anything else there is left untouched.
        if verify_existing(sys, user, owner, repo, &destination).is_ok() {
            staging.close().map_err(|_| {
                "the repository is in place, but this clone's private staging could not be removed"
            })?;
            return Ok(Placed::InPlace);
        }
        return Err(format!(
            "cannot publish ~/{} without replacing an existing destination; left untouched",
            repo.name
        )
        .into());
    }
    Ok(if staging.close().is_ok() {
        Placed::Published
    } else {
        Placed::PublishedStagingLeft
    })
}

/// Remove what clones left in `user`'s home when their process ended mid-clone (a crash, a kill,
/// a power loss): each staging directory of theirs whose owner mark no process holds now, so its
/// clone is gone. One whose mark is held belongs to a clone still running; one with no mark (an
/// older aterm's, or one cut off before it marked it) is left, since nothing shows its owner is
/// gone. A link, another account's directory, and a published clone (at `~/<name>`, outside
/// every staging directory) are never entered. Only a process of `user`'s own sweeps. How many
/// it removed.
pub(crate) fn sweep_staging(sys: &dyn System, user: &User) -> usize {
    if sys.euid() != user.uid {
        return 0;
    }
    let home = Path::new(&user.home);
    let Ok(names) = sys.list_dir(home) else {
        return 0;
    };
    let mut swept = 0;
    for name in names.iter().filter(|name| name.starts_with(STAGING)) {
        let staging = home.join(name);
        if !sys
            .stat(&staging)
            .is_ok_and(|st| st.dir && !st.symlink && st.uid == user.uid)
        {
            continue;
        }
        let owner = staging.join(OWNER);
        if !sys.stat(&owner).is_ok_and(|st| !st.dir && !st.symlink) {
            continue;
        }
        // Taken at once, the mark's clone is gone; held, it is still cloning.
        let Ok(mark) = sys.exclusive(&owner, Duration::ZERO) else {
            continue;
        };
        let removed = crate::agent::remove_tree(sys, &staging);
        drop(mark);
        if removed.is_ok() {
            swept += 1;
        }
    }
    swept
}

fn verify_new(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    clone: &Path,
    source: &Remote,
) -> Result<(), String> {
    // A fresh advertisement chooses the new checkout's branch. Only a truly empty
    // remote that advertises no unborn HEAD needs the inventory's saved fallback.
    let default_branch = source.branch.as_ref().or(repo.default_branch.as_ref());
    verify_integrity(sys, user, clone)?;
    let head = sys
        .run_complete_stdout(&git(sys, user, Some(clone)).args([
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
        ]))
        .map_err(|_| "cannot verify cloned HEAD")?;
    match &source.head {
        Some(expected)
            if head.success()
                && std::str::from_utf8(&head.stdout)
                    .ok()
                    .is_some_and(|value| value.trim() == expected) => {}
        None if matches!(head.status, Some(1 | 128)) && head.stdout.is_empty() => {
            if let Some(branch) = default_branch {
                text(
                    sys,
                    &git(sys, user, Some(clone)).args([
                        "symbolic-ref",
                        "HEAD",
                        &format!("refs/heads/{branch}"),
                    ]),
                    "cannot establish the empty repository's default branch",
                )?;
            }
        }
        _ => {
            return Err(
                "cloned HEAD does not match the advertised GitHub commit or empty state".into(),
            );
        }
    }
    let listed = git_read(
        sys,
        user,
        clone,
        &["for-each-ref", "--format=%(objectname) %(refname)"],
    )?;
    let mut refs = BTreeMap::new();
    for line in listed.lines() {
        let (oid, reference) = line
            .split_once(' ')
            .ok_or("unrecognized cloned Git reference")?;
        if !self::oid(oid) || refs.insert(reference.to_owned(), oid.to_owned()).is_some() {
            return Err("invalid cloned Git reference".into());
        }
    }
    let mut wanted = BTreeMap::new();
    for (reference, oid) in &source.refs {
        let target = reference.strip_prefix("refs/heads/").map_or_else(
            || reference.clone(),
            |name| format!("refs/remotes/origin/{name}"),
        );
        wanted.insert(target, oid.clone());
    }
    if let (Some(branch), Some(head)) = (default_branch, &source.head) {
        wanted.insert(format!("refs/heads/{branch}"), head.clone());
        wanted.insert("refs/remotes/origin/HEAD".into(), head.clone());
    }
    if refs != wanted {
        return Err(
            "the full set of cloned branches and tags does not match GitHub; run init again".into(),
        );
    }
    let symbolic = git_read(sys, user, clone, &["symbolic-ref", "--quiet", "HEAD"])?;
    if let Some(branch) = default_branch
        && symbolic.trim() != format!("refs/heads/{branch}")
    {
        return Err("the cloned checkout is not on GitHub's default branch".into());
    }
    if let (Some(branch), Some(_)) = (default_branch, &source.head) {
        let origin_head = git_read(
            sys,
            user,
            clone,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        )?;
        let upstream_remote = git_read(
            sys,
            user,
            clone,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )?;
        let upstream_ref = git_read(
            sys,
            user,
            clone,
            &["config", "--get", &format!("branch.{branch}.merge")],
        )?;
        if origin_head.trim() != format!("refs/remotes/origin/{branch}")
            || upstream_remote.trim() != "origin"
            || upstream_ref.trim() != format!("refs/heads/{branch}")
        {
            return Err(
                "the cloned default branch does not track GitHub's advertised branch".into(),
            );
        }
    }
    if !git_read(
        sys,
        user,
        clone,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?
    .is_empty()
    {
        return Err(
            "the new clone has incomplete checkout or unexpected worktree changes; not published"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::git_credential::script;
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, GithubResponse, Live, Ran, Stat};
    use std::path::PathBuf;

    mod clone_route_tests;
    mod default_branch_tests;
    mod hook_installer_tests;
    mod staging_tests;

    const TOKEN: &str = "fixture_synced_token_only";
    /// alice's token file, as the vault leaves it.
    const SOURCE: &str = "/Users//alice/.secrets/github";
    const BASE: &str =
        "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc";
    const A: &str = "1111111111111111111111111111111111111111";
    const B: &str = "2222222222222222222222222222222222222222";

    fn directory(uid: u32) -> Stat {
        Stat {
            uid,
            gid: 20,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        }
    }

    fn config() -> Github {
        Github {
            account: "alice".into(),
            owner: "alice".into(),
            token_file: SOURCE.into(),
            token_key: None,
        }
    }

    fn token() -> Secret {
        Secret::new(TOKEN.as_bytes()).unwrap()
    }

    fn fixture() -> (Fake, User, Github) {
        let mut sys = Fake::root();
        let who = user("alice", 501);
        sys.euid = who.uid;
        sys.stat_of(&who.home, directory(who.uid));
        sys.executable(crate::items::github::CLT_GIT, b"synthetic git");
        let profile = "/Users//alice/.config/aterm/init.toml";
        let bytes = format!("[github]\naccount='alice'\nowner='alice'\ntoken_file='{SOURCE}'\n");
        sys.file(profile, bytes.as_bytes()).stat_of(
            profile,
            Stat {
                uid: who.uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
        // Her token file, which aterm's route over that file hands back.
        sys.file(SOURCE, TOKEN.as_bytes()).stat_of(
            SOURCE,
            Stat {
                uid: who.uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: TOKEN.len() as u64,
            },
        );
        script::launcher(&sys, &who);
        let helper = config().helper(&sys, &who).expect("the launcher is there");
        script::hands_back(&sys, &who, &helper, TOKEN);
        sys.on_github_api(&who, "/user", json(r#"{"login":"alice"}"#));
        (sys, who, config())
    }

    fn value(name: &str, id: u64, private: bool) -> Value {
        aterm_json::json!({
            "id": id, "name": name, "full_name": format!("alice/{name}"),
            "owner": {"login": "alice"}, "private": private,
            "clone_url": format!("https://github.com/alice/{name}.git"),
            "default_branch": "main", "size": 0,
        })
    }

    fn json(body: &str) -> Result<GithubResponse, String> {
        Ok(GithubResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.as_bytes().to_vec(),
        })
    }

    fn page(values: Vec<Value>, links: Option<String>) -> Result<GithubResponse, String> {
        let mut answer = json(&aterm_json::to_string(&values).unwrap())?;
        answer
            .headers
            .extend(links.map(|links| ("Link".to_owned(), links)));
        Ok(answer)
    }

    #[test]
    fn pagination_includes_private_empty_and_every_page_without_any_mutation() {
        let (sys, who, config) = fixture();
        let first = (0..30)
            .map(|i| value(&format!("repo{i:03}"), i + 1, i % 2 == 0))
            .collect();
        let links = format!(
            "<https://api.github.com{BASE}&page=2>; rel=\"next\", <https://api.github.com{BASE}&page=2>; rel=\"last\""
        );
        sys.on_github_api(&who, &format!("{BASE}&page=1"), page(first, Some(links)));
        let mut empty = value("zz-empty-private", 101, true);
        empty
            .as_object_mut()
            .unwrap()
            .insert("default_branch".into(), Value::Null);
        sys.on_github_api(&who, &format!("{BASE}&page=2"), page(vec![empty], None));
        let repos = enumerate(&sys, &who, &config, &token())
            .unwrap()
            .repositories;
        assert_eq!(repos.len(), 31);
        assert_eq!(repos.last().unwrap().default_branch, None);
        assert_eq!(sys.calls().len(), 2);
        assert!(
            sys.calls()
                .iter()
                .all(|call| call.contains(" agh GET ") && !call.contains("clone"))
        );

        let (sys, who, config) = fixture();
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                (0..30)
                    .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                    .collect(),
                None,
            ),
        );
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=2"),
            page(
                vec![],
                Some(format!(
                    "<https://api.github.com{BASE}&page=1>; rel=\"prev\", <https://api.github.com{BASE}&page=1>; rel=\"last\""
                )),
            ),
        );
        assert_eq!(
            enumerate(&sys, &who, &config, &token())
                .unwrap()
                .repositories
                .len(),
            30
        );
    }

    #[test]
    fn a_profile_changed_after_probe_refuses_the_prepared_repository_source() {
        let (sys, who, config) = fixture();
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(vec![value("new", 1, true)], None),
        );
        let ctx = ctx(std::slice::from_ref(&who));
        let item = item(&who, &config, Vec::new());
        assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
        let path = "/Users//alice/.config/aterm/init.toml";
        let bytes = format!("[github]\naccount='alice'\nowner='team'\ntoken_file='{SOURCE}'\n");
        sys.file(path, bytes.as_bytes()).stat_of(
            path,
            Stat {
                uid: who.uid,
                gid: 20,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: bytes.len() as u64,
            },
        );
        sys.on_github_api(&who, &format!("{BASE}&page=1"), page(Vec::new(), None));
        assert!(
            matches!(item.probe(&sys, &ctx), Probe::Blocked { .. }),
            "a stale source cannot read green even if its old inventory is empty"
        );
        assert!(
            item.apply(&sys, &ctx)
                .unwrap_err()
                .contains("profile changed")
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
    }

    #[test]
    fn malformed_foreign_duplicate_or_incomplete_pages_never_start_cloning() {
        for variant in 0..5 {
            let (sys, who, config) = fixture();
            let mut values = vec![value("one", 1, true)];
            let mut links = None;
            match variant {
                0 => {
                    values[0].as_object_mut().unwrap().remove("default_branch");
                }
                1 => {
                    values[0]
                        .as_object_mut()
                        .unwrap()
                        .insert("name".into(), Value::String("../escape".into()));
                }
                2 => {
                    values.push(value("two", 1, false));
                }
                3 => {
                    links = Some("<https://elsewhere.invalid/repos?page=2>; rel=\"next\"".into());
                }
                4 => {
                    values = (0..30)
                        .map(|i| value(&format!("repo{i:03}"), i + 1, false))
                        .collect();
                    sys.on_github_api(
                        &who,
                        &format!("{BASE}&page=2"),
                        page(vec![value("omitted", 101, true)], None),
                    );
                }
                _ => unreachable!(),
            }
            sys.on_github_api(&who, &format!("{BASE}&page=1"), page(values, links));
            assert!(enumerate(&sys, &who, &config, &token()).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("scratch") && !call.contains(" clone "))
            );
        }
        let (sys, who, config) = fixture();
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            Err("GitHub's answer was cut short".into()),
        );
        assert!(enumerate(&sys, &who, &config, &token()).is_err());
    }

    #[test]
    fn differing_personal_owner_is_refused_and_organization_scope_is_explicit() {
        let (sys, who, mut config) = fixture();
        config.owner = "other".into();
        sys.on_github_api(
            &who,
            "/users/other",
            json(r#"{"login":"other","type":"User"}"#),
        );
        assert!(
            enumerate(&sys, &who, &config, &token())
                .unwrap_err()
                .contains("organization")
        );
        let (sys, who, mut config) = fixture();
        config.owner = "team".into();
        sys.on_github_api(
            &who,
            "/users/team",
            json(r#"{"login":"team","type":"Organization"}"#),
        );
        sys.on_github_api(
            &who,
            "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
            page(vec![], None),
        );
        assert!(
            enumerate(&sys, &who, &config, &token())
                .unwrap()
                .repositories
                .is_empty()
        );
    }

    pub(in crate::init_github) fn script_existing(sys: &Fake, who: &User, path: &Path, name: &str) {
        for (args, output) in [
            (
                vec![
                    "rev-parse",
                    "--is-inside-work-tree",
                    "--show-prefix",
                    "--show-toplevel",
                ],
                format!("true\n\n{}\n", path.display()),
            ),
            // What `checkout::read` asks of a worktree it aligns.
            (
                vec!["rev-parse", "--is-inside-work-tree"],
                "true\n".to_owned(),
            ),
            (vec!["rev-parse", "--show-prefix"], String::new()),
            (
                vec!["config", "--get-all", "remote.origin.url"],
                format!("https://github.com/alice/{name}.git\n"),
            ),
            (
                vec!["rev-parse", "--is-shallow-repository"],
                "false\n".into(),
            ),
        ] {
            sys.on(
                &git(sys, who, Some(path)).args(args).display(),
                Ran::ok(&output),
            );
        }
        sys.on(
            &partial_listing(git(sys, who, Some(path))).display(),
            Ran::exit(1, ""),
        );
    }

    fn script_read(sys: &Fake, who: &User, path: &Path, name: &str, status: &str) {
        for (args, output) in [
            (
                vec!["ls-remote", "--get-url", "origin"],
                format!("https://github.com/alice/{name}.git\n"),
            ),
            (
                vec!["symbolic-ref", "--quiet", "HEAD"],
                "refs/heads/main\n".to_owned(),
            ),
            (
                vec!["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
                format!("{A}\n"),
            ),
        ] {
            sys.on(
                &git(sys, who, Some(path)).args(args).display(),
                Ran::ok(&output),
            );
        }
        sys.on(
            &crate::checkout::Programs::listing(git(sys, who, Some(path))).display(),
            Ran::exit(1, ""),
        );
        sys.on(
            &crate::checkout::Programs::default()
                .off(git(sys, who, Some(path)))
                .args(crate::checkout::NO_SIDE_RUNS)
                .args([
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=normal",
                    "--ignore-submodules=all",
                ])
                .display(),
            Ran::ok(status),
        );
    }

    #[test]
    fn a_clean_worktree_behind_github_is_fast_forwarded_and_a_recorded_one_left_to_develop() {
        use std::sync::{Arc, Mutex};
        for behind in [true, false] {
            let (sys, who, config) = fixture();
            let home = Path::new(&who.home);
            let (moving, dirty, recorded) =
                (home.join("moving"), home.join("dirty"), home.join("aterm"));
            for (path, name) in [(&moving, "moving"), (&dirty, "dirty"), (&recorded, "aterm")] {
                sys.stat_of(path.to_str().unwrap(), directory(who.uid));
                script_existing(&sys, &who, path, name);
            }
            script_read(&sys, &who, &dirty, "dirty", " M README\0");
            sys.on(
                &crate::checkout::network(&sys, &who, Some(&dirty))
                    .args(["ls-remote", "--symref", "origin", "HEAD"])
                    .display(),
                Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{B}\tHEAD\n")),
            );
            script_read(&sys, &who, &moving, "moving", "");
            let state = Arc::new(Mutex::new((if behind { A } else { B }.to_owned(), false)));
            let model = {
                let (state, moving) = (Arc::clone(&state), moving.clone());
                move |_: &Fake, cmd: &Cmd| -> Option<Ran> {
                    let args: Vec<String> = cmd
                        .args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect();
                    if args.get(1).map(PathBuf::from) != Some(moving.clone()) {
                        return None;
                    }
                    let mut state = state.lock().unwrap();
                    let (head, fetched) = (state.0.clone(), state.1);
                    let mut operation = &args[2..];
                    while operation.first().map(String::as_str) == Some("-c")
                        && operation
                            .get(1)
                            .is_some_and(|value| value.starts_with("credential."))
                    {
                        operation = &operation[2..];
                    }
                    let line = operation.join(" ");
                    Some(match line.as_str() {
                        "rev-parse --verify --quiet HEAD^{commit}" => Ran::ok(&format!("{head}\n")),
                        "ls-remote --symref origin HEAD" => {
                            Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{B}\tHEAD\n"))
                        }
                        line if line == format!("rev-parse --verify --quiet {B}^{{commit}}") => {
                            if fetched {
                                Ran::ok(&format!("{B}\n"))
                            } else {
                                Ran::exit(1, "")
                            }
                        }
                        "rev-list --count HEAD --not --remotes=origin" => Ran::ok("0\n"),
                        "-c core.hooksPath=/dev/null -c core.fsmonitor= -c maintenance.auto=false fetch --quiet --no-recurse-submodules origin" =>
                        {
                            state.1 = true;
                            Ran::ok("")
                        }
                        "rev-parse --verify --quiet refs/remotes/origin/main^{commit}" => {
                            Ran::ok(&format!("{B}\n"))
                        }
                        line if line == format!("merge-base --is-ancestor {A} {B}") => Ran::ok(""),
                        line if line == format!("rev-list --count {A}..{B}") => Ran::ok("1\n"),
                        line if line
                            == format!(
                                "-c core.hooksPath=/dev/null -c core.fsmonitor= -c maintenance.auto=false -c submodule.recurse=false merge --ff-only --no-autostash --quiet {B}"
                            ) =>
                        {
                            state.0 = B.to_owned();
                            Ran::ok("")
                        }
                        _ => return None,
                    })
                }
            };
            sys.simulate(model);
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(
                    vec![
                        value("aterm", 1, true),
                        value("dirty", 2, true),
                        value("moving", 3, true),
                    ],
                    None,
                ),
            );
            let probe = item(&who, &config, vec![recorded.clone()])
                .probe(&sys, &ctx(std::slice::from_ref(&who)));
            let rows = engine::run(
                &[item(&who, &config, vec![recorded.clone()])],
                Mode::Apply,
                &sys,
                &ctx(std::slice::from_ref(&who)),
                &mut NoJournal,
            );
            let calls = sys.calls();
            if behind {
                assert!(
                    matches!(&probe, Probe::Change { from, .. } if from.contains("1 clean worktree(s) behind GitHub")),
                    "{probe:?}"
                );
                assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
                assert_eq!(state.lock().unwrap().0, B);
                assert_eq!(
                    calls.iter().filter(|call| call.contains(" fetch ")).count(),
                    1
                );
                assert_eq!(
                    calls
                        .iter()
                        .filter(|call| call.contains(" merge --ff-only "))
                        .count(),
                    1
                );
            } else {
                assert!(matches!(&probe, Probe::Ok { .. }), "{probe:?}");
                assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
                assert!(
                    !calls
                        .iter()
                        .any(|call| call.contains(" fetch ") || call.contains(" merge "))
                );
            }
            let after = item(&who, &config, vec![recorded.clone()])
                .probe(&sys, &ctx(std::slice::from_ref(&who)));
            assert!(
                matches!(&after, Probe::Ok { now } if now.contains("1 recorded in [develop]")
                    && now.contains("~/dirty: it has uncommitted changes")),
                "{after:?}"
            );
            let recorded_reads: Vec<&String> = calls
                .iter()
                .filter(|call| call.contains(&format!("-C {} ", recorded.display())))
                .collect();
            assert!(
                recorded_reads
                    .iter()
                    .all(|call| call.contains(" rev-parse --is-inside-work-tree")
                        || call.contains(" rev-parse --show-prefix")
                        || call.contains(" rev-parse --show-toplevel")
                        || call.contains(" config --get-all remote.origin.url")),
                "{recorded_reads:?}"
            );
            for word in [
                "reset", "stash", "rebase", "checkout", "switch", "pull", "push",
            ] {
                assert!(
                    !calls.iter().any(|call| call.contains(&format!(" {word} "))
                        || call.ends_with(&format!(" {word}"))),
                    "{word}: {calls:?}"
                );
            }
        }
    }

    const GIB: u64 = aterm_capacity::floor::GIB;

    /// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
    fn volume(free: u64) -> aterm_capacity::reading::Volume {
        aterm_capacity::reading::Volume {
            mount: PathBuf::from("/System/Volumes/Data"),
            total: 100 * GIB,
            free,
            floor: 8 * GIB,
            room: 8 * GIB,
        }
    }

    fn sized(name: &str, id: u64, size_kib: u64) -> Value {
        let mut value = value(name, id, true);
        value
            .as_object_mut()
            .unwrap()
            .insert("size".into(), aterm_json::json!(size_kib));
        value
    }

    /// A clone runs while five times GitHub's size for it leaves the disk watch's floor free, to
    /// the byte; one that would not is named and left; an unread volume runs every clone.
    #[test]
    fn a_clone_runs_only_while_it_leaves_the_disk_watchs_floor_free() {
        let repo = |size_kib| Repo {
            owner: "alice".into(),
            name: "big".into(),
            default_branch: Some("main".into()),
            size_kib,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        assert_eq!(no_room(None, &repo(u64::MAX)), None);
        assert_eq!(short_of_room(None, &[], INIT_AGAIN), None);

        // GitHub's 0 (empty, or not sized yet) still needs room above the floor.
        assert_eq!(no_room(Some(&volume(8 * GIB + 5 * 1024)), &repo(0)), None);
        assert_eq!(
            no_room(Some(&volume(8 * GIB + 5 * 1024 - 1)), &repo(0)).as_deref(),
            Some("~/big")
        );
        assert_eq!(
            no_room(Some(&volume(3 * GIB)), &repo(0)).as_deref(),
            Some("~/big")
        );

        let volume = volume(8 * GIB + 5 * 1000 * 1024);
        assert_eq!(no_room(Some(&volume), &repo(1000)), None);
        let left = [
            no_room(Some(&volume), &repo(1001)).unwrap(),
            no_room(Some(&volume), &repo(2 * 1024 * 1024)).unwrap(),
        ];
        assert_eq!(left, ["~/big", "~/big (about 10 GiB)"]);
        assert_eq!(short_of_room(Some(&volume), &[], INIT_AGAIN), None);
        assert_eq!(
            short_of_room(Some(&volume), &left, "try again").unwrap(),
            "2 repositories not cloned, for want of disk space: ~/big, ~/big (about 10 GiB); \
             /System/Volumes/Data has 8 GiB free and keeps 8 GiB free; free space, then try again"
        );
    }

    /// The plan says how many missing repositories the disk has no room for; a pass leaves
    /// out each of them, still clones the next one that fits, and ends naming the one it left.
    #[test]
    fn a_repository_the_disk_has_no_room_for_is_left_and_the_rest_cloned() {
        let (mut sys, who, config) = fixture();
        sys.volume = Some(volume(12 * GIB));
        let (_, _, clone_key) = script_clone(&sys, &who, false);
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                vec![sized("huge", 1, 2 * 1024 * 1024), value("new", 2, true)],
                None,
            ),
        );
        let ctx = ctx(std::slice::from_ref(&who));
        match item(&who, &config, Vec::new()).probe(&sys, &ctx) {
            Probe::Change { from, .. } => assert_eq!(
                from,
                "2 of 2 repositories missing from ~, 1 of them with no room on the disk now; 0 \
                 clean worktree(s) behind GitHub"
            ),
            other => panic!("{other:?}"),
        }
        let why = item(&who, &config, Vec::new())
            .apply(&sys, &ctx)
            .unwrap_err();
        assert_eq!(
            why,
            "1 repository not cloned, for want of disk space: ~/huge (about 10 GiB); \
             /System/Volumes/Data has 12 GiB free and keeps 8 GiB free; free space, then run \
             aterm init again"
        );
        let calls = sys.calls();
        assert_eq!(calls.iter().filter(|call| **call == clone_key).count(), 1);
        assert!(
            calls.iter().all(|call| !call.contains("alice/huge")),
            "{calls:?}"
        );
    }

    /// A pass reads the disk once, and each clone it admits holds its room for the ones after
    /// it: of two repositories that each fit alone but not together, the first clones and the
    /// second is left with the reason, never started.
    #[test]
    fn each_clone_a_pass_admits_holds_its_room_for_the_ones_after_it() {
        let (mut sys, who, _config) = fixture();
        // 8 GiB above the floor: room for one clone of about 5 GiB, not for two.
        sys.volume = Some(volume(16 * GIB));
        let sized = |name: &str, sequence| {
            let (mut repo, _, _) = script_clone_named(&sys, &who, name, sequence, false);
            repo.size_kib = GIB / 1024;
            repo
        };
        let missing = [sized("one", 0), sized("two", 1)];
        let cloned = clone_all(&sys, &who, &missing, None);
        let names: Vec<&str> = cloned
            .cloned
            .iter()
            .map(|repo| repo.name.as_str())
            .collect();
        assert_eq!(names, ["one"]);
        assert!(cloned.failed.is_empty(), "{:?}", cloned.failed);
        assert_eq!(cloned.left, ["~/two (about 5 GiB)"]);
        let calls = sys.calls();
        assert!(
            calls.iter().all(|call| !call.contains("alice/two")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_worktree_whose_origin_cannot_be_read_never_stops_a_clone() {
        for (stderr, offline) in [
            (
                "git@github.com: Permission denied (publickey).\nfatal: Could not read from \
                 remote repository.\n",
                false,
            ),
            (
                "fatal: unable to access 'https://github.com/alice/present.git/': Could not \
                 resolve host: github.com\n",
                true,
            ),
        ] {
            let (sys, who, config) = fixture();
            let present = Path::new(&who.home).join("present");
            sys.stat_of(present.to_str().unwrap(), directory(who.uid));
            script_existing(&sys, &who, &present, "present");
            script_read(&sys, &who, &present, "present", "");
            sys.on(
                &crate::checkout::network(&sys, &who, Some(&present))
                    .args(["ls-remote", "--symref", "origin", "HEAD"])
                    .display(),
                Ran::exit(128, stderr),
            );
            let (_, _, clone_key) = script_clone(&sys, &who, false);
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("new", 1, true), value("present", 2, true)], None),
            );
            let origin = Origin(vec![crate::checkout::Remote::Github {
                owner: "alice".into(),
                name: "present".into(),
            }]);
            let unread = crate::checkout::read(&sys, &who, &present, &origin).unwrap_err();
            assert_eq!(unread.offline, offline);
            let ctx = ctx(std::slice::from_ref(&who));
            let item = item(&who, &config, Vec::new());
            match item.probe(&sys, &ctx) {
                Probe::Change { from, .. } => {
                    assert!(from.starts_with("1 of 2 repositories missing"), "{from}");
                }
                other => panic!("{other:?}"),
            }
            let scan = Repositories {
                user: who.clone(),
                config: config.clone(),
                recorded: Vec::new(),
                hooks: std::sync::Mutex::new(Vec::new()),
                reading: LastReading::default(),
            }
            .scan(&sys, &token())
            .unwrap();
            assert!(
                scan.kept().is_some_and(
                    |kept| kept.starts_with("~/present: ") && kept.contains(&unread.why)
                ),
                "{:?}",
                scan.kept()
            );
            item.apply(&sys, &ctx).unwrap();
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| **call == clone_key)
                    .count(),
                1
            );
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains(" fetch ") && !call.contains(" merge ")),
                "{:?}",
                sys.calls()
            );
        }
    }

    #[test]
    fn the_profile_s_develop_checkouts_are_left_to_the_develop_items() {
        for develop in [true, false] {
            let (sys, who, _) = fixture();
            let aterm = Path::new(&who.home).join("aterm");
            sys.stat_of(aterm.to_str().unwrap(), directory(who.uid));
            script_existing(&sys, &who, &aterm, "aterm");
            script_read(&sys, &who, &aterm, "aterm", "");
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("aterm", 1, true)], None),
            );
            let mut profile = "[github]\naccount = \"alice\"\nowner = \"alice\"\ntoken_file = \
                               \"~/.secrets/github\"\n"
                .to_owned();
            if develop {
                profile.push_str("\n[develop]\ncheckouts = [\"~/aterm\"]\n");
            }
            sys.write_user_file(
                &who,
                &Path::new(&who.home).join(".config/aterm/init.toml"),
                profile.as_bytes(),
                0o600,
            )
            .unwrap();
            // After the profile: its `token_file` is what the route's helper names.
            let remote_head = crate::checkout::network(&sys, &who, Some(&aterm))
                .args(["ls-remote", "--symref", "origin", "HEAD"])
                .display();
            sys.on(
                &remote_head,
                Ran::ok(&format!("ref: refs/heads/main\tHEAD\n{A}\tHEAD\n")),
            );
            let ctx = ctx(std::slice::from_ref(&who));
            let items = crate::init_github::items(&sys, &ctx, None).unwrap();
            let repositories = items
                .iter()
                .find(|item| item.id().starts_with("init.github.repositories@"))
                .unwrap();
            let probe = repositories.probe(&sys, &ctx);
            let read = sys.calls().contains(&remote_head);
            if develop {
                assert!(
                    matches!(&probe, Probe::Ok { now } if now.contains("1 recorded in [develop]")),
                    "{probe:?}"
                );
                assert!(!read, "{:?}", sys.calls());
            } else {
                assert!(
                    matches!(&probe, Probe::Ok { now } if !now.contains("[develop]")),
                    "{probe:?}"
                );
                assert!(read, "{:?}", sys.calls());
            }
        }
    }

    #[test]
    fn plan_and_check_probe_do_not_clone_or_change_existing_branches_or_files() {
        let (sys, who, config) = fixture();
        let home = Path::new(&who.home);
        let existing = home.join("existing");
        sys.stat_of(existing.to_str().unwrap(), directory(who.uid));
        sys.file(
            existing.join("dirty.txt").to_str().unwrap(),
            b"owner changes",
        );
        script_existing(&sys, &who, &existing, "existing");
        script_read(&sys, &who, &existing, "existing", "?? dirty.txt\0");
        sys.on_github_api(
            &who,
            &format!("{BASE}&page=1"),
            page(
                vec![value("existing", 1, true), value("missing", 2, true)],
                None,
            ),
        );
        let rows = engine::run(
            &[item(&who, &config, Vec::new())],
            Mode::Plan,
            &sys,
            &ctx(std::slice::from_ref(&who)),
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(
            sys.read_file(&existing.join("dirty.txt")).unwrap(),
            b"owner changes"
        );
        assert!(sys.calls().iter().all(|call| !call.contains("scratch")
            && !call.contains(" clone ")
            && !call.contains("rename")
            && !call.contains("checkout")
            && !call.contains("fetch")
            && !call.contains("pull")
            && !call.contains("reset")));
    }

    /// A file, a link or another origin's worktree where a selected repository goes is left as
    /// it is. The repository remains in the desired count and the stage stays unfinished.
    #[test]
    fn occupied_symlink_and_mismatched_origins_are_left_as_notes_and_never_fetched() {
        for kind in 0..3 {
            let (sys, who, config) = fixture();
            let path = Path::new(&who.home).join("one");
            match kind {
                0 => {
                    sys.file(path.to_str().unwrap(), b"owner file");
                }
                1 => {
                    sys.link(path.to_str().unwrap(), "/elsewhere");
                }
                2 => {
                    sys.stat_of(path.to_str().unwrap(), directory(who.uid));
                    let cmd = git(&sys, &who, Some(&path)).args([
                        "config",
                        "--get-all",
                        "remote.origin.url",
                    ]);
                    sys.on(
                        &cmd.display(),
                        Ran::ok("https://private-token@github.com/alice/one.git\n"),
                    );
                    script_existing(&sys, &who, &path, "one");
                }
                _ => unreachable!(),
            }
            sys.on_github_api(
                &who,
                &format!("{BASE}&page=1"),
                page(vec![value("one", 1, true)], None),
            );
            let row = engine::run(
                &[item(&who, &config, Vec::new())],
                Mode::Apply,
                &sys,
                &ctx(std::slice::from_ref(&who)),
                &mut NoJournal,
            )
            .remove(0);
            assert_eq!(row.outcome, Outcome::Person, "{row:?}");
            assert_eq!(
                crate::init_observe::Reading::of(&row),
                Some(crate::init_observe::Reading::Repositories {
                    present: 0,
                    count: 1,
                    signed_in: true,
                }),
            );
            let why = &row
                .explain
                .as_ref()
                .expect("the preserved destination is explained")
                .why;
            assert!(why.contains("~/one"), "{why}");
            assert!(!why.contains("private-token"), "{why}");
            assert!(sys.calls().iter().all(|call| !call.contains("scratch")
                && !call.contains(" clone ")
                && !call.contains("rename")
                && !call.contains("fetch")));
            assert!(sys.stat(&path).is_ok());
        }
    }

    fn script_clone(sys: &Fake, who: &User, empty: bool) -> (Repo, PathBuf, String) {
        script_clone_named(sys, who, "new", 0, empty)
    }

    /// A clone of github.com/alice/`name` into the `sequence`th staging directory the run
    /// allocates, scripted from its first read to its last.
    pub(in crate::init_github) fn script_clone_named(
        sys: &Fake,
        who: &User,
        name: &str,
        sequence: u64,
        empty: bool,
    ) -> (Repo, PathBuf, String) {
        script_clone_named_route(sys, who, name, sequence, empty, false)
    }

    /// Where the `sequence`th clone of github.com/alice/`name` stages its repository, and the
    /// display of that clone's command, under which a script adds what the clone writes.
    pub(in crate::init_github) fn clone_key(
        sys: &Fake,
        who: &User,
        name: &str,
        sequence: u64,
    ) -> (PathBuf, String) {
        let staged =
            Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", sequence).join("repo");
        let command = crate::checkout::network(sys, who, None)
            .budget(CLONE_BUDGET)
            .args([
                "clone",
                "--no-single-branch",
                "--tags",
                "--origin",
                "origin",
                "--template=",
                "--",
                &format!("https://github.com/alice/{name}.git"),
            ])
            .arg(&staged);
        (staged, command.display())
    }

    /// [`script_clone_named`], its effective URL a same-repository SSH rewrite when `rewritten`:
    /// the person's own URL, which the clone keeps, under aterm's credential route.
    pub(in crate::init_github) fn script_clone_named_route(
        sys: &Fake,
        who: &User,
        name: &str,
        sequence: u64,
        empty: bool,
        rewritten: bool,
    ) -> (Repo, PathBuf, String) {
        let repo = Repo {
            owner: "alice".into(),
            name: name.into(),
            default_branch: Some("main".into()),
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        let url = format!("https://github.com/alice/{name}.git");
        let url = url.as_str();
        let endpoint = if rewritten {
            format!("git@github.com:alice/{name}.git")
        } else {
            url.to_owned()
        };
        sys.on(
            &git(sys, who, None)
                .args(["ls-remote", "--get-url", "--", url])
                .display(),
            Ran::ok(&endpoint),
        );
        let route = || crate::checkout::network(sys, who, None);
        let remote = if empty {
            String::new()
        } else {
            format!(
                "ref: refs/heads/main\tHEAD\n{A}\tHEAD\n{A}\trefs/heads/main\n{B}\trefs/heads/topic\n{B}\trefs/tags/v1\n"
            )
        };
        let command = route().args([
            "ls-remote",
            "--symref",
            "--",
            url,
            "HEAD",
            "refs/heads/*",
            "refs/tags/*",
        ]);
        sys.on(&command.display(), Ran::ok(&remote));
        let (staged, clone_key) = clone_key(sys, who, name, sequence);
        sys.on(&clone_key, Ran::ok(""));
        sys.on_stat(&clone_key, &staged, directory(who.uid));
        sys.on_write(&clone_key, &staged.join(".git/fixture"), b"complete clone");
        script_existing(sys, who, &staged, name);
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["rev-parse", "--git-path", "objects/info/alternates"])
                .display(),
            Ran::ok(".git/objects/info/alternates\n"),
        );
        let head = git(sys, who, Some(&staged)).args(["rev-parse", "--verify", "HEAD^{commit}"]);
        sys.on(
            &head.display(),
            if empty {
                Ran::exit(128, "unborn")
            } else {
                Ran::ok(A)
            },
        );
        let refs = if empty {
            String::new()
        } else {
            format!(
                "{A} refs/heads/main\n{A} refs/remotes/origin/HEAD\n{A} refs/remotes/origin/main\n{B} refs/remotes/origin/topic\n{B} refs/tags/v1\n"
            )
        };
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["for-each-ref", "--format=%(objectname) %(refname)"])
                .display(),
            Ran::ok(&refs),
        );
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["symbolic-ref", "HEAD", "refs/heads/main"])
                .display(),
            Ran::ok(""),
        );
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["symbolic-ref", "--quiet", "HEAD"])
                .display(),
            Ran::ok("refs/heads/main\n"),
        );
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
                .display(),
            Ran::ok("refs/remotes/origin/main\n"),
        );
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["config", "--get", "branch.main.remote"])
                .display(),
            Ran::ok("origin\n"),
        );
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["config", "--get", "branch.main.merge"])
                .display(),
            Ran::ok("refs/heads/main\n"),
        );
        sys.on(
            &git(sys, who, Some(&staged))
                .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
                .display(),
            Ran::ok(""),
        );
        (repo, staged, clone_key)
    }

    /// A clone's publish rename is not repeated safely, so it holds the update boundary while
    /// it runs (init-on-by-default design §6.7): an update's handoff waits for it, boundedly.
    /// Before, nothing held the guard, and a handoff could cut the publish in half.
    #[test]
    fn a_clones_publish_rename_holds_the_update_boundary() {
        let (sys, who, _) = fixture();
        let (repo, _, _) = script_clone(&sys, &who, false);
        clone_one(&sys, &who, "alice", &repo).unwrap();
        assert_eq!(
            sys.guarded_renames(),
            [Path::new(&who.home).join("new")],
            "the publish rename ran under the guard"
        );
    }

    /// A guarded clone checks `current` before every step, the publish included. The publish's
    /// guard waits for a pending update handoff (design §6.7), up to minutes, after the last
    /// check; once the handoff is refused it checks `current` again, and publishes nothing when
    /// the vault changed meanwhile. Before, the clone was published on the check made before the
    /// wait.
    #[test]
    fn a_clone_publish_that_waited_for_a_handoff_checks_current_again() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for changes in [true, false] {
            let (sys, who, _) = fixture();
            let changed = AtomicBool::new(false);
            let current = || {
                if changed.load(Ordering::SeqCst) {
                    Err("injected: the vault changed while the publish waited".to_owned())
                } else {
                    Ok(())
                }
            };
            let (repo, _staged, _) = script_clone(&sys, &who, false);
            let latch = sys.boundary.latch_handoff(Duration::from_secs(60));
            let result = std::thread::scope(|scope| {
                let worker = scope.spawn(|| clone_one_guarded(&sys, &who, &repo, &current));
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                while sys.boundary.waiting() == 0 {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the publish never waited"
                    );
                    aterm_uds::deadline::poll_yield();
                }
                changed.store(changes, Ordering::SeqCst);
                drop(latch);
                worker.join().unwrap()
            });
            let published = sys
                .calls()
                .iter()
                .any(|call| call.starts_with("rename-noreplace "));
            if changes {
                assert!(
                    !published,
                    "the clone was published after the vault changed during its wait: {result:?}"
                );
                assert!(
                    matches!(&result, Err(why) if why.contains("the vault changed")),
                    "{result:?}"
                );
            } else {
                // CONTROL: nothing changed while it waited, and it publishes once.
                assert!(published && result.is_ok(), "{result:?}");
            }
            assert_eq!(sys.boundary.any_held(), None);
        }
    }

    #[test]
    fn full_and_empty_new_clones_are_verified_then_published_without_credentials_in_origin() {
        for empty in [false, true] {
            let (sys, who, _) = fixture();
            let (repo, staged, _) = script_clone(&sys, &who, empty);
            clone_one(&sys, &who, "alice", &repo).unwrap();
            let final_path = Path::new(&who.home).join("new");
            assert_eq!(
                sys.read_file(&final_path.join(".git/fixture")).unwrap(),
                b"complete clone"
            );
            assert!(sys.stat(&staged).is_err());
            let calls = sys.calls();
            let integrity = calls
                .iter()
                .position(|call| call.contains("rev-parse --is-shallow-repository"))
                .unwrap();
            let published = calls
                .iter()
                .position(|call| call.starts_with("rename-noreplace "))
                .unwrap();
            assert!(integrity < published);
            // The clone checked its objects' connectivity itself: nothing reads them again.
            assert!(
                calls.iter().all(|call| !call.contains(" fsck ")),
                "{calls:?}"
            );
            assert_eq!(
                calls.iter().filter(|call| call.contains(" clone ")).count(),
                1
            );
        }
        for endpoint in [
            "git@github.com:alice/new.git",
            "ssh://git@github.com:22/alice/new.git",
        ] {
            let (sys, who, _) = fixture();
            let key = git(&sys, &who, None)
                .args([
                    "ls-remote",
                    "--get-url",
                    "--",
                    "https://github.com/alice/new.git",
                ])
                .display();
            sys.on(&key, Ran::ok(endpoint)).on(&key, Ran::ok(endpoint));
            let (repo, _, _) = script_clone(&sys, &who, false);
            clone_one(&sys, &who, "alice", &repo).unwrap();
            assert!(sys.stat(&Path::new(&who.home).join("new")).unwrap().dir);
        }
    }

    /// A guarded clone carries aterm's credential route and no keychain helper, and refuses to
    /// go on once the vault snapshot changed.
    #[test]
    fn guarded_clones_carry_the_route_and_refuse_newer_versions() {
        for changed in [false, true] {
            let (sys, who, _) = fixture();
            let current = || {
                if changed && sys.calls().iter().any(|call| call.contains(" clone ")) {
                    Err("injected newer vault version".into())
                } else {
                    Ok(())
                }
            };
            let (repo, _staged, _) = script_clone(&sys, &who, false);
            let result = clone_one_guarded(&sys, &who, &repo, &current);
            assert_eq!(result.is_ok(), !changed, "{result:?}");
            let final_path = Path::new(&who.home).join("new");
            assert_eq!(sys.stat(&final_path).is_ok(), !changed);
            let calls = sys.calls();
            let network: Vec<_> = calls
                .iter()
                .filter(|call| call.contains("ls-remote --symref") || call.contains(" clone "))
                .collect();
            assert_eq!(network.len(), 2, "{calls:?}");
            assert!(
                network
                    .iter()
                    .all(|call| call.contains("-c credential.helper= ")
                        && !call.contains("osxkeychain")),
                "{network:?}"
            );
            assert!(calls.iter().all(|call| !call.contains("--global")));
            assert_eq!(
                calls
                    .iter()
                    .any(|call| call.starts_with("rename-noreplace ")),
                !changed
            );
        }
    }

    /// Two clones of one repository race (`sync.clone` and a second `aterm init`): the one that
    /// finds it in place at its origin, before it clones or as it publishes, is done, and its
    /// staging goes; the racing worktree is never touched.
    #[test]
    fn a_clone_that_finds_the_repository_in_place_at_its_origin_is_done() {
        for at_publish in [false, true] {
            let (sys, who, _) = fixture();
            let staged =
                Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
            let (repo, _, clone_key) = script_clone(&sys, &who, false);
            let destination = Path::new(&who.home).join("new");
            script_existing(&sys, &who, &destination, "new");
            if at_publish {
                sys.on_stat(&clone_key, &destination, directory(who.uid));
                sys.on_write(&clone_key, &destination.join(".git/racer"), b"racing clone");
            } else {
                sys.stat_of(destination.to_str().unwrap(), directory(who.uid));
                sys.file(
                    destination.join(".git/racer").to_str().unwrap(),
                    b"racing clone",
                );
            }
            assert_eq!(
                clone_one(&sys, &who, "alice", &repo),
                Ok(Placed::InPlace),
                "{at_publish}"
            );
            assert_eq!(
                sys.read_file(&destination.join(".git/racer")).unwrap(),
                b"racing clone"
            );
            assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
            assert!(sys.stat(&staged).is_err(), "{at_publish}");
            assert_eq!(
                sys.calls().iter().any(|call| call.contains(" clone ")),
                at_publish,
                "{:?}",
                sys.calls()
            );
        }
    }

    #[test]
    fn failure_or_a_destination_created_during_clone_never_overwrites_owner_data() {
        for race in [false, true] {
            let (sys, who, _) = fixture();
            let staged =
                Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
            if !race {
                sys.on(
                    &git(&sys, &who, Some(&staged))
                        .args(["rev-parse", "--is-shallow-repository"])
                        .display(),
                    Ran::exit(1, "failure includes secret-like bytes that must not escape"),
                );
            }
            let (repo, _, clone_key) = script_clone(&sys, &who, false);
            let destination = Path::new(&who.home).join("new");
            if race {
                sys.on_stat(&clone_key, &destination, directory(who.uid));
                sys.on_write(&clone_key, &destination.join("owner.txt"), b"racing owner");
            }
            let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
            assert!(!error.contains("secret-like"));
            assert!(sys.stat(&staged).is_err());
            if race {
                assert_eq!(
                    sys.read_file(&destination.join("owner.txt")).unwrap(),
                    b"racing owner"
                );
                assert!(sys.read_file(&destination.join(".git/fixture")).is_err());
            } else {
                assert!(sys.stat(&destination).is_err());
                assert!(
                    sys.calls()
                        .iter()
                        .all(|call| !call.starts_with("rename-noreplace "))
                );
            }
        }
        for endpoint in [
            "https://mirror.invalid/alice/new.git",
            "https://github.com/other/new.git",
            "https://private-token@github.com/alice/new.git",
            "file:///owner/repository",
            "https://github.com/alice/new.git\nhttps://elsewhere.invalid/new.git",
            "",
        ] {
            let (sys, who, _) = fixture();
            let key = git(&sys, &who, None)
                .args([
                    "ls-remote",
                    "--get-url",
                    "--",
                    "https://github.com/alice/new.git",
                ])
                .display();
            sys.on(&key, Ran::ok(endpoint));
            let (repo, _, _) = script_clone(&sys, &who, false);
            let error = clone_one(&sys, &who, "alice", &repo).unwrap_err();
            assert!(!error.contains(endpoint) || endpoint.is_empty());
            assert!(sys.calls().iter().all(|call| !call.contains("--symref")
                && !call.contains(" clone ")
                && !call.contains("scratch allocated")));
        }
        let (sys, who, _) = fixture();
        let key = git(&sys, &who, None)
            .args([
                "ls-remote",
                "--get-url",
                "--",
                "https://github.com/alice/new.git",
            ])
            .display();
        sys.on(&key, Ran::ok("https://github.com/alice/new.git"))
            .on(&key, Ran::ok("https://mirror.invalid/alice/new.git"));
        let (repo, _, _) = script_clone(&sys, &who, false);
        assert!(clone_one(&sys, &who, "alice", &repo).is_err());
        assert!(sys.calls().iter().any(|call| call.contains("--symref")));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" clone ") && !call.contains("scratch allocated"))
        );
    }

    /// A promisor remote whose name is not UTF-8 makes a partial clone as any other does, and
    /// [`verify_integrity`] refuses it: [`partial_listing`] reads the config in the C locale.
    /// Under the account's UTF-8 locale git's `.` matches no such byte, the remote went unlisted
    /// and the check passed. The controls: a repository with no promisor passes; this one's
    /// remote is live (a read of an object the repository lacks, lazy fetches allowed, fetches it
    /// from there); and the same listing under the account's own locale misses it (a COULD NOT
    /// RUN where it does not, a host whose git matches those bytes there or lacks the locale).
    #[test]
    fn a_promisor_remote_named_in_bytes_that_are_not_utf8_is_refused() {
        use std::io::Write as _;
        use std::os::unix::ffi::OsStrExt as _;
        crate::items::github::need_native_git();
        let sys = Live::new();
        let home = Scratch::new(&sys, "github-promisor-bytes").unwrap();
        let mut who = user("fixture", sys.euid());
        who.home = home.path().to_string_lossy().into_owned();
        let run = |at: Option<&Path>, args: &[&str]| {
            text(
                &sys,
                &git(&sys, &who, at)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .args(crate::checkout::NO_SIDE_RUNS)
                    .args([
                        "-c",
                        "user.name=Fixture",
                        "-c",
                        "user.email=fixture@example.invalid",
                    ])
                    .args(args.iter().copied()),
                "isolated local Git fixture failed",
            )
            .unwrap()
        };
        let source = home.path().join("source");
        let repo = home.path().join("repo");
        run(
            None,
            &["init", "--quiet", "--template=", source.to_str().unwrap()],
        );
        sys.write_file(&source.join("code"), b"only the source has this\n", 0o600)
            .unwrap();
        run(Some(&source), &["add", "--", "code"]);
        run(Some(&source), &["commit", "--quiet", "-m", "fixture"]);
        let blob = run(Some(&source), &["rev-parse", "HEAD:code"]);
        let blob = blob.trim();
        run(
            None,
            &["init", "--quiet", "--template=", repo.to_str().unwrap()],
        );
        assert_eq!(
            verify_integrity(&sys, &who, &repo),
            Ok(()),
            "control: a repository with no promisor passes"
        );
        let mut config = std::fs::File::options()
            .append(true)
            .open(repo.join(".git/config"))
            .unwrap();
        config.write_all(b"[remote \"\xff\"]\n\turl = ").unwrap();
        config.write_all(source.as_os_str().as_bytes()).unwrap();
        config.write_all(b"\n\tpromisor = true\n").unwrap();
        drop(config);
        let unlisted = sys
            .run_complete_stdout(&git(&sys, &who, Some(&repo)).args([
                "config",
                "--get-regexp",
                PARTIAL_KEYS,
            ]))
            .unwrap();
        if unlisted.status != Some(1) {
            aterm_uds::host::could_not_run(format!(
                "git under this account's locale lists the remote named in bytes that are not \
                 UTF-8 ({}), so the C locale changes nothing here",
                unlisted.describe()
            ));
        }
        let refused = verify_integrity(&sys, &who, &repo).unwrap_err();
        assert!(refused.contains("partial"), "{refused}");
        // The control: git fetches through that remote what the repository lacks.
        let fetched = text(
            &sys,
            &git(&sys, &who, Some(&repo))
                .env("GIT_NO_LAZY_FETCH", "0")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(crate::checkout::NO_SIDE_RUNS)
                .args(["cat-file", "-t", blob]),
            "a read that may fetch lazily",
        )
        .unwrap();
        assert_eq!(fetched, "blob\n");
    }

    #[test]
    fn matching_real_dirty_worktree_on_an_alternate_branch_is_only_read() {
        crate::items::github::need_native_git();
        let sys = Live::new();
        let home = Scratch::new(&sys, "github-repo-fixture").unwrap();
        let mut who = user("fixture", sys.euid());
        who.home = home.path().to_string_lossy().into_owned();
        let path = home.path().join("existing");
        text(
            &sys,
            &git(&sys, &who, None)
                .args(["init", "--template=", "--initial-branch=main"])
                .arg(&path),
            "fixture init",
        )
        .unwrap();
        text(
            &sys,
            // A commit starts automatic maintenance too, whose fork would hold this drain's
            // pipe past git's exit on a loaded machine.
            &git(&sys, &who, Some(&path))
                .args(crate::checkout::NO_SIDE_RUNS)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "fixture",
                ]),
            "fixture commit",
        )
        .unwrap();
        text(
            &sys,
            &git(&sys, &who, Some(&path)).args([
                "remote",
                "add",
                "origin",
                "git@github.com:alice/existing.git",
            ]),
            "fixture remote",
        )
        .unwrap();
        text(
            &sys,
            &git(&sys, &who, Some(&path)).args(["switch", "-c", "topic"]),
            "fixture branch",
        )
        .unwrap();
        sys.write_file(&path.join("dirty.txt"), b"local owner edits", 0o600)
            .unwrap();
        let repo = Repo {
            owner: "alice".into(),
            name: "existing".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
        assert_eq!(
            sys.read_file(&path.join("dirty.txt")).unwrap(),
            b"local owner edits"
        );
        assert_eq!(
            git_read(&sys, &who, &path, &["symbolic-ref", "--quiet", "HEAD"])
                .unwrap()
                .trim(),
            "refs/heads/topic"
        );
    }

    #[test]
    fn deliberately_shallow_or_partial_matching_existing_checkout_is_preserved_without_integrity_probes()
     {
        let (sys, who, _) = fixture();
        let path = Path::new(&who.home).join("existing");
        script_existing(&sys, &who, &path, "existing");
        sys.refuse_complete_stdout(
            &git(&sys, &who, Some(&path))
                .args(["rev-parse", "--is-shallow-repository"])
                .display(),
            "history intentionally unavailable",
        );
        let repo = Repo {
            owner: "alice".into(),
            name: "existing".into(),
            default_branch: Some("main".into()),
            size_kib: 0,
            fork: false,
            archived: false,
            required_submodules: Vec::new(),
        };
        verify_existing(&sys, &who, "alice", &repo, &path).unwrap();
        assert!(sys.calls().iter().all(|call| !call.contains("shallow")
            && !call.contains("partialclone")
            && !call.contains("fsck")
            && !call.contains("fetch")));
    }

    #[test]
    fn dirty_new_checkout_or_incorrect_tracking_is_never_published() {
        for (args, bad) in [
            (
                vec!["status", "--porcelain=v1", "-z", "--untracked-files=all"],
                " D missing.txt\0",
            ),
            (
                vec!["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
                "refs/remotes/origin/other\n",
            ),
            (vec!["config", "--get", "branch.main.remote"], "other\n"),
            (
                vec!["config", "--get", "branch.main.merge"],
                "refs/heads/other\n",
            ),
        ] {
            let (sys, who, _) = fixture();
            let staged =
                Fake::scratch_path(Some(Path::new(&who.home)), "github-clone", 0).join("repo");
            sys.on(
                &git(&sys, &who, Some(&staged)).args(args).display(),
                Ran::ok(bad),
            );
            let (repo, _, _) = script_clone(&sys, &who, false);
            assert!(clone_one(&sys, &who, "alice", &repo).is_err());
            assert!(sys.stat(&Path::new(&who.home).join("new")).is_err());
            assert!(sys.stat(&staged).is_err());
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.starts_with("rename-noreplace "))
            );
        }
    }
}
