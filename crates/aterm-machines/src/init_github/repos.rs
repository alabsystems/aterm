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
use crate::repository_progress::{Phase, Progress};
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
        empty: std::sync::Mutex::new(None),
        lease: std::sync::Mutex::new(None),
        hooks: std::sync::Mutex::new(Vec::new()),
        reading: LastReading::default(),
    })
}

struct Repositories {
    user: User,
    config: Github,
    recorded: Vec<PathBuf>,
    /// Only this item's admitted source may reuse its actual empty advertisements.
    empty: std::sync::Mutex<Option<(super::source::Stamp, Vec<EmptyCheckout>)>>,
    /// Keep an unfinished-checkout acquisition owned through its completion probe.
    lease:
        std::sync::Mutex<Option<std::sync::Arc<crate::sync::repository_refresh::RepositoryPass>>>,
    /// What the last apply's clones made of their hook installers ([`Cloned::hooks`]), which
    /// the read-back's row says.
    hooks: std::sync::Mutex<Vec<String>>,
    reading: LastReading,
}

struct Scan {
    /// Every selected repository, including a destination that must stay untouched.
    all: usize,
    repositories: Vec<Repo>,
    missing: Vec<Repo>,
    existing: Vec<(Repo, PathBuf, Stands)>,
    dependencies: Vec<(Repo, PathBuf)>,
    unborn: Vec<(Repo, PathBuf)>,
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
        let _lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
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
            let source = match super::source::Stamp::read(sys, &self.user) {
                Ok(source) => source,
                Err(why) => return Probe::Unusable { why },
            };
            let selected = match super::develop_checkouts(sys, &self.user) {
                Ok(selected) => selected,
                Err(why) => return Probe::Unusable { why },
            };
            let current = || self.ensure_current(sys, &source, &selected, &|| Ok(()));
            let mut scan = match self.scan(sys, &token, &selected) {
                Err(why) => return Probe::Unusable { why },
                Ok(scan) => scan,
            };
            let mut recorded: BTreeSet<_> = self.recorded.iter().cloned().collect();
            recorded.extend(selected.iter().cloned());
            let receipt = self
                .empty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let mut empty = receipt
                .filter(|(stamp, _)| stamp == &source)
                .map_or_else(Vec::new, |(_, empty)| empty);
            empty.retain(|proof| !scan.unborn.iter().any(|(repo, _)| proof.is_for(repo)));
            *self
                .empty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = if empty.is_empty() {
                None
            } else {
                Some((source.clone(), empty.clone()))
            };
            let mut observed = Vec::new();
            for candidate in std::mem::take(&mut scan.unborn) {
                match observe_unborn(sys, &self.user, &candidate, &recorded, &current) {
                    Ok(UnbornObservation::Empty(proof)) => {
                        observed.push(candidate.0);
                        empty.push(*proof);
                    }
                    Ok(UnbornObservation::Owed) => scan.unborn.push(candidate),
                    Ok(UnbornObservation::Preserved(why)) => {
                        scan.blocked.push(format!("~/{}: {why}", candidate.0.name))
                    }
                    Err(why) if why.is_network() => {
                        return Probe::Blocked {
                            why: why.to_string(),
                        };
                    }
                    Err(why) => {
                        return Probe::Unusable {
                            why: why.to_string(),
                        };
                    }
                }
            }
            if !observed.is_empty() {
                let standing = match partition_with_empty(sys, &self.user, &observed, &empty) {
                    Ok(standing) => standing,
                    Err(why) => return Probe::Unusable { why },
                };
                if let Some(why) = named(&standing.git_waits) {
                    return Probe::Blocked { why };
                }
                scan.unborn.extend(standing.unborn);
                scan.dependencies.extend(standing.dependencies);
                scan.blocked.extend(standing.notes);
                for (repo, path) in standing.present {
                    if recorded.contains(&path) {
                        scan.recorded += 1;
                    } else {
                        scan.existing.push((
                            repo,
                            path,
                            Stands::Kept("origin currently advertises no commits".into()),
                        ));
                    }
                }
            }
            if let Err(why) = current() {
                return Probe::Blocked { why };
            }
            *self
                .empty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = if empty.is_empty() {
                None
            } else {
                Some((source.clone(), empty))
            };
            let present = scan.existing.len() + scan.recorded;
            self.reading.set(Some(Reading::Repositories {
                present,
                count: scan.all,
                signed_in: true,
            }));
            if scan.missing.is_empty()
                && scan.dependencies.is_empty()
                && scan.unborn.is_empty()
                && scan.moving() == 0
            {
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
                        "{} of {} repositories missing from ~{}; {} clean worktree(s) behind GitHub{}{}",
                        scan.missing.len(),
                        scan.all,
                        unfit(sys, &self.user, &scan.missing),
                        scan.moving(),
                        if scan.unborn.is_empty() {
                            String::new()
                        } else {
                            format!(
                                "; {} repositories need their first checkout",
                                scan.unborn.len()
                            )
                        },
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
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let source = super::source::Stamp::read(sys, &self.user)?;
        let selected = super::develop_checkouts(sys, &self.user)?;
        let mut recorded: BTreeSet<_> = self.recorded.iter().cloned().collect();
        recorded.extend(selected.iter().cloned());
        let current = || self.ensure_current(sys, &source, &selected, authorized);
        current()?;
        let Some(token) = auth_ready(sys, &self.user, &self.config)? else {
            return Err("GitHub authentication changed; no repositories cloned".into());
        };
        let scan = self.scan(sys, &token, &selected)?;
        if !scan.unborn.is_empty() && lease.is_none() {
            lease = Some(
                sys.repository_pass()?
                    .pass
                    .ok_or("another repository/converge pass holds align.lock")?,
            );
        }
        let receipt = self
            .empty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut empty = receipt
            .filter(|(stamp, _)| stamp == &source)
            .map_or_else(Vec::new, |(_, empty)| empty);
        empty.retain(|proof| !scan.unborn.iter().any(|(repo, _)| proof.is_for(repo)));
        *self
            .empty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = if empty.is_empty() {
            None
        } else {
            Some((source.clone(), empty.clone()))
        };
        let mut unfinished = Vec::new();
        let mut hydrated = false;
        for candidate in &scan.unborn {
            match acquire_unborn(
                sys,
                &self.user,
                std::slice::from_ref(candidate),
                &recorded,
                &current,
            ) {
                Ok(mut admitted) => {
                    hydrated |= admitted.is_empty();
                    empty.append(&mut admitted);
                }
                Err(why) => unfinished.push(why.to_string()),
            }
        }
        let home = Path::new(&self.user.home);
        let guard = CloneGuard::new(&current);
        let Cloned {
            left,
            mut failed,
            hooks,
            empty: mut cloned_empty,
            ..
        } = clone_all(sys, &self.user, &scan.missing, Some(&guard));
        empty.append(&mut cloned_empty);
        failed.extend(unfinished);
        let room = || short_of_room(sys.volume(home).as_ref(), &left, INIT_AGAIN);
        // A failed row is read back by nothing: it says what the clones that went made of their
        // hook installers, after why it failed, a guard that stopped the rest included; a done
        // one leaves that to the read-back's row.
        if let Err(why) = current() {
            return Err(stopped_after_clones(why, failed, room(), &hooks));
        }
        // A first commit can introduce required gitlinks. Reuse the admitted selection,
        // but inspect local dependencies again after acquiring it.
        let dependencies = if hydrated {
            let standing = partition_with_empty(sys, &self.user, &scan.repositories, &empty)?;
            if let Some(why) = named(&standing.git_waits) {
                failed.push(why);
            }
            standing.dependencies
        } else {
            scan.dependencies
        };
        for (repo, path) in dependencies {
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
            current()?;
            *self
                .empty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = if empty.is_empty() {
                None
            } else {
                Some((source.clone(), empty))
            };
            *self
                .lease
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = lease;
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
    fn ensure_current(
        &self,
        sys: &dyn System,
        source: &super::source::Stamp,
        selected: &BTreeSet<PathBuf>,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
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
        if *selected != super::develop_checkouts(sys, &self.user)? {
            return Err(
                "develop checkout selection changed while repositories were being refreshed".into(),
            );
        }
        Ok(())
    }

    fn scan(
        &self,
        sys: &dyn System,
        token: &Secret,
        selected: &BTreeSet<PathBuf>,
    ) -> Result<Scan, String> {
        admit_home(sys, &self.user)?;
        let listed = enumerate(sys, &self.user, &self.config, token)?;
        let receipt = self
            .empty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let empty = match &receipt {
            Some((source, empty)) if source.is_current(sys, &self.user)? => empty.as_slice(),
            _ => &[],
        };
        let Partition {
            missing,
            present,
            dependencies,
            unborn,
            notes,
            git_waits,
        } = partition_with_empty(sys, &self.user, &listed.repositories, empty)?;
        if let Some(why) = named(&git_waits) {
            return Err(why);
        }
        let mut aligned = Vec::new();
        let mut recorded = 0;
        let mut delegated: BTreeSet<_> = self.recorded.iter().cloned().collect();
        delegated.extend(selected.iter().cloned());
        for (repo, path) in present {
            if delegated.contains(&path) {
                recorded += 1;
                continue;
            }
            let stands = crate::checkout::read(sys, &self.user, &path, &repo.origin())
                .unwrap_or_else(|unread| Stands::Kept(unread.why.into()));
            aligned.push((repo, path, stands));
        }
        if let Some((source, _)) = &receipt
            && !empty.is_empty()
            && !source.is_current(sys, &self.user)?
        {
            return Err(
                "the configured repository source changed during empty-origin readback".into(),
            );
        }
        Ok(Scan {
            all: listed.repositories.len(),
            repositories: listed.repositories,
            missing,
            existing: aligned,
            dependencies,
            unborn,
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
    /// Matching worktrees without a commit need a fresh, owned remote observation.
    pub(super) unborn: Vec<(Repo, PathBuf)>,
    pub(super) notes: Vec<String>,
    /// Dependencies whose local inspection waits for a capable Git, without touching them.
    pub(super) git_waits: Vec<String>,
}

/// A complete empty advertisement, bound to this pass's unchanged physical checkout.
/// Only acquisition can construct it; local absence and catalogue metadata cannot.
#[derive(Clone, Debug)]
pub(super) struct EmptyCheckout {
    name: String,
    owner: String,
    branch: String,
    physical: [FileId; 3],
    refs: [u8; 32],
}

impl EmptyCheckout {
    pub(super) fn is_for(&self, repo: &Repo) -> bool {
        self.name == repo.name && self.owner == repo.owner
    }

    fn capture(sys: &dyn System, user: &User, repo: &Repo, path: &Path) -> Result<Self, String> {
        verify_existing(sys, user, &repo.owner, repo, path)?;
        clone_command(
            sys,
            user,
            git(sys, user, Some(path)),
            &repo.owner,
            repo,
            "origin",
        )?;
        owned_storage(sys, user, path)?;
        let branch = crate::checkout::unborn(sys, user, path)?
            .ok_or("an empty advertisement no longer matches the local checkout")?;
        let refs = git_read(
            sys,
            user,
            path,
            &["for-each-ref", "--format=%(objectname) %(refname)"],
        )?;
        let mut hash = aterm_digest::Sha256::new();
        hash.update(refs.as_bytes());
        Ok(Self {
            name: repo.name.clone(),
            owner: repo.owner.clone(),
            branch,
            physical: refresh_subjects(sys, path)
                .map_err(|_| "cannot identify the empty checkout")?,
            refs: hash.finalize(),
        })
    }

    fn current(&self, sys: &dyn System, user: &User, repo: &Repo, path: &Path) -> bool {
        self.is_for(repo)
            && Self::capture(sys, user, repo, path).is_ok_and(|now| {
                now.branch == self.branch && now.physical == self.physical && now.refs == self.refs
            })
    }
}

fn owned_storage(sys: &dyn System, user: &User, path: &Path) -> Result<(), String> {
    for path in [
        path.to_owned(),
        path.join(".git"),
        path.join(".git/objects"),
    ] {
        if !sys
            .stat(&path)
            .is_ok_and(|s| s.dir && !s.symlink && s.uid == user.uid)
        {
            return Err(
                "the unfinished checkout's storage is linked, foreign or unreadable".into(),
            );
        }
    }
    super::dependencies::complete_storage(sys, user, path)
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
    partition_with_empty(sys, user, all, &[])
}

pub(super) fn partition_with_empty(
    sys: &dyn System,
    user: &User,
    all: &[Repo],
    empty: &[EmptyCheckout],
) -> Result<Partition, String> {
    if all.is_empty() {
        admit_destination(sys, user)?;
    } else {
        admit_home(sys, user)?;
    }
    let mut standing = Partition::default();
    for (repo, read) in all.iter().zip(read_all(sys, user, all, empty)) {
        match read {
            Standing::Missing => standing.missing.push(repo.clone()),
            Standing::Present(path) => standing.present.push((repo.clone(), path)),
            Standing::Dependencies(path) => standing.dependencies.push((repo.clone(), path)),
            Standing::Unborn(path) => standing.unborn.push((repo.clone(), path)),
            Standing::GitWait(why) => standing.git_waits.push(why),
            Standing::Note(why) => standing.notes.push(why),
        }
    }
    Ok(standing)
}

/// How many repositories [`partition_with_empty`] reads at once. Each read is a few local Git
/// commands; one after another, fifty repositories took most of `aterm status`'s three seconds.
const READ_LANES: usize = 8;

/// Where one repository stands in [`Partition`].
enum Standing {
    Missing,
    Present(PathBuf),
    Dependencies(PathBuf),
    Unborn(PathBuf),
    GitWait(String),
    Note(String),
}

/// Each of `all`'s standing, in list order, read up to [`READ_LANES`] at once. The reads only
/// look: none changes a checkout, so their order does not matter.
fn read_all(sys: &dyn System, user: &User, all: &[Repo], empty: &[EmptyCheckout]) -> Vec<Standing> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let read: Vec<std::sync::OnceLock<Standing>> =
        all.iter().map(|_| std::sync::OnceLock::new()).collect();
    let lane = || {
        loop {
            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (Some(repo), Some(slot)) = (all.get(index), read.get(index)) else {
                return;
            };
            let _ = slot.set(standing(sys, user, repo, empty));
        }
    };
    // This thread is a lane too: a lane the system cannot start leaves its share to the others.
    std::thread::scope(|scope| {
        for _ in 1..all.len().min(READ_LANES) {
            let _ = std::thread::Builder::new().spawn_scoped(scope, lane);
        }
        lane();
    });
    read.into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("every repository is read once the lanes have joined")
        })
        .collect()
}

fn standing(sys: &dyn System, user: &User, repo: &Repo, empty: &[EmptyCheckout]) -> Standing {
    if HOME_FOLDERS
        .iter()
        .any(|folder| folder.eq_ignore_ascii_case(&repo.name))
    {
        return Standing::Note(format!(
            "~/{} is a standard home folder; left untouched",
            repo.name
        ));
    }
    let path = Path::new(&user.home).join(&repo.name);
    match sys.stat(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Standing::Missing,
        Ok(stat) if stat.dir && !stat.symlink => {
            match verify_existing(sys, user, &repo.owner, repo, &path) {
                Ok(()) => match crate::checkout::unborn(sys, user, &path) {
                    Ok(Some(_))
                        if !empty
                            .iter()
                            .any(|proof| proof.current(sys, user, repo, &path)) =>
                    {
                        Standing::Unborn(path)
                    }
                    Err(why) => Standing::Note(format!("~/{}: {why}", repo.name)),
                    Ok(_) => match super::dependencies::ready(sys, user, &path, repo) {
                        Ok(true) => Standing::Present(path),
                        Ok(false) => Standing::Dependencies(path),
                        Err(super::dependencies::Unready::Git(why)) => {
                            Standing::GitWait(format!("~/{}: {why}", repo.name))
                        }
                        Err(super::dependencies::Unready::Preserved(why)) => {
                            Standing::Note(format!("~/{}: {why}", repo.name))
                        }
                    },
                },
                Err(why) => Standing::Note(why),
            }
        }
        Ok(_) => Standing::Note(format!(
            "~/{} is occupied or linked; left untouched",
            repo.name
        )),
        Err(_) => Standing::Note(format!("cannot inspect ~/{}; left untouched", repo.name)),
    }
}

fn unborn_current(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    path: &Path,
    physical: [FileId; 3],
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(), String> {
    current()?;
    owned_storage(sys, user, path)?;
    verify_existing(sys, user, &repo.owner, repo, path)?;
    clone_command(
        sys,
        user,
        git(sys, user, Some(path)),
        &repo.owner,
        repo,
        "origin",
    )?;
    if refresh_subjects(sys, path).ok() != Some(physical) {
        return Err("unfinished checkout storage changed; preserved".into());
    }
    Ok(())
}

fn read_unborn(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    path: &Path,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<(Remote, String, [FileId; 3]), AcquisitionFailure> {
    current()?;
    owned_storage(sys, user, path)?;
    let physical =
        refresh_subjects(sys, path).map_err(|_| "cannot identify unfinished checkout storage")?;
    let branch = crate::checkout::unborn(sys, user, path)?.ok_or("unfinished HEAD changed")?;
    verify_existing(sys, user, &repo.owner, repo, path)?;
    let url = format!("https://github.com/{}/{}.git", repo.owner, repo.name);
    let command = clone_command(
        sys,
        user,
        git(sys, user, Some(path)),
        &repo.owner,
        repo,
        &url,
    )?;
    let bytes = crate::checkout::acquisition_read(
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
        "cannot read the unfinished repository's complete remote references",
    )?;
    let remote =
        remote(std::str::from_utf8(&bytes).map_err(|_| "invalid remote reference encoding")?)?;
    unborn_current(sys, user, repo, path, physical, current)?;
    Ok((remote, branch, physical))
}

#[derive(Debug)]
pub(super) enum UnbornObservation {
    Empty(Box<EmptyCheckout>),
    Owed,
    Preserved(String),
}

/// Conservative local repair debt, never proof that a remote is empty or current.
/// A later network-admitted observation can lift a stale catalogue hint.
pub(super) fn local_unborn(
    sys: &dyn System,
    user: &User,
    candidate: &(Repo, PathBuf),
    recorded: &BTreeSet<PathBuf>,
) -> Result<Option<String>, String> {
    let (repo, path) = candidate;
    let branch = crate::checkout::unborn(sys, user, path)?.ok_or("unfinished HEAD changed")?;
    if recorded.contains(path) {
        return Ok(Some(
            "the unfinished checkout is recorded in [develop]; left to the develop item".into(),
        ));
    }
    let Some(default) = repo.default_branch.as_deref() else {
        return Err("the unfinished checkout's default branch is unknown locally; waiting for an admitted remote refresh".into());
    };
    if default != branch {
        return Ok(Some(
            "the unfinished branch differs from the admitted default branch; preserved".into(),
        ));
    }
    crate::checkout::unborn_preservation(sys, user, path, &branch)
}

fn first_commit<'a>(
    remote: &'a Remote,
    branch: &str,
    path: &Path,
    recorded: &BTreeSet<PathBuf>,
) -> Result<&'a str, String> {
    if recorded.contains(path) {
        return Err("the unfinished checkout is recorded in [develop]; its first commit is left to the develop item".into());
    }
    match remote.head.as_deref() {
        Some(tip)
            if remote.branch.as_deref() == Some(branch)
                && remote
                    .refs
                    .get(&format!("refs/heads/{branch}"))
                    .map(String::as_str)
                    == Some(tip) =>
        {
            Ok(tip)
        }
        _ => Err("the unfinished branch differs from the advertised default; preserved".into()),
    }
}

/// Read-only classification for a network-admitted probe. Only a complete empty
/// advertisement returns a fact; a nonempty remote never starts a fetch or checkout.
pub(super) fn observe_unborn(
    sys: &dyn System,
    user: &User,
    candidate: &(Repo, PathBuf),
    recorded: &BTreeSet<PathBuf>,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<UnbornObservation, AcquisitionFailure> {
    let (repo, path) = candidate;
    let (remote, branch, physical) = read_unborn(sys, user, repo, path, current)?;
    let observed = if remote.head.is_none() && remote.refs.is_empty() {
        UnbornObservation::Empty(Box::new(EmptyCheckout::capture(sys, user, repo, path)?))
    } else {
        match first_commit(&remote, &branch, path, recorded) {
            Ok(_) => match crate::checkout::unborn_preservation(sys, user, path, &branch)? {
                None => UnbornObservation::Owed,
                Some(why) => UnbornObservation::Preserved(why),
            },
            Err(why) => UnbornObservation::Preserved(why),
        }
    };
    unborn_current(sys, user, repo, path, physical, current)?;
    Ok(observed)
}

/// Acquire only unfinished checkouts. Remote emptiness is a pass-owned readback;
/// a first commit uses the same guarded, non-overwriting native checkout boundary.
pub(super) fn acquire_unborn(
    sys: &dyn System,
    user: &User,
    unborn: &[(Repo, PathBuf)],
    recorded: &BTreeSet<PathBuf>,
    current: &(dyn Fn() -> Result<(), String> + Sync),
) -> Result<Vec<EmptyCheckout>, AcquisitionFailure> {
    let mut empty = Vec::new();
    for (repo, path) in unborn {
        let (remote, branch, physical) = read_unborn(sys, user, repo, path, current)?;
        let admitted = || unborn_current(sys, user, repo, path, physical, current);
        if remote.head.is_none() && remote.refs.is_empty() {
            empty.push(EmptyCheckout::capture(sys, user, repo, path)?)
        } else {
            let tip = first_commit(&remote, &branch, path, recorded)?;
            crate::checkout::hydrate_unborn(sys, user, path, &branch, tip, &admitted)?;
        }
        admitted()?;
    }
    Ok(empty)
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
/// `--no-lazy-fetch`, which a git before 2.45 refuses.
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

/// What one run of git tells of a worktree: `true`, the path below the top level (an empty line
/// at it), the top level, and where its index is (relative to the worktree for its own `.git`,
/// else absolute, as a linked worktree's or a submodule's is).
pub(super) const WORKTREE_READ: [&str; 6] = [
    "rev-parse",
    "--is-inside-work-tree",
    "--show-prefix",
    "--show-toplevel",
    "--git-path",
    "index",
];

/// The checkout at `path` is `owner`'s `repo` at its one GitHub origin ([`at_origin`]), and a
/// HEAD that names a commit it has comes with its index ([`written`]): what a pass may count
/// present, or align. A HEAD that names no commit here is the unfinished-checkout path's to
/// judge ([`crate::checkout::unborn`]: an empty repository's clone or one cut off before it
/// fetched, which [`observe_unborn`] and [`acquire_unborn`] decide from GitHub, apart from a
/// branch that names a commit it never fetched, which is left untouched).
pub(super) fn verify_existing(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    path: &Path,
) -> Result<(), String> {
    let index = at_origin(sys, user, owner, repo, path)?;
    written(sys, user, repo, path, &index)
}

/// The top-level worktree at `path` has `owner`'s `repo` as its one GitHub origin, whether or
/// not its clone finished: what a clone this process just ran to its end needs (one without a
/// checkout writes no index, which [`written`] would read as cut off).
pub(super) fn verify_origin(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    path: &Path,
) -> Result<(), String> {
    at_origin(sys, user, owner, repo, path).map(drop)
}

fn unread(repo: &Repo) -> String {
    format!(
        "cannot verify the existing Git worktree ~/{}; left untouched",
        repo.name
    )
}

/// [`verify_origin`], returning where the worktree's index is.
fn at_origin(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
    path: &Path,
) -> Result<PathBuf, String> {
    let read = git_read(sys, user, path, &WORKTREE_READ).map_err(|_| unread(repo))?;
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
    let index = match lines.next().map(str::trim) {
        Some(index) if !index.is_empty() => path.join(index),
        _ => return Err(unread(repo)),
    };
    let origin = git_read(
        sys,
        user,
        path,
        &["config", "--get-all", "remote.origin.url"],
    )
    .map_err(|_| unread(repo))?;
    let origins = origin.lines().collect::<Vec<_>>();
    if origins.len() != 1 || !origin_matches(origins[0], owner, &repo.name) {
        return Err(format!(
            "~/{} has a different or credential-bearing GitHub origin; left untouched",
            repo.name
        ));
    }
    Ok(index)
}

/// Whether the worktree at `path`, whose index git keeps at `index`, wrote the files of the
/// commit its HEAD names. git writes the index last, under its lock, so a `git clone` cut off
/// (killed, a power loss) after it fetched and before it wrote the files leaves a commit and no
/// index, lock or no lock; a finished clone with a commit always has one (an empty-tree
/// commit's included, measured). The read is [`crate::checkout::head_commit`], which fetches
/// nothing a partial clone lacks; a HEAD that names no commit here (exit 1) is not judged here
/// ([`verify_existing`]). A deliberately shallow or partial checkout is finished: nothing here
/// reads its history.
fn written(
    sys: &dyn System,
    user: &User,
    repo: &Repo,
    path: &Path,
    index: &Path,
) -> Result<(), String> {
    let head = sys
        .run_complete_stdout(&crate::checkout::head_commit(sys, user, path))
        .map_err(|_| unread(repo))?;
    match head.status {
        Some(0) => match sys.stat(index) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(format!(
                "~/{} has a commit and no index, as a clone cut off before it wrote its files \
                 leaves it; left untouched",
                repo.name
            )),
            Err(_) => Err(unread(repo)),
        },
        Some(1) => Ok(()),
        _ => Err(unread(repo)),
    }
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
        &mut Progress::new(&repo.name, Path::new(&user.home).join(&repo.name)),
    )
    .and_then(|placement| {
        placement.completion?;
        Ok(placement.placed)
    })
    .map_err(|why| why.to_string())
}

#[cfg(test)]
pub(super) fn clone_one(
    sys: &dyn System,
    user: &User,
    owner: &str,
    repo: &Repo,
) -> Result<Placed, String> {
    clone_one_with(
        sys,
        user,
        owner,
        repo,
        None,
        None,
        &mut Progress::new(&repo.name, Path::new(&user.home).join(&repo.name)),
    )
    .and_then(|placement| {
        placement.completion?;
        Ok(placement.placed)
    })
    .map_err(|why| why.to_string())
}

/// How many clones run at once ([`clone_all`]).
const LANES: usize = 4;

/// What [`clone_all`] did.
#[derive(Default)]
pub(super) struct Cloned {
    pub(super) empty: Vec<EmptyCheckout>,
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

/// Publication remains owned even if its later currentness/readback fails. The
/// publishing lane still owes its installer and staging disposition in that case.
struct ClonePlacement {
    placed: Placed,
    completion: Result<Option<EmptyCheckout>, AcquisitionFailure>,
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
            let mut progress = Progress::new(&repo.name, home.join(&repo.name));
            progress.enter(Phase::WaitingForSpace);
            sys.repository_progress(&progress);
            done.left.push(name);
            continue;
        }
        reserved = reserved.saturating_add(need(repo));
        let progress = Progress::new(&repo.name, home.join(&repo.name));
        sys.repository_progress(&progress);
        queue.push_back((repo, progress));
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
                    let (repo, mut progress, staging) = {
                        let mut queue = queue
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let Some((repo, progress)) = queue.pop_front() else {
                            return;
                        };
                        (repo, progress, Scratch::new_in(sys, home, "github-clone"))
                    };
                    // It reads the snapshot first and between its steps.
                    let parent = match staging {
                        Ok(staging) => clone_one_with(
                            sys,
                            user,
                            &repo.owner,
                            repo,
                            guard,
                            Some(staging),
                            &mut progress,
                        ),
                        Err(_) => Err(AcquisitionFailure::Local(
                            "cannot allocate private repository staging in this home".to_owned(),
                        )),
                    };
                    let published = matches!(
                        parent,
                        Ok(ClonePlacement {
                            placed: Placed::Published | Placed::PublishedStagingLeft,
                            ..
                        })
                    );
                    // Published with its staging left behind, whatever its dependencies do.
                    let staging_left = matches!(
                        parent,
                        Ok(ClonePlacement {
                            placed: Placed::PublishedStagingLeft,
                            ..
                        })
                    );
                    let result = parent.and_then(|placement| {
                        let empty = placement.completion?;
                        progress.enter(Phase::Dependencies);
                        sys.repository_progress(&progress);
                        let current = || guard.map_or(Ok(()), |guard| (guard.current)());
                        super::dependencies::repair(
                            sys,
                            user,
                            &home.join(&repo.name),
                            repo,
                            &current,
                        )?;
                        Ok((placement.placed, empty))
                    });
                    if result.is_err() && guard.is_some_and(|guard| (guard.current)().is_err()) {
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    // Only a clone this lane published: one found in place is another's.
                    let hooks = published
                        .then(|| install_hooks(sys, user, repo, guard))
                        .flatten();
                    match &result {
                        Ok(_) => progress.enter(Phase::Ready),
                        Err(why) => why.record_progress(&mut progress),
                    }
                    progress.usable = result.is_ok();
                    sys.repository_progress(&progress);
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
            Ok((_, empty)) => {
                done.cloned.push(repo.clone());
                done.empty.extend(empty);
            }
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

/// Whether another clone of `repo` (`sync.clone`, a second `aterm init`) left it in place at
/// `destination`: a worktree at its origin whose HEAD names a commit it has, with its index
/// ([`verify_existing`], then [`crate::checkout::unborn`] reading `None`). A HEAD that names no
/// commit (a clone that has not fetched yet, or one cut off before it did) is no repository in
/// place: the clone that meets it leaves it untouched, and the next pass's standing read takes it
/// up as the unfinished checkout it is.
fn in_place(sys: &dyn System, user: &User, owner: &str, repo: &Repo, destination: &Path) -> bool {
    verify_existing(sys, user, owner, repo, destination).is_ok()
        && matches!(crate::checkout::unborn(sys, user, destination), Ok(None))
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
    progress: &mut Progress,
) -> Result<ClonePlacement, AcquisitionFailure> {
    let current = || guard.map_or(Ok(()), |guard| (guard.current)());
    current()?;
    let home = Path::new(&user.home);
    let destination = home.join(&repo.name);
    match sys.stat(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        // Another clone of it (`sync.clone`, a second `aterm init`) published it since the step
        // read what was missing: a worktree in place at this repository's origin is done.
        Ok(_) if in_place(sys, user, owner, repo, &destination) => {
            progress.placed = true;
            return Ok(ClonePlacement {
                placed: Placed::InPlace,
                completion: Ok(None),
            });
        }
        _ => {
            return Err(format!("~/{} appeared before cloning; left untouched", repo.name).into());
        }
    }
    let url = format!("https://github.com/{owner}/{}.git", repo.name);
    progress.enter(Phase::Connecting);
    sys.repository_progress(progress);
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
    progress.staging = Some(clone.clone());
    sys.repository_progress(progress);
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
            "--progress",
            "--",
            &url,
        ])
        .arg(&clone);
    let acquired = sys.run_repository(&command, progress).map_err(|_| {
        AcquisitionFailure::Local("GitHub clone failed; existing destination left untouched".into())
    })?;
    if !acquired.success() {
        return Err(AcquisitionFailure::Network(
            "GitHub clone failed; existing destination left untouched".into(),
        ));
    }
    progress.enter(Phase::Verifying);
    sys.repository_progress(progress);
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
    progress.enter(Phase::Placing);
    sys.repository_progress(progress);
    if sys.rename_noreplace(&clone, &destination).is_err() {
        // Another clone of it published first: a worktree in place at this repository's origin
        // is done, and this one's staging goes. Anything else there is left untouched.
        if in_place(sys, user, owner, repo, &destination) {
            staging.close().map_err(|_| {
                "the repository is in place, but this clone's private staging could not be removed"
            })?;
            return Ok(ClonePlacement {
                placed: Placed::InPlace,
                completion: Ok(None),
            });
        }
        return Err(format!(
            "cannot publish ~/{} without replacing an existing destination; left untouched",
            repo.name
        )
        .into());
    }
    let placed = if staging.close().is_ok() {
        Placed::Published
    } else {
        Placed::PublishedStagingLeft
    };
    progress.placed = true;
    let completion: Result<Option<EmptyCheckout>, AcquisitionFailure> = (|| {
        current()?;
        if source.head.is_none() && source.refs.is_empty() {
            Ok(Some(EmptyCheckout::capture(sys, user, repo, &destination)?))
        } else {
            Ok(None)
        }
    })();
    Ok(ClonePlacement { placed, completion })
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
pub(super) mod tests;
