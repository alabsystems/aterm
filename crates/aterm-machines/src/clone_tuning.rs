// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Clone tuning: every clone of the Secrets vault's repository inventory converges, on each sync
//! pass (`sync.clone-tuning`, [`crate::sync`]) and on a bare `aterm doctor`
//! ([`crate::init::Doctor::clone_tuning`]), to the git client tuning its repository declares as
//! data in [`DECLARATION`] (trust's GIT-CLIENT-TUNING-1).
//!
//! A clone's own hook installer tunes only a clone aterm's setup has just published
//! (`scripts/install-git-hooks.sh`, [`crate::init_github`]): one found in place never ran it, and
//! running it again would repeat a 6 s self-test, may reach the network and runs the clone's own
//! code. This runs none of a clone's code. It reads the declaration as data, from origin's
//! default branch as the clone last fetched it ([`crate::checkout::fetched_head_by`]: `git config
//! --blob <blob> --no-includes`, after a size check, never the worktree's copy), and applies
//! only aterm's allowlist: `core.untrackedCache` and `feature.manyFiles`, set to `true`, in the
//! clone's own config; `core.untrackedCache` alone in each populated submodule's; and index
//! format 4 for the superproject's index. Every other declared key is named, never applied.
//!
//! The clones are the inventory's present ones ([`crate::init_github::vault_clones`], whose
//! dependency reads turn off every hook and filter driver a repository names, as the writes
//! here do); under an explicit `[github]` profile, `[github] enabled = false` or a System State
//! hold of GitHub repositories none is in scope, and a folder the inventory does not name is
//! never touched.
//!
//! Every git run here stays on this machine: none fetches an object a partial clone lacks
//! (`GIT_NO_LAZY_FETCH`, and `--no-lazy-fetch` where git takes it: [`Git`], so a git that
//! neither takes the option nor was seen to obey the variable tunes nothing). A write runs
//! nothing else: no hook, whether a hooks folder's or one a config names, no filter driver, and
//! no fsmonitor daemon ([`quiet`], [`checkout::Programs`]); a clone whose config names a hook or
//! filter aterm cannot name to turn off is left as it is, and so is one whose config is a link,
//! which git would write through.
//!
//! The rules are the hook installers' (`scripts/hooks/_git_client_tuning.sh`):
//! * A key set in any scope (`git config --show-scope`, read with no `-c`) is kept, and so is an
//!   explicit `index.version` with its index.
//! * A clone whose `config.lock` or `index.lock` is there (another git holds it, or one that
//!   stopped left it) is left for the next pass: never waited on, and no lock is removed. A lock
//!   another git takes after the plan looked stops the clone where it is, for the next pass.
//! * The untracked cache is correct only where a directory's mtime moves when an entry in it
//!   changes. Git's own self-test (`update-index --test-untracked-cache`, about 6 s) runs once,
//!   in a scratch repository under `<state>/converge`, on that folder's volume, and its verdict
//!   (exit 0 or 1; any other end is none, for the next pass) is kept. Only a repository whose top
//!   is on that volume can get either key (`feature.manyFiles`
//!   turns the cache on too): one on another volume, or on one whose self-test failed, gets
//!   neither. The volume is judged at each repository's top; a folder mounted inside a worktree
//!   is not judged apart.
//!
//! What aterm set is recorded in `<state>/converge/`[`RECORD`], never in the clone (whose config
//! trust-cg's host boundary judges key by key), per git directory ([`git_dir_key`]), so a clone
//! made again at the same path starts afresh. A key removed after aterm set it is the
//! person's opt-out and is not set again; a key the declaration drops is unset only while the
//! repository's own config still holds aterm's value.

use crate::checkout::{self, NO_SIDE_RUNS, NoLazyFetch, NotFetched};
use crate::engine::Place;
use crate::init::Holds;
use crate::init_github::{self, VaultClones};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, FileId, Ran, Scratch, System};
use aterm_converge::{Class, Layer, Remedy, Requires, StoodAside};
use aterm_types::macos_setup::Setting;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Where a repository declares its tuning, in git config syntax: the fleet's one path.
pub const DECLARATION: &str = "scripts/git-clone.gitconfig";

/// The largest declaration read: far above any real one, so a blob that is not one is refused
/// before git parses it.
const DECLARATION_MAX: u64 = 64 * 1024;

/// The largest `.gitmodules` read, for the same reason.
const GITMODULES_MAX: u64 = 256 * 1024;

/// What aterm set, and each volume's self-test, under `<state>/converge` (0600).
pub const RECORD: &str = "clone-tuning.json";

/// The largest record read.
const RECORD_MAX: usize = 4 * 1024 * 1024;

/// Taken across an apply, so a doctor and a sync pass never write the record at once.
const LOCK: &str = "clone-tuning.lock";

/// How long an apply waits for another's [`LOCK`]: one self-test and a few config writes.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// How deep nested submodules are followed, and how many repositories one clone may hold.
const SUBMODULE_DEPTH: usize = 8;
const REPOSITORIES_MAX: usize = 512;

/// The doctor row's id stem (`init.github.tuning@<user>`), as `--only` names it.
pub const ID: &str = "init.github.tuning";

/// The doctor row's family, beside init's GitHub items.
pub const FAMILY: &str = "github";

const UNTRACKED_CACHE: &str = "core.untrackedCache";
const MANY_FILES: &str = "feature.manyFiles";

/// aterm's allowlist for a clone's own config. Both keys turn the untracked cache on.
const SUPERPROJECT_KEYS: [&str; 2] = [UNTRACKED_CACHE, MANY_FILES];

/// aterm's allowlist for a submodule's config: trust-cg's host boundary refuses
/// `feature.manyFiles` in its repository's config.
const SUBMODULE_KEYS: [&str; 1] = [UNTRACKED_CACHE];

/// The one index format aterm converts a superproject's index to.
const INDEX_VERSION: u32 = 4;

/// The clones as one row reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// Nothing is in scope, and why.
    NotApplicable(String),
    /// Every clone is as it declares, with what was left unapplied.
    Current(String),
    /// Some clone differs: each one, and what it becomes.
    Due {
        /// Each differing clone and how.
        from: String,
        /// What they become.
        to: String,
    },
    /// Only clones another git holds differ.
    Deferred(String),
}

impl Reading {
    /// The engine's probe for this reading.
    #[must_use]
    pub fn probe(self) -> Probe {
        match self {
            Self::NotApplicable(why) => Probe::NotApplicable { why },
            Self::Current(now) => Probe::Ok { now },
            Self::Due { from, to } => Probe::Change { from, to },
            Self::Deferred(why) => Probe::Deferred { why },
        }
    }
}

/// What an [`apply`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuned {
    /// What it tuned, for the row.
    pub said: String,
    /// Why it stood aside, wholly or for some clone: nothing was in scope as it began, or
    /// another git held a clone it would write. `None` once every differing clone is tuned.
    pub aside: Option<String>,
}

/// What the differing clones become.
const TO: &str = "tuned as each declares in its scripts/git-clone.gitconfig";

/// What an apply that tuned no clone says.
const NOTHING: &str = "nothing tuned";

/// Read every clone in scope and say how it differs from its declaration. Writes nothing and
/// takes no lock.
///
/// # Errors
/// The context, init profile, inventory or [`RECORD`] cannot be read.
pub fn read(sys: &dyn System, ctx: &Ctx, state: &Path, holds: &Holds) -> Result<Reading, String> {
    let (git, tops) = match scope(sys, ctx, holds)? {
        Scope::Out(why) => return Ok(Reading::NotApplicable(why)),
        Scope::Clones { git, tops } => (git, tops),
    };
    let dir = converge_dir(state);
    let Some(record) = Record::read(&dir)? else {
        return Ok(Reading::NotApplicable(newer(&dir)));
    };
    let clones = plan_all(sys, git, &tops, &record, test_volume(sys, &dir));
    Ok(summary(&clones, sys.machine()))
}

/// Tune every clone in scope as [`read`] plans it: run the self-test first where one is owed,
/// then write each key, record it, and convert the superproject's index. Stands aside
/// ([`Tuned::aside`]) where nothing is in scope as it begins, and for each clone another git
/// holds.
///
/// # Errors
/// The lock, the record or the self-test cannot be had, or a write failed (each one named; the
/// rest still ran).
pub fn apply(sys: &dyn System, ctx: &Ctx, state: &Path, holds: &Holds) -> Result<Tuned, String> {
    apply_with(
        sys,
        ctx,
        state,
        holds,
        &Seams {
            self_test: &self_test,
            planned: &|| {},
            lock_wait: LOCK_WAIT,
        },
    )
}

/// Git's untracked-cache self-test in a scratch repository under a folder: whether it passed
/// ([`self_test`]).
type SelfTest<'a> = dyn Fn(&dyn System, Git<'_>, &Path) -> Result<bool, String> + 'a;

/// What [`apply_with`] runs that a test gives in place of the real step.
struct Seams<'a> {
    /// The self-test ([`self_test`]).
    self_test: &'a SelfTest<'a>,
    /// Run once the clones are planned, before the first write: nothing, outside a test.
    planned: &'a dyn Fn(),
    /// How long to wait for another aterm's tuning to end ([`LOCK_WAIT`], outside a test).
    lock_wait: Duration,
}

/// [`apply`], its seams given.
fn apply_with(
    sys: &dyn System,
    ctx: &Ctx,
    state: &Path,
    holds: &Holds,
    seams: &Seams<'_>,
) -> Result<Tuned, String> {
    let (git, tops) = match scope(sys, ctx, holds)? {
        Scope::Out(why) => return Ok(aside(why)),
        Scope::Clones { git, tops } => (git, tops),
    };
    let dir = converge_dir(state);
    aterm_uds::private_file::ensure_private_dir(&dir)
        .map_err(|why| format!("{}: {why}", dir.display()))?;
    let _held = match aterm_uds::private_file::PrivateLock::take(
        &dir,
        LOCK,
        seams.lock_wait,
        "another aterm tunes the vault's clones now",
    ) {
        Ok(held) => held,
        Err(why) if why.kind() == std::io::ErrorKind::WouldBlock => {
            return Ok(aside(
                "another aterm tunes the vault's clones now; the next pass tunes them".into(),
            ));
        }
        Err(why) => return Err(format!("{}: {why}", dir.join(LOCK).display())),
    };
    let Some(mut record) = Record::read(&dir)? else {
        return Ok(aside(newer(&dir)));
    };
    let test = test_volume(sys, &dir);
    let mut clones = plan_all(sys, git, &tops, &record, test);
    if let Some(dev) = clones.iter().find_map(|clone| clone.pending) {
        let passed = (seams.self_test)(sys, git, &dir)?;
        record.volumes.insert(
            dev.to_string(),
            Tested {
                passed,
                at_s: sys.now(),
            },
        );
        record.write(&dir)?;
        clones = plan_all(sys, git, &tops, &record, test);
    }
    (seams.planned)();
    let mut done = Vec::new();
    let mut failed = Vec::new();
    let mut held = Vec::new();
    for clone in &clones {
        if let Some(why) = &clone.held {
            held.push(why.clone());
            continue;
        }
        if !clone.differs() {
            continue;
        }
        let tuning = tune(sys, git, clone, &mut record, &dir, &mut failed)?;
        done.extend(tuning.done);
        held.extend(tuning.held);
    }
    if !failed.is_empty() {
        let mut why = failed.join("; ");
        if !done.is_empty() {
            why.push_str(&format!("; tuned {}", done.join(", ")));
        }
        return Err(why);
    }
    Ok(Tuned {
        said: if done.is_empty() {
            NOTHING.to_owned()
        } else {
            format!("tuned {}", done.join(", "))
        },
        aside: (!held.is_empty()).then(|| format!("{}; the next pass tunes it", held.join("; "))),
    })
}

/// What tuning one clone came to.
struct Tuning {
    /// What it tuned, where it changed anything (the failed writes named apart).
    done: Option<String>,
    /// The lock another git took after the plan looked, which stopped the rest of its writes:
    /// left for the next pass.
    held: Option<String>,
}

/// Write one planned clone: each repository's keys, each recorded once set, then the
/// superproject's index. A failed write is named in `failed` and the rest still run; a lock
/// another git took since the plan stops the clone where it is.
///
/// # Errors
/// The record cannot be written.
fn tune(
    sys: &dyn System,
    git: Git<'_>,
    clone: &Plan,
    record: &mut Record,
    dir: &Path,
    failed: &mut Vec<String>,
) -> Result<Tuning, String> {
    let mut wrote = false;
    for repo in &clone.repos {
        if repo.set.is_empty() && repo.unset.is_empty() {
            continue;
        }
        let quiet = match quiet(sys, git, &repo.path) {
            Ok(quiet) => quiet,
            Err(why) => {
                failed.push(format!("{}: {why}", repo.label));
                continue;
            }
        };
        let writes = repo
            .set
            .iter()
            .map(|key| (*key, true))
            .chain(repo.unset.iter().map(|key| (*key, false)));
        let mut changed = false;
        let mut locked = false;
        for (key, set) in writes {
            let wrote = if set {
                write(sys, &quiet, &["config", "--local", key, "true"])
            } else {
                write(sys, &quiet, &["config", "--local", "--unset", key])
            };
            match wrote {
                Ok(()) => {
                    if set {
                        record.own(repo, key);
                    } else {
                        record.disown(repo, key);
                    }
                    changed = true;
                }
                Err(Wrote::Locked) => {
                    locked = true;
                    break;
                }
                Err(Wrote::Failed(why)) => failed.push(format!(
                    "{}: {}{key}: {why}",
                    repo.label,
                    if set { "" } else { "unset " }
                )),
            }
        }
        // Set first, then recorded: a record that cannot be written forgets aterm's claim, and
        // a key aterm does not claim is only ever kept.
        if changed {
            record.write(dir)?;
            wrote = true;
        }
        if locked {
            return Ok(Tuning {
                done: wrote.then(|| clone.label.clone()),
                held: Some(lock_held(&repo.label, "config.lock")),
            });
        }
    }
    let converted = match clone.repos.first() {
        Some(top) if clone.index.is_some() => convert_index(sys, git, top),
        _ => Ok(None),
    };
    let done = wrote.then(|| clone.label.clone());
    Ok(match converted {
        Ok(Some(from)) => Tuning {
            done: Some(format!(
                "{} (index format {from} -> {INDEX_VERSION})",
                clone.label
            )),
            held: None,
        },
        Ok(None) => Tuning { done, held: None },
        Err(Wrote::Locked) => Tuning {
            done,
            held: Some(lock_held(&clone.label, "index.lock")),
        },
        Err(Wrote::Failed(why)) => {
            failed.push(format!(
                "{}: index format not converted: {why}; the next pass tries again",
                clone.label
            ));
            Tuning { done, held: None }
        }
    })
}

/// Why a clone is left for the next pass at a lock that is there: another git holds it, or one
/// that stopped left it (aterm never removes it).
fn lock_held(label: &str, lock: &str) -> String {
    format!("{label}: its {lock} is there (another git holds it, or one that stopped left it)")
}

/// An apply that tuned nothing and stood aside, and why.
fn aside(why: String) -> Tuned {
    Tuned {
        said: NOTHING.to_owned(),
        aside: Some(why),
    }
}

/// System State's holds as they stand when called: [`item`]'s row reads them at its probe and
/// again at its apply.
pub type HoldsNow = Box<dyn Fn() -> Holds + Send + Sync>;

/// The doctor's row for this login ([`read`] and [`apply`] over `state`, each under the holds
/// `holds` reads as it starts).
///
/// # Errors
/// The context holds no single user.
pub fn item(ctx: &Ctx, state: &Path, holds: HoldsNow) -> Result<Box<dyn Item>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("clone tuning requires exactly one local user".into());
    };
    Ok(Box::new(CloneTuning {
        user: user.name.clone(),
        state: state.to_path_buf(),
        holds,
        said: Mutex::new(None),
        aside: Mutex::new(None),
    }))
}

/// Where [`item`] binds ([`crate::engine::bind`]): a relay-class repair of the account's own on
/// the workstation, which bare `aterm doctor` applies.
#[must_use]
pub fn place() -> Place {
    Place {
        layer: Layer::Workstation,
        class: Class::Relay,
        requires: Requires::Account,
        remedy: Remedy::Command(format!("aterm doctor --only {ID}")),
    }
}

/// [`item`]'s row.
struct CloneTuning {
    user: String,
    state: PathBuf,
    holds: HoldsNow,
    /// What the last apply said it tuned, the row's detail.
    said: Mutex<Option<String>>,
    /// Why the last apply stood aside.
    aside: Mutex<Option<StoodAside>>,
}

impl Item for CloneTuning {
    fn id(&self) -> String {
        format!("{ID}@{}", self.user)
    }

    fn family(&self) -> &'static str {
        FAMILY
    }

    fn title(&self) -> String {
        "git tuning of the vault's clones".to_owned()
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        match read(sys, ctx, &self.state, &(self.holds)()) {
            Ok(reading) => reading.probe(),
            Err(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let tuned = apply(sys, ctx, &self.state, &(self.holds)())?;
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
            tuned.aside.map(StoodAside::Waiting);
        *self.said.lock().unwrap_or_else(PoisonError::into_inner) = Some(tuned.said);
        Ok(())
    }

    fn detail(&self, _: &dyn System, _: &Ctx) -> Option<String> {
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Which clones are in scope, and the git that reads them.
enum Scope<'c> {
    Out(String),
    Clones { git: Git<'c>, tops: Vec<PathBuf> },
}

fn scope<'c>(sys: &dyn System, ctx: &'c Ctx, holds: &Holds) -> Result<Scope<'c>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("clone tuning requires exactly one local user".into());
    };
    if holds.holds(Setting::Repositories) {
        return Ok(Scope::Out(holds.why(Setting::Repositories)));
    }
    if !init_github::git_installed(sys) {
        return Ok(Scope::Out(format!(
            "no git on this {} reads a clone's settings yet",
            sys.machine()
        )));
    }
    let tops = match init_github::vault_clones(sys, ctx)? {
        VaultClones::Off => {
            return Ok(Scope::Out(
                "off: init.toml's [github] enabled = false".into(),
            ));
        }
        VaultClones::Profile => {
            return Ok(Scope::Out(
                "init.toml's [github] profile names its own repositories, and aterm tunes the \
                 vault inventory's clones alone"
                    .into(),
            ));
        }
        VaultClones::NoInventory => {
            return Ok(Scope::Out(format!(
                "no repository inventory from the Secrets vault is on this {}",
                sys.machine()
            )));
        }
        VaultClones::Present(tops) => tops,
    };
    if tops.is_empty() {
        return Ok(Scope::Out(format!(
            "no clone of the vault's repository inventory is on this {}",
            sys.machine()
        )));
    }
    guarded(sys, user, tops)
}

/// [`scope`] once `tops` holds a clone: the git that reads them, kept from fetching what a
/// partial clone lacks as this git keeps a read from it ([`NoLazyFetch::of`], asked once, before
/// any clone is read); or, where it does neither (a git before 2.44 that ignores
/// `GIT_NO_LAZY_FETCH`), why no clone is in scope, in the words of [`System::machine`].
///
/// # Errors
/// Git could not be asked ([`NoLazyFetch::of`]).
fn guarded<'c>(sys: &dyn System, user: &'c User, tops: Vec<PathBuf>) -> Result<Scope<'c>, String> {
    let Some(off) = NoLazyFetch::of(sys, user)? else {
        let machine = sys.machine();
        return Ok(Scope::Out(format!(
            "this {machine}'s git neither takes --no-lazy-fetch (git 2.44 has it) nor obeys \
             GIT_NO_LAZY_FETCH (the 2.39.4 security release has it), one of which keeps aterm's \
             reads of a clone on this {machine}"
        )));
    };
    Ok(Scope::Clones {
        git: Git { user, off },
        tops,
    })
}

/// The git every run here is ([`Git::local`]): `user`'s, kept from fetching an object a partial
/// clone lacks the way this git keeps a read from it (`off`, which [`scope`] asks once a pass).
#[derive(Clone, Copy)]
struct Git<'u> {
    user: &'u User,
    off: NoLazyFetch,
}

impl Git<'_> {
    /// [`checkout::git`] in `path` that never fetches an object a partial clone lacks: its
    /// `GIT_NO_LAZY_FETCH`, and `--no-lazy-fetch` where this git takes it
    /// ([`NoLazyFetch::git`]), never an option it refuses.
    fn local(self, sys: &dyn System, path: Option<&Path>) -> Cmd {
        self.off.git(sys, self.user, path)
    }
}

fn converge_dir(state: &Path) -> PathBuf {
    state.join(aterm_converge::FileJournal::DIR)
}

/// Why a [`RECORD`] a newer aterm wrote leaves the clones to it.
fn newer(dir: &Path) -> String {
    format!(
        "{} is a newer aterm's, which tunes these clones; this one changes nothing",
        dir.join(RECORD).display()
    )
}

/// The volume the self-test's scratch repository under `dir` is on: `dir`'s, or, before it
/// exists, its nearest existing ancestor's, which a folder made there inherits.
fn test_volume(sys: &dyn System, dir: &Path) -> Option<u64> {
    dir.ancestors()
        .find_map(|at| sys.file_id(at).ok())
        .map(|id| id.dev)
}

/// What aterm set, and what each self-test said ([`RECORD`]). Any change to its shape takes a
/// new [`SCHEMA`], which an older aterm reads as a newer one's and leaves alone.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema: u32,
    /// By volume (`st_dev`, in decimal).
    #[serde(default)]
    volumes: BTreeMap<String, Tested>,
    /// By git directory: [`git_dir_key`] of the repository's common directory, which holds its
    /// config.
    #[serde(default)]
    repositories: BTreeMap<String, Owned>,
}

const SCHEMA: u32 = 2;

/// A record's schema alone, read before its shape is.
#[derive(Deserialize)]
struct Schema {
    schema: u32,
}

/// One volume's self-test.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Tested {
    passed: bool,
    at_s: u64,
}

/// The keys aterm set in one repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Owned {
    /// Where the repository was when aterm last wrote it, for a person reading the record.
    path: String,
    set: Vec<String>,
}

impl Record {
    /// The record, a fresh one where there is none, or `None` where a newer aterm wrote it.
    fn read(dir: &Path) -> Result<Option<Self>, String> {
        match aterm_uds::private_file::read_private(dir, RECORD, RECORD_MAX) {
            Ok(bytes) => {
                let not_ours = |why: aterm_json::Error| {
                    format!(
                        "{}: not aterm's clone tuning record: {why}",
                        dir.join(RECORD).display()
                    )
                };
                let Schema { schema } = aterm_json::from_slice(&bytes).map_err(not_ours)?;
                if schema > SCHEMA {
                    return Ok(None);
                }
                if schema < SCHEMA {
                    // An older aterm's: what it claims is forgotten, so each key it set is kept
                    // as the person's, and its self-tests run again.
                    return Ok(Some(Self {
                        schema: SCHEMA,
                        ..Self::default()
                    }));
                }
                aterm_json::from_slice(&bytes).map(Some).map_err(not_ours)
            }
            Err(aterm_uds::private_file::PrivateFileError::Missing) => Ok(Some(Self {
                schema: SCHEMA,
                ..Self::default()
            })),
            Err(why) => Err(format!("{}: {why}", dir.join(RECORD).display())),
        }
    }

    fn write(&self, dir: &Path) -> Result<(), String> {
        let bytes = aterm_json::to_vec(self).map_err(|why| why.to_string())?;
        aterm_uds::private_file::write_private(dir, RECORD, &bytes)
            .map_err(|why| format!("{}: {why}", dir.join(RECORD).display()))
    }

    /// The keys aterm set in the repository whose git directory is `git_dir`.
    fn owned(&self, git_dir: &str) -> &[String] {
        self.repositories
            .get(git_dir)
            .map_or(&[], |owned| owned.set.as_slice())
    }

    fn own(&mut self, repo: &Repo, key: &str) {
        let entry = self.repositories.entry(repo.git_dir.clone()).or_default();
        entry.path = repo.path.to_string_lossy().into_owned();
        if !entry.set.iter().any(|set| set.eq_ignore_ascii_case(key)) {
            entry.set.push(key.to_owned());
        }
    }

    fn disown(&mut self, repo: &Repo, key: &str) {
        if let Some(entry) = self.repositories.get_mut(&repo.git_dir) {
            entry.set.retain(|set| !set.eq_ignore_ascii_case(key));
            if entry.set.is_empty() {
                self.repositories.remove(&repo.git_dir);
            }
        }
    }
}

/// What a clone declares that aterm applies.
#[derive(Debug, Default, PartialEq, Eq)]
struct Declared {
    superproject: Vec<&'static str>,
    submodule: Vec<&'static str>,
    /// `Some(4)` only: a declared format aterm converts to.
    index: Option<u32>,
    /// What it declares that aterm leaves unapplied.
    notes: Vec<String>,
}

/// One repository of a clone, as planned.
#[derive(Debug)]
struct Repo {
    path: PathBuf,
    label: String,
    /// Its git directory's identity ([`Record::repositories`]' key), which a clone moved keeps
    /// and one made again at the same path does not share.
    git_dir: String,
    /// Where its own and its common git directories are (`config.lock`, `index.lock`).
    own_dir: PathBuf,
    common_dir: PathBuf,
    set: Vec<&'static str>,
    unset: Vec<&'static str>,
}

/// One clone, as planned.
#[derive(Debug, Default)]
struct Plan {
    label: String,
    /// Its declaration names something aterm applies.
    declares: bool,
    /// Another git holds a lock here: left for the next pass.
    held: Option<String>,
    /// A volume whose self-test this plan waits on.
    pending: Option<u64>,
    /// The superproject first.
    repos: Vec<Repo>,
    /// The superproject's index format, where it moves to [`INDEX_VERSION`].
    index: Option<u32>,
    notes: Vec<String>,
}

impl Plan {
    fn differs(&self) -> bool {
        self.index.is_some()
            || self
                .repos
                .iter()
                .any(|repo| !repo.set.is_empty() || !repo.unset.is_empty())
    }

    /// How the clone differs, for the row.
    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(top) = self.repos.first() {
            if !top.set.is_empty() {
                parts.push(format!("{} unset", and_list(&top.set)));
            }
            if !top.unset.is_empty() {
                parts.push(format!(
                    "{} dropped from its declaration but still set",
                    and_list(&top.unset)
                ));
            }
        }
        if let Some(from) = self.index {
            parts.push(format!("index format {from}"));
        }
        let subs = self.repos.get(1..).unwrap_or_default();
        for (what, unset) in [("unset", false), ("dropped but still set", true)] {
            let mut keys: Vec<&'static str> = Vec::new();
            let mut count = 0;
            for repo in subs {
                let these = if unset { &repo.unset } else { &repo.set };
                if !these.is_empty() {
                    count += 1;
                    for key in these {
                        if !keys.contains(key) {
                            keys.push(key);
                        }
                    }
                }
            }
            if count > 0 {
                parts.push(format!(
                    "{} {what} in {count} submodule{}",
                    and_list(&keys),
                    if count == 1 { "" } else { "s" }
                ));
            }
        }
        let mut line = format!("{}: {}", self.label, parts.join(", "));
        if self.pending.is_some() {
            line.push_str(" (once git's untracked-cache self-test passes on its volume)");
        }
        line
    }
}

fn and_list(keys: &[&str]) -> String {
    match keys {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn summary(clones: &[Plan], machine: &str) -> Reading {
    let differing: Vec<String> = clones
        .iter()
        .filter(|clone| clone.held.is_none() && clone.differs())
        .map(Plan::describe)
        .collect();
    if !differing.is_empty() {
        return Reading::Due {
            from: differing.join("; "),
            to: TO.to_owned(),
        };
    }
    let held: Vec<&str> = clones
        .iter()
        .filter_map(|clone| clone.held.as_deref())
        .collect();
    if !held.is_empty() {
        return Reading::Deferred(format!("{}; the next pass tunes it", held.join("; ")));
    }
    let declaring = clones.iter().filter(|clone| clone.declares).count();
    let mut now = format!(
        "{} vault clone{} on this {machine}, each as it declares",
        clones.len(),
        if clones.len() == 1 { "" } else { "s" }
    );
    if declaring < clones.len() {
        now.push_str(&format!(" ({declaring} declare tuning)"));
    }
    let notes: Vec<&str> = clones
        .iter()
        .flat_map(|clone| clone.notes.iter().map(String::as_str))
        .collect();
    if !notes.is_empty() {
        now.push_str("; ");
        now.push_str(&notes.join("; "));
    }
    Reading::Current(now)
}

fn plan_all(
    sys: &dyn System,
    git: Git<'_>,
    tops: &[PathBuf],
    record: &Record,
    test: Option<u64>,
) -> Vec<Plan> {
    tops.iter()
        .map(|top| plan(sys, git, top, record, test))
        .collect()
}

/// `path` as the person names it: under their home, `~/…`.
fn label(user: &User, path: &Path) -> String {
    match path.strip_prefix(&user.home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Plan one clone: its declaration, then each of its repositories. A clone with no declaration
/// still has each key aterm set and its declaration no longer holds unset. A clone that differs
/// where another git holds a lock it would take is left for the next pass, as a whole.
fn plan(sys: &dyn System, git: Git<'_>, top: &Path, record: &Record, test: Option<u64>) -> Plan {
    let mut plan = Plan {
        label: label(git.user, top),
        ..Plan::default()
    };
    let declared = match declaration(sys, git, top) {
        Ok(declared) => declared,
        Err(why) => {
            plan.notes
                .push(format!("{}: {why}, so nothing there changed", plan.label));
            return plan;
        }
    };
    plan.declares = !declared.superproject.is_empty()
        || !declared.submodule.is_empty()
        || declared.index.is_some();
    plan.notes.extend(
        declared
            .notes
            .iter()
            .map(|note| format!("{} {note}", plan.label)),
    );
    let mut paths = vec![top.to_path_buf()];
    submodules(sys, git, top, 0, &mut paths, &mut plan.notes);
    let mut withheld = None;
    for (at, path) in paths.iter().enumerate() {
        let superproject = at == 0;
        let keys = if superproject {
            &declared.superproject
        } else {
            &declared.submodule
        };
        match plan_repo(sys, git, path, superproject, keys, record, test) {
            Ok(planned) => {
                plan.pending = plan.pending.or(planned.pending);
                withheld = withheld.or(planned.withheld);
                plan.notes.extend(planned.notes);
                if superproject {
                    match index_plan(sys, git, top, &declared, &planned.repo) {
                        Ok(index) => plan.index = index,
                        Err(why) => plan.notes.push(format!("{}: {why}", planned.repo.label)),
                    }
                }
                plan.repos.push(planned.repo);
            }
            Err(why) => {
                plan.notes.push(format!(
                    "{}: {why}; it is left as it is",
                    label(git.user, path)
                ));
                if superproject {
                    return plan;
                }
            }
        }
    }
    if let Some(why) = withheld {
        plan.notes
            .push(format!("{}: no untracked cache: {why}", plan.label));
    }
    if plan.differs() {
        plan.held = held(sys, &plan);
    }
    if plan.differs()
        && plan.held.is_none()
        && let Some(why) = unnamed(sys, git, &plan)
    {
        plan.notes
            .push(format!("{why}; the clone is left as it is"));
        plan.index = None;
        plan.pending = None;
        for repo in &mut plan.repos {
            repo.set.clear();
            repo.unset.clear();
        }
    }
    plan
}

/// Why a write in one of `plan`'s repositories could run something aterm cannot turn off by
/// name ([`checkout::Programs::named`]).
fn unnamed(sys: &dyn System, git: Git<'_>, plan: &Plan) -> Option<String> {
    plan.repos
        .iter()
        .enumerate()
        .filter(|(at, repo)| {
            !repo.set.is_empty() || !repo.unset.is_empty() || (*at == 0 && plan.index.is_some())
        })
        .find_map(|(_, repo)| {
            checkout::Programs::named(sys, git.local(sys, Some(&repo.path)))
                .err()
                .map(|why| format!("{}: {why}", repo.label))
        })
}

/// The first lock that is there that `plan`'s writes would take: never waited on, never removed.
fn held(sys: &dyn System, plan: &Plan) -> Option<String> {
    for (at, repo) in plan.repos.iter().enumerate() {
        let writes = !repo.set.is_empty() || !repo.unset.is_empty();
        if writes && sys.stat(&repo.common_dir.join("config.lock")).is_ok() {
            return Some(lock_held(&repo.label, "config.lock"));
        }
        if at == 0 && plan.index.is_some() && sys.stat(&repo.own_dir.join("index.lock")).is_ok() {
            return Some(lock_held(&repo.label, "index.lock"));
        }
    }
    None
}

/// The clone's declaration on origin's default branch as last fetched: absent is an empty one.
fn declaration(sys: &dyn System, git: Git<'_>, top: &Path) -> Result<Declared, String> {
    let (branch, tip) =
        checkout::fetched_head_by(sys, &|| git.local(sys, Some(top))).map_err(|why| match why {
            NotFetched::Unread(unread) => unread.why,
            NotFetched::Unrecorded { .. } => {
                "records no default branch of origin's as last fetched (refs/remotes/origin/HEAD), \
                 so its declaration cannot be read"
                    .to_owned()
            }
        })?;
    let listed = git_text(sys, git, top, &["ls-tree", "-z", &tip, "--", DECLARATION])?;
    let listed = listed.trim_end_matches('\0');
    if listed.is_empty() {
        return Ok(Declared::default());
    }
    let what = format!("its {DECLARATION} on {branch}");
    let (meta, _) = listed
        .split_once('\t')
        .ok_or_else(|| format!("cannot read {what}"))?;
    let fields: Vec<&str> = meta.split_whitespace().collect();
    let [mode, kind, blob] = fields.as_slice() else {
        return Err(format!("cannot read {what}"));
    };
    if !matches!(*mode, "100644" | "100755") || *kind != "blob" {
        return Err(format!("{what} is not a regular file"));
    }
    // A partial clone lists a blob it never fetched. A git kept from fetching by the variable
    // alone (`NoLazyFetch::Variable`; git 2.43.0, measured) stops (128, "could not fetch ... from
    // promisor remote") at any read of such a blob, its size included, so this question, whose
    // every failure reads as absent, comes before any read of its size or contents.
    if !run(
        sys,
        &git.local(sys, Some(top)).args(["cat-file", "-e", blob]),
    )?
    .success()
    {
        return Err(format!(
            "{what} has not been fetched to this {} (a partial clone's)",
            sys.machine()
        ));
    }
    let size: u64 = git_text(sys, git, top, &["cat-file", "-s", blob])?
        .trim()
        .parse()
        .map_err(|_| format!("cannot read the size of {what}"))?;
    if size > DECLARATION_MAX {
        return Err(format!(
            "{what} is {size} bytes, over the {DECLARATION_MAX} a declaration may be"
        ));
    }
    let ran = run(
        sys,
        &git.local(sys, Some(top)).args([
            "config",
            "--blob",
            blob,
            "--no-includes",
            "--null",
            "--list",
        ]),
    )?;
    if !ran.success() {
        return Err(format!("{what} is not git config syntax"));
    }
    Ok(parse(&ran.stdout))
}

/// A declaration's `git config --null --list`: `name\nvalue` records, each ended by NUL.
fn parse(listed: &[u8]) -> Declared {
    let mut declared = Declared::default();
    let mut never: Vec<String> = Vec::new();
    let mut declared_index = None;
    for record in listed
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record = String::from_utf8_lossy(record);
        let (name, value) = match record.split_once('\n') {
            Some((name, value)) => (name, Some(value)),
            None => (record.as_ref(), None),
        };
        match (name, value) {
            ("tuning.superproject", Some(key)) => {
                take(
                    key,
                    &SUPERPROJECT_KEYS,
                    &mut declared.superproject,
                    name,
                    &mut declared.notes,
                );
            }
            ("tuning.submodule", Some(key)) => {
                take(
                    key,
                    &SUBMODULE_KEYS,
                    &mut declared.submodule,
                    name,
                    &mut declared.notes,
                );
            }
            ("tuning.indexversion", Some(value)) => declared_index = Some(value.trim().to_owned()),
            ("tuning.never", Some(key)) => never.push(key.trim().to_ascii_lowercase()),
            _ => declared.notes.push(format!(
                "declares {name}, no tuning entry aterm reads: ignored"
            )),
        }
    }
    // git's own rule for a repeated key: the last one wins.
    match declared_index.as_deref() {
        None => {}
        Some("4") => declared.index = Some(INDEX_VERSION),
        Some(other) => declared.notes.push(format!(
            "declares index format {other}; aterm converts an index only to {INDEX_VERSION}"
        )),
    }
    for keys in [&mut declared.superproject, &mut declared.submodule] {
        keys.retain(|key| {
            let refused = never.contains(&key.to_ascii_lowercase());
            if refused {
                declared
                    .notes
                    .push(format!("declares {key} both to set and never set: not set"));
            }
            !refused
        });
    }
    declared.notes.dedup();
    declared
}

/// Keep `key` when aterm's `allowed` names it (any case); name it otherwise.
fn take(
    key: &str,
    allowed: &[&'static str],
    into: &mut Vec<&'static str>,
    name: &str,
    notes: &mut Vec<String>,
) {
    match allowed
        .iter()
        .find(|allowed| allowed.eq_ignore_ascii_case(key.trim()))
    {
        Some(found) if !into.contains(found) => into.push(found),
        Some(_) => {}
        None => notes.push(format!(
            "declares {name}={key}, outside aterm's allowlist: not applied"
        )),
    }
}

/// Each populated submodule under `repo`, nested ones too, in `.gitmodules` order: a listed path
/// that is a real folder, reached through no link, holding a repository whose top it is. Read
/// from the worktree's `.gitmodules` as data (`git config --file`, after a size check); no index
/// is read and no shell runs.
fn submodules(
    sys: &dyn System,
    git: Git<'_>,
    repo: &Path,
    depth: usize,
    into: &mut Vec<PathBuf>,
    notes: &mut Vec<String>,
) {
    let listing = repo.join(".gitmodules");
    match sys.stat(&listing) {
        Ok(stat) if !stat.dir && !stat.symlink && stat.len <= GITMODULES_MAX => {}
        Ok(stat) if !stat.dir && !stat.symlink => {
            notes.push(format!(
                "{}: its .gitmodules is {} bytes, over the {GITMODULES_MAX} aterm reads, so its \
                 submodules are left as they are",
                label(git.user, repo),
                stat.len
            ));
            return;
        }
        _ => return,
    }
    if depth >= SUBMODULE_DEPTH {
        notes.push(format!(
            "{}: submodules nested past {SUBMODULE_DEPTH} levels are left as they are",
            label(git.user, repo)
        ));
        return;
    }
    let Ok(ran) = run(
        sys,
        &git.local(sys, Some(repo))
            .arg("config")
            .arg("--file")
            .arg(&listing)
            .args([
                "--no-includes",
                "--null",
                "--get-regexp",
                r"^submodule\..*\.path$",
            ]),
    ) else {
        notes.push(format!(
            "{}: cannot read its .gitmodules",
            label(git.user, repo)
        ));
        return;
    };
    match ran.status {
        Some(0) => {}
        Some(1) => return,
        _ => {
            notes.push(format!(
                "{}: cannot read its .gitmodules",
                label(git.user, repo)
            ));
            return;
        }
    }
    for record in ran
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record = String::from_utf8_lossy(record);
        let Some((_, path)) = record.split_once('\n') else {
            continue;
        };
        let relative = Path::new(path);
        if path.is_empty()
            || !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            notes.push(format!(
                "{}: its .gitmodules names {path:?}, not a path inside it: left as it is",
                label(git.user, repo)
            ));
            continue;
        }
        let sub = repo.join(relative);
        if !populated(sys, git, repo, relative) {
            continue;
        }
        if into.len() >= REPOSITORIES_MAX {
            notes.push(format!(
                "{}: repositories past {REPOSITORIES_MAX} are left as they are",
                label(git.user, repo)
            ));
            return;
        }
        into.push(sub.clone());
        submodules(sys, git, &sub, depth + 1, into, notes);
    }
}

/// Whether `relative` under `repo` is a real folder, reached through no link on the way, holding
/// a repository whose top it is.
fn populated(sys: &dyn System, git: Git<'_>, repo: &Path, relative: &Path) -> bool {
    let sub = repo.join(relative);
    match sys.stat(&sub) {
        Ok(stat) if stat.dir && !stat.symlink => {}
        _ => return false,
    }
    if sys.stat(&sub.join(".git")).is_err() {
        return false;
    }
    let Ok(top) = git_text(sys, git, &sub, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    match (
        sys.canonicalize(Path::new(top.trim())),
        sys.canonicalize(&sub),
        sys.canonicalize(repo),
    ) {
        (Ok(top), Ok(sub), Ok(repo)) => top == sub && sub == repo.join(relative),
        _ => false,
    }
}

/// [`Record::repositories`]' key for a git directory: its inode and birth time where the
/// filesystem keeps one (`<ino>@<born_ns>`), else its volume and inode (`<dev>:<ino>`). A
/// volume's `st_dev` is numbered as volumes mount, so it can change across a restart, and a
/// key that held it would lose every claim and opt-out with it; the birth time tells apart a
/// clone made again at the same path where a filesystem reuses inode numbers (ext4).
fn git_dir_key(id: FileId) -> String {
    match id.born_ns {
        Some(born) => format!("{}@{born}", id.ino),
        None => format!("{}:{}", id.dev, id.ino),
    }
}

/// [`plan_repo`]'s answer.
struct Planned {
    repo: Repo,
    /// The volume whose self-test its keys wait on.
    pending: Option<u64>,
    /// Why its volume gets no untracked cache.
    withheld: Option<&'static str>,
    notes: Vec<String>,
}

/// Where a repository's volume stands with the untracked cache.
enum Volume {
    Passed,
    Pending(u64),
    Withheld(&'static str),
}

/// What `path` gets: each declared key unset in every scope, where its volume allows the cache,
/// and each key aterm set that the declaration dropped while its own config still holds aterm's
/// value.
fn plan_repo(
    sys: &dyn System,
    git: Git<'_>,
    path: &Path,
    superproject: bool,
    declared: &[&'static str],
    record: &Record,
    test: Option<u64>,
) -> Result<Planned, String> {
    let here = label(git.user, path);
    let dirs = git_text(
        sys,
        git,
        path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    )?;
    let mut lines = dirs.lines();
    let (Some(own_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Err("cannot read where its git directory is".into());
    };
    let (own_dir, common_dir) = (PathBuf::from(own_dir), PathBuf::from(common_dir));
    // Git writes through a linked config, into a file outside the repository that others may
    // share.
    if sys
        .stat(&common_dir.join("config"))
        .is_ok_and(|stat| stat.symlink)
    {
        return Err("its config is a link, which aterm does not write through".into());
    }
    let id = sys
        .file_id(&common_dir)
        .map_err(|why| format!("cannot identify its git directory: {why}"))?;
    let git_dir = git_dir_key(id);
    let owned = record.owned(&git_dir);
    let allowed: &[&'static str] = if superproject {
        &SUPERPROJECT_KEYS
    } else {
        &SUBMODULE_KEYS
    };
    let volume = match sys.file_id(path) {
        Err(_) => Volume::Withheld("its volume cannot be read"),
        Ok(at) if Some(at.dev) != test => Volume::Withheld(
            "it is not on the volume of aterm's own state, where git's untracked-cache self-test \
             runs",
        ),
        Ok(at) => match record.volumes.get(&at.dev.to_string()) {
            Some(tested) if tested.passed => Volume::Passed,
            Some(_) => Volume::Withheld("git's untracked-cache self-test failed on its volume"),
            None => Volume::Pending(at.dev),
        },
    };
    let mut planned = Planned {
        repo: Repo {
            path: path.to_path_buf(),
            label: here.clone(),
            git_dir,
            own_dir,
            common_dir,
            set: Vec::new(),
            unset: Vec::new(),
        },
        pending: None,
        withheld: None,
        notes: Vec::new(),
    };
    for key in allowed {
        let wanted = declared.contains(key);
        let ours = owned.iter().any(|set| set.eq_ignore_ascii_case(key));
        if !wanted && !ours {
            continue;
        }
        let now = scoped(sys, git, path, key)?;
        // aterm's value is the `true` it wrote in the repository's own file: one an included
        // file or another scope gives is somebody else's.
        let aterms = ours && own_value(sys, git, path, key)?.as_deref() == Some("true");
        match (wanted, ours) {
            (true, false) if now.is_none() => match volume {
                Volume::Passed => planned.repo.set.push(key),
                Volume::Pending(dev) => {
                    planned.repo.set.push(key);
                    planned.pending = Some(dev);
                }
                Volume::Withheld(why) => planned.withheld = Some(why),
            },
            // Somebody's choice, in some scope: kept.
            (true, false) => {}
            (true, true) if !aterms => {
                let mut note = format!(
                    "{here}: {key} no longer holds the true aterm set in its own config; aterm \
                     does not set it again"
                );
                if superproject && *key == UNTRACKED_CACHE && now.is_none() {
                    note.push_str(&format!(
                        " (while {MANY_FILES} is on, the untracked cache stays on unless \
                         {UNTRACKED_CACHE} is false)"
                    ));
                }
                planned.notes.push(note);
            }
            (false, true) if aterms => planned.repo.unset.push(key),
            _ => {}
        }
    }
    Ok(planned)
}

/// The format the superproject's index moves from, where it moves: the declaration names 4,
/// `feature.manyFiles` is in effect once the plan's keys are written, no explicit
/// `index.version` names another, and the index is at another format.
fn index_plan(
    sys: &dyn System,
    git: Git<'_>,
    top: &Path,
    declared: &Declared,
    repo: &Repo,
) -> Result<Option<u32>, String> {
    if declared.index != Some(INDEX_VERSION) || repo.unset.contains(&MANY_FILES) {
        return Ok(None);
    }
    if !repo.set.contains(&MANY_FILES) && !many_files(sys, git, top)? {
        return Ok(None);
    }
    index_due(sys, git, top)
}

/// Whether `feature.manyFiles` is in effect in `top`.
fn many_files(sys: &dyn System, git: Git<'_>, top: &Path) -> Result<bool, String> {
    let ran = run(
        sys,
        &git.local(sys, Some(top))
            .args(["config", "--type=bool", "--get", MANY_FILES]),
    )?;
    match ran.status {
        Some(0) => Ok(stdout(&ran) == "true"),
        Some(1) => Ok(false),
        _ => Err(format!("cannot read its {MANY_FILES}")),
    }
}

/// The index's format where it is not [`INDEX_VERSION`] and no explicit `index.version` names
/// another: read from the index file's header, which takes no lock and writes nothing.
fn index_due(sys: &dyn System, git: Git<'_>, top: &Path) -> Result<Option<u32>, String> {
    let explicit = run(
        sys,
        &git.local(sys, Some(top))
            .args(["config", "--get", "index.version"]),
    )?;
    match explicit.status {
        Some(0) if stdout(&explicit) != INDEX_VERSION.to_string() => return Ok(None),
        Some(0 | 1) => {}
        _ => return Err("cannot read its index.version".into()),
    }
    let index = git_text(
        sys,
        git,
        top,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    let head = match sys.read_head(Path::new(index.trim()), 8) {
        Ok(head) => head,
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(why) => return Err(format!("cannot read its index: {why}")),
    };
    match head.as_slice() {
        [b'D', b'I', b'R', b'C', a, b, c, d] => {
            let version = u32::from_be_bytes([*a, *b, *c, *d]);
            Ok((version != INDEX_VERSION).then_some(version))
        }
        _ => Err("its index has no index header".into()),
    }
}

/// Move the superproject's index to [`INDEX_VERSION`] where it is still due after the writes,
/// running nothing else ([`quiet`]); `Some(from)` once moved.
fn convert_index(sys: &dyn System, git: Git<'_>, top: &Repo) -> Result<Option<u32>, Wrote> {
    if top.unset.contains(&MANY_FILES) || !many_files(sys, git, &top.path).map_err(Wrote::Failed)? {
        return Ok(None);
    }
    let Some(from) = index_due(sys, git, &top.path).map_err(Wrote::Failed)? else {
        return Ok(None);
    };
    let quiet = quiet(sys, git, &top.path).map_err(Wrote::Failed)?;
    write(sys, &quiet, &["update-index", "--index-version", "4"])?;
    Ok(Some(from))
}

/// The scope and value `key` has in `path`, read with no `-c`: `None` where it is unset.
fn scoped(
    sys: &dyn System,
    git: Git<'_>,
    path: &Path,
    key: &str,
) -> Result<Option<(String, String)>, String> {
    let ran = run(
        sys,
        &git.local(sys, Some(path))
            .args(["config", "--show-scope", "--get", key]),
    )?;
    match ran.status {
        Some(0) => {
            let out = stdout(&ran);
            let (scope, value) = out
                .split_once('\t')
                .ok_or_else(|| format!("cannot read which config sets its {key}"))?;
            Ok(Some((scope.to_owned(), value.to_owned())))
        }
        Some(1) => Ok(None),
        _ => Err(format!(
            "cannot read which config sets its {key} (git config --show-scope needs git 2.26)"
        )),
    }
}

/// The value `key` has in `path`'s own config file, no included file read: `None` where unset.
fn own_value(
    sys: &dyn System,
    git: Git<'_>,
    path: &Path,
    key: &str,
) -> Result<Option<String>, String> {
    let ran = run(
        sys,
        &git.local(sys, Some(path))
            .args(["config", "--local", "--no-includes", "--get", key]),
    )?;
    match ran.status {
        Some(0) => Ok(Some(stdout(&ran))),
        Some(1) => Ok(None),
        _ => Err(format!("cannot read its own config's {key}")),
    }
}

/// Run git's untracked-cache self-test in a scratch repository under `dir`, on its volume.
fn self_test(sys: &dyn System, git: Git<'_>, dir: &Path) -> Result<bool, String> {
    let mut scratch = Scratch::new_in(sys, dir, "untracked-cache-test").map_err(|why| {
        format!(
            "cannot make a scratch repository in {}: {why}",
            dir.display()
        )
    })?;
    let at = scratch.path().to_path_buf();
    let init = run(
        sys,
        &git.local(sys, None)
            .args(NO_SIDE_RUNS)
            .args(["init", "--quiet", "--template="])
            .arg(&at),
    )?;
    if !init.success() {
        return Err(format!(
            "cannot make a scratch repository in {}: {}",
            at.display(),
            init.describe()
        ));
    }
    let tested = run(
        sys,
        &quiet(sys, git, &at)?.args(["update-index", "--test-untracked-cache"]),
    )?;
    let _ = scratch.close();
    verdict(&tested)
}

/// Git's own verdict in its self-test's exit: 0 where the volume keeps directory times as the
/// untracked cache needs, 1 where it does not.
///
/// # Errors
/// Any other end (a `die`, 128, as on a full disk; a signal) is no verdict: nothing is kept,
/// and the next pass tests again.
fn verdict(tested: &Ran) -> Result<bool, String> {
    match tested.status {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git's untracked-cache self-test did not finish: {}; the next pass tests again",
            tested.describe()
        )),
    }
}

/// [`Git::local`] for a write in `path` that runs nothing else: no hook from a hooks folder and no
/// fsmonitor daemon ([`NO_SIDE_RUNS`]), no hook or filter driver its config names
/// ([`checkout::Programs::off`]), and no wait on a `config.lock` another
/// git took after [`held`] looked (`core.configLockTimeout=0`); with git's messages in English
/// (`LC_ALL=C`), which [`write`] reads.
fn quiet(sys: &dyn System, git: Git<'_>, path: &Path) -> Result<Cmd, String> {
    let programs = checkout::Programs::named(sys, git.local(sys, Some(path)))?;
    Ok(programs.off(
        git.local(sys, Some(path))
            .args(NO_SIDE_RUNS)
            .args(["-c", "core.configLockTimeout=0"])
            .env("LC_ALL", "C"),
    ))
}

/// Why a write did not land.
#[derive(Debug)]
enum Wrote {
    /// Another git holds a lock the write takes (git's "File exists").
    Locked,
    Failed(String),
}

fn write(sys: &dyn System, quiet: &Cmd, args: &[&str]) -> Result<(), Wrote> {
    let ran = run(sys, &quiet.clone().args(args.iter().copied())).map_err(Wrote::Failed)?;
    if ran.success() {
        Ok(())
    } else if String::from_utf8_lossy(&ran.stderr).contains("File exists") {
        Err(Wrote::Locked)
    } else {
        Err(Wrote::Failed(ran.describe()))
    }
}

fn run(sys: &dyn System, cmd: &Cmd) -> Result<Ran, String> {
    sys.run_complete_stdout(cmd)
        .map_err(|why| format!("cannot run git: {why}"))
}

fn stdout(ran: &Ran) -> String {
    String::from_utf8_lossy(&ran.stdout).trim().to_owned()
}

/// One git read that must succeed, its standard output.
fn git_text(sys: &dyn System, git: Git<'_>, path: &Path, args: &[&str]) -> Result<String, String> {
    let ran = run(sys, &git.local(sys, Some(path)).args(args.iter().copied()))?;
    if !ran.success() {
        return Err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or("read"),
            ran.describe()
        ));
    }
    Ok(String::from_utf8_lossy(&ran.stdout)
        .trim_end_matches('\n')
        .to_owned())
}

#[cfg(test)]
mod tests;
