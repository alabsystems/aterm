// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DAILY UPKEEP'S BRANCH-NAME STEP (DC-02c, `docs/DESIGN-disk-custody-2026-11-12.md` §3.2,
//! the fourth row of its worktree table): stale branch names are pruned by software, and no
//! prune drops the last name of a commit.
//!
//! For each main checkout among the home's children ([`super::discover::main_checkouts`]):
//!
//! - A remote-tracking name `refs/remotes/origin/<b>` whose branch the server no longer has
//!   is deleted. Which branches the server has is read from the server itself
//!   ([`pins::origin_heads`]: `git ls-remote` for a local origin, `aterm agh repo refs` for a
//!   GitHub one), never from what a local `fetch --prune` did, so the step works on a checkout
//!   nobody has fetched; a listing that lacks `refs/heads/main` is no listing, and no name goes
//!   by it. Where `origin/main` lacks some commit of the name and no lasting name holds it (a
//!   local branch, a tag or a pin under `refs/orphans/` other than the name's own, [`only_name`];
//!   never a remote-tracking name, a stash, a prefetch or a session's scratch ref, which go
//!   without anyone deciding to drop a commit), the name's tip is first pinned as
//!   `refs/orphans/<b>/<12 hex digits>` in the shared repository (DC-02b's shape, so two stale
//!   names of one branch at two commits are two pins, and a pin never collides with a branch
//!   path). Its push is then recorded in DC-02b's owed file ([`pins::owe_entries`]) before it is
//!   sent under that same name to the origin and read back there against the local commit
//!   ([`pins::push`]), and its outcome settled there ([`pins::settle`]): one that went leaves
//!   the file, one that failed stays owed with one `worktree-push` `owed` row, retried by the
//!   worktree step's rounds, and one an earlier run left owed is left to those rounds. A pin
//!   whose push cannot be recorded keeps its name; otherwise the prune happens, since the pin
//!   holds the commits and the file holds what the origin is owed. A name whose own pin stands
//!   from an earlier run that stopped part way is judged as its only name again, so that pin's
//!   push is recorded before the name goes.
//! - A local branch `refs/heads/<b>` that no worktree holds (`git worktree list`), no live
//!   session used ([`Facts::used`], `repo_work::Used`'s branches) and nothing moved for
//!   [`QUIET`] (its reflog's newest entry, or its tip's committer time where it has none: a
//!   branch any program, of any harness or none, just made or moved is someone's) is deleted
//!   once `origin/main` contains its commits ([`super::worktree::delete_merged_branch`]:
//!   compare-and-delete, never `main` or origin's default branch, never one being rebased or
//!   bisected). An unmerged one stays, and is pushed under its own name where the server has no
//!   branch of that name, its push recorded first as a pin's is; where the server has that
//!   name, it is left alone.
//!
//! - A worktree registration whose folder is gone (`.git/worktrees/<id>`, its folder removed
//!   by hand) is dropped, one at a time, once what it alone holds is pinned
//!   ([`atpkg::build_cache::gone_entries::drop_gone`], DC-13b): its HEAD, its index, its own
//!   refs and reflogs, under `refs/orphans/<folder name>/<12 hex digits>` in the repository, and
//!   its private submodule repositories' commits under the same name in the checkout's module
//!   store ([`Hooks::drop_gone`]). Each pin's push is recorded and tried as a stale name's is ([`Checkout::send`])
//!   before the entry goes; one that cannot be recorded keeps it. A locked registration, one
//!   whose folder stands and one whose folder is away (a volume not mounted now) are left alone
//!   and nothing is said of them; one that cannot be read or pinned stays with one `kept` row.
//!   `git worktree prune` is never run.
//!
//! Every pin and every prune is a `branch-ref` journal row written before the ref changes; one
//! whose intent the ledger does not take does not happen. Tags, `refs/remotes/origin/main`,
//! origin's default branch and other remotes' names are never touched, nothing on the server is
//! deleted or moved (each push only creates a ref where none stands there), and no `fetch` or
//! `remote prune` runs. Limit: a name an agent's own `git fetch --prune` removed before this
//! step saw it is not pinned here; git's reflog holds it for its expiry window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use atpkg::build_cache::gone_entries::{Custody, Pass, Pin, Registration};
use atpkg::build_cache::retired::Git;

use super::worktree::pins::{self, Owed, Push, Tried};
use super::{RefRow, ref_row, worktree_push_row};

/// The whole step's wall-clock bound: no name is judged after it, and the next day's step
/// starts over.
pub const STEP_WALL: Duration = Duration::from_secs(120);

/// How long past [`STEP_WALL`] the git runs and pushes under way at it may go before they are
/// killed.
pub const GIT_GRACE: Duration = Duration::from_secs(60);

/// How long a local branch must have stood unmoved before it is deleted or pushed: one day,
/// the step's own period.
pub const QUIET: u64 = 24 * 60 * 60;

/// The most bytes of one open session's transcript [`used_by`] reads for the branches it used.
const TAIL_BYTES: u64 = 8 << 20;

/// What the step reads beyond the repositories themselves.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The main checkouts whose names are judged.
    pub checkouts: &'a [PathBuf],
    /// The local branches (short names, `agent/x`) a live session used, `None` where that is
    /// not known: then no local branch is deleted or pushed.
    pub used: Option<&'a [String]>,
    /// The file of pushes owed to an origin ([`pins::OWED_FILE`] in aterm's state folder).
    pub owed: &'a Path,
}

/// The pass over one checkout's worktree registrations whose folder is gone
/// ([`atpkg::build_cache::gone_entries::drop_gone`], with git, the home and its placing bound by
/// the caller, and the live process fence; a headless fixture's probe in a test).
pub type DropGone<'a> = &'a dyn Fn(&Path, &mut dyn Custody) -> Result<Pass, String>;

/// What the step calls to act.
pub struct Hooks<'a> {
    /// Whether the claim the step runs under is still this host's.
    pub still_held: &'a dyn Fn() -> bool,
    /// Append one row to the ledger; `false` when it is not on record.
    pub journal: &'a mut dyn FnMut(&str) -> bool,
    /// `aterm agh <args>`, for an origin on GitHub ([`pins::agh_until`]).
    pub agh: pins::Agh<'a>,
    /// The pass over a checkout's registrations whose folder is gone.
    pub drop_gone: DropGone<'a>,
}

/// One name the step deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    /// The checkout whose repository held it.
    pub checkout: PathBuf,
    /// The full name.
    pub name: String,
    /// The commit it named.
    pub oid: String,
    /// The pin that holds its commits, where it was their only name.
    pub pin: Option<String>,
}

/// What one step did.
#[derive(Debug, Default)]
pub struct Done {
    /// The names that went.
    pub pruned: Vec<Pruned>,
    /// The pins written, `(checkout, pin, commit)`.
    pub pinned: Vec<(PathBuf, String, String)>,
    /// The pushes that went and read back as the local commit.
    pub pushed: Vec<Owed>,
    /// The pushes that failed, each owed with why.
    pub owed: Vec<Owed>,
    /// The pushes the origin refuses for good (a ref of that name there names another commit).
    pub parked: Vec<Owed>,
    /// The owed pushes the owed file had no room for once this step's were added, each
    /// journalled `dropped`: their refs stand where they were made.
    pub dropped: Vec<Owed>,
    /// The names an intent was written for that stay, `(checkout, name, why)`.
    pub kept: Vec<(PathBuf, String, String)>,
    /// The worktree registrations whose folder was gone that went, each with its checkout and
    /// the pins that hold what it held.
    pub entries_dropped: Vec<(PathBuf, Registration, Vec<Pin>)>,
    /// The worktree registrations whose folder is gone, or that could not be read, that stay,
    /// each with its checkout and why.
    pub entries_kept: Vec<(PathBuf, Registration, String)>,
    /// The checkouts whose server or names could not be read, each with why: no name of a
    /// server it could not read went.
    pub unread: Vec<(PathBuf, String)>,
    /// Why the owed file could not be read or written, when it could not.
    pub unrecorded: Option<String>,
    /// Why the step stopped before every checkout was through.
    pub unfinished: Option<String>,
}

/// The branches the open sessions used ([`crate::harness::repo_work::Used`]'s branches, read off the
/// last [`TAIL_BYTES`] of each one's transcript), `None` where some open session's transcript
/// cannot be named or read: then no local branch is judged.
#[must_use]
pub fn used_by(open: &[super::worktree::OpenSession]) -> Option<Vec<String>> {
    let mut used = Vec::new();
    for session in open {
        let transcript = session.transcript.as_ref().ok()?;
        let tail = crate::harness::codex_rollout::tail_of(transcript, TAIL_BYTES)?;
        used.extend(crate::harness::repo_work::Used::of_session(None, Some(&tail)).branches);
    }
    used.sort();
    used.dedup();
    Some(used)
}

/// What git answered, through `git`.
fn git_out(git: &dyn Git, dir: &Path, args: &[&str]) -> Result<String, String> {
    let a = git.run(dir, args);
    if a.code == Some(0) {
        Ok(a.out)
    } else {
        Err(format!("git {} failed: {}", args.join(" "), a.err.trim()))
    }
}

/// The first 12 hex digits of `oid`.
fn short(oid: &str) -> &str {
    oid.get(..12).unwrap_or(oid)
}

/// Whether `origin/main` contains `oid`: `Err` where git could not say.
fn merged(git: &dyn Git, checkout: &Path, oid: &str) -> Result<bool, String> {
    let a = git.run(
        checkout,
        &[
            "merge-base",
            "--is-ancestor",
            oid,
            "refs/remotes/origin/main",
        ],
    );
    match a.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "git could not say whether origin/main holds {}: {}",
            short(oid),
            a.err.trim()
        )),
    }
}

/// Whether some commit `oid` reaches is held by no lasting name of `checkout`'s repository:
/// no local branch, no tag and no pin under `refs/orphans/` but `pin` (the stale name's own,
/// which an earlier run may have made before it stopped). A remote-tracking name, a stash, a
/// prefetch, a bisect or a session's scratch ref, and a worktree's detached HEAD, hold nothing
/// here: each goes or moves without anyone deciding to drop a commit. `origin/main` is a
/// remote-tracking name too, so a commit it alone holds counts as held by none; the caller asks
/// [`merged`] first.
fn only_name(git: &dyn Git, checkout: &Path, oid: &str, pin: &str) -> Result<bool, String> {
    let exclude = format!("--exclude={pin}");
    let lone = git_out(
        git,
        checkout,
        &[
            "rev-list",
            "-n",
            "1",
            oid,
            "--not",
            "--branches",
            "--tags",
            &exclude,
            "--glob=refs/orphans/*",
        ],
    )?;
    Ok(!lone.trim().is_empty())
}

/// When the local branch `name` last moved, in seconds since the epoch: its reflog's newest
/// entry, or `committed` (its tip's committer time) where it has none (its reflog expired, or
/// git keeps none here).
fn last_moved(git: &dyn Git, checkout: &Path, name: &str, committed: u64) -> Result<u64, String> {
    let log = git_out(
        git,
        checkout,
        &[
            "reflog",
            "show",
            "-n",
            "1",
            "--date=unix",
            "--format=%gd",
            name,
            "--",
        ],
    )?;
    let log = log.trim();
    if log.is_empty() {
        return Ok(committed);
    }
    log.rsplit_once("@{")
        .and_then(|(_, at)| at.strip_suffix('}'))
        .and_then(|at| at.parse().ok())
        .ok_or_else(|| format!("git read {name}'s reflog as {log}, no time"))
}

/// The pin of the stale remote-tracking branch `branch` at `oid`.
fn pin_name(branch: &str, oid: &str) -> String {
    format!("refs/orphans/{branch}/{}", short(oid))
}

/// One step over `facts.checkouts` at `now`, until `wall`.
#[must_use]
pub fn step(
    git: &dyn Git,
    facts: &Facts<'_>,
    now: SystemTime,
    wall: Instant,
    mut hooks: Hooks<'_>,
) -> Done {
    let at = i64::try_from(pins::epoch_secs(now)).unwrap_or(0);
    let mut done = Done::default();
    for checkout in facts.checkouts {
        if Instant::now() >= wall {
            done.unfinished = Some("the step's time ran out; the rest go at its next run".into());
            break;
        }
        if !(hooks.still_held)() {
            done.unfinished = Some("the claim the step runs under was lost".into());
            break;
        }
        let mut one = Checkout {
            git,
            facts,
            hooks: &mut hooks,
            checkout,
            at,
            now,
            wall,
            done: &mut done,
        };
        one.gone_entries();
        if let Err(why) = one.tidy() {
            done.unread.push((checkout.clone(), why));
        }
    }
    done
}

/// One checkout's names, under the step's hooks.
struct Checkout<'s, 'h, 'a> {
    git: &'s dyn Git,
    facts: &'s Facts<'a>,
    hooks: &'s mut Hooks<'h>,
    checkout: &'s Path,
    at: i64,
    now: SystemTime,
    wall: Instant,
    done: &'s mut Done,
}

impl Checkout<'_, '_, '_> {
    /// Append `row`; `false` when it is not on record.
    fn journal(&mut self, row: &str) -> bool {
        (self.hooks.journal)(row)
    }

    /// A `branch-ref` row of `name` at `oid`.
    fn row(&self, action: &str, name: &str, oid: &str, pin: Option<&str>, text: &str) -> String {
        ref_row(
            self.at,
            action,
            &RefRow {
                checkout: self.checkout,
                name,
                oid,
                pin,
                text,
            },
        )
    }

    /// Whether the step may go on to another name.
    fn within(&mut self) -> bool {
        if Instant::now() >= self.wall {
            self.done.unfinished =
                Some("the step's time ran out; the rest go at its next run".into());
            return false;
        }
        if !(self.hooks.still_held)() {
            self.done.unfinished = Some("the claim the step runs under was lost".into());
            return false;
        }
        true
    }

    /// Judge every name of the checkout. `Err`: nothing of it could be read.
    fn tidy(&mut self) -> Result<(), String> {
        let git = self.git;
        let checkout = self.checkout;
        // Every judgment is against origin/main: a checkout without one has nothing judged.
        git_out(
            git,
            checkout,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main^{commit}",
            ],
        )
        .map_err(|_| "it has no origin/main to judge its names against".to_owned())?;
        let listed = git_out(
            git,
            checkout,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(committerdate:unix)",
                "refs/remotes/origin/",
                "refs/heads/",
            ],
        )?;
        // `(name, commit, its committer time)`.
        let names: Vec<(String, String, u64)> = listed
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\0');
                let (name, oid, kind) = (f.next()?, f.next()?, f.next()?);
                let committed = f.next()?.trim().parse().ok()?;
                (kind == "commit").then(|| (name.to_owned(), oid.to_owned(), committed))
            })
            .collect();
        let default = git.run(
            checkout,
            &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"],
        );
        let default = match default.code {
            Some(0) => default
                .out
                .trim()
                .strip_prefix("refs/remotes/origin/")
                .map(str::to_owned),
            Some(_) => None,
            None => return Err(format!("git could not read origin's HEAD: {}", default.err)),
        };
        let kept_name = |b: &str| b == "main" || b == "HEAD" || default.as_deref() == Some(b);
        // The server's branches, read from the server: where they cannot be read, or the
        // listing lacks main, no remote-tracking name is judged stale and no branch is pushed.
        let server = pins::origin_heads(git, self.hooks.agh, checkout).and_then(|heads| {
            if heads.contains_key("refs/heads/main") {
                Ok(heads)
            } else {
                Err(
                    "its origin's listing holds no refs/heads/main, so it is read as no \
                     listing"
                        .to_owned(),
                )
            }
        });
        if let Err(why) = &server {
            self.done.unread.push((checkout.to_path_buf(), why.clone()));
        }
        if let Ok(server) = &server {
            for (name, oid, _) in &names {
                let Some(branch) = name.strip_prefix("refs/remotes/origin/") else {
                    continue;
                };
                if kept_name(branch) || server.contains_key(&format!("refs/heads/{branch}")) {
                    continue;
                }
                if !self.within() {
                    return Ok(());
                }
                self.stale(name, branch, oid);
            }
        }
        let Some(used) = self.facts.used else {
            return Ok(());
        };
        let held = git_out(git, checkout, &["worktree", "list", "--porcelain"])?;
        let held: BTreeSet<&str> = held
            .lines()
            .filter_map(|l| l.strip_prefix("branch "))
            .collect();
        let now = pins::epoch_secs(self.now);
        for (name, oid, committed) in &names {
            let Some(branch) = name.strip_prefix("refs/heads/") else {
                continue;
            };
            if kept_name(branch) || held.contains(name.as_str()) || used.iter().any(|u| u == branch)
            {
                continue;
            }
            if !self.within() {
                return Ok(());
            }
            let judged = last_moved(git, checkout, name, *committed).and_then(|moved| {
                if now.saturating_sub(moved) < QUIET {
                    return Ok(None);
                }
                merged(git, checkout, oid).map(Some)
            });
            match judged {
                // Moved within the day: someone's, whichever program moved it.
                Ok(None) => {}
                Ok(Some(true)) => self.delete_merged(name, oid),
                Ok(Some(false)) => {
                    if let Ok(server) = &server
                        && !server.contains_key(name)
                    {
                        self.push_branch(name, oid);
                    }
                }
                Err(why) => self
                    .done
                    .kept
                    .push((checkout.to_path_buf(), name.clone(), why)),
            }
        }
        Ok(())
    }

    /// The stale remote-tracking name `name` (of the server's former branch `branch`) at
    /// `oid`: where no lasting name holds its commits main lacks, pinned and its push recorded
    /// (and tried) first, then deleted.
    fn stale(&mut self, name: &str, branch: &str, oid: &str) {
        let (git, checkout) = (self.git, self.checkout);
        let pin = pin_name(branch, oid);
        let lone = match merged(git, checkout, oid) {
            Ok(true) => Ok(false),
            Ok(false) => only_name(git, checkout, oid, &pin),
            Err(why) => Err(why),
        };
        let lone = match lone {
            Ok(lone) => lone,
            Err(why) => {
                self.done
                    .kept
                    .push((checkout.to_path_buf(), name.into(), why));
                return;
            }
        };
        let pin = if lone {
            let text = "the server no longer has its branch, and no lasting name holds commits \
                        of it origin/main lacks: they are pinned first";
            if !self.journal(&self.row("pinning", name, oid, Some(&pin), text)) {
                self.done.kept.push((
                    checkout.to_path_buf(),
                    name.into(),
                    "the ledger took no row of its pin, so it is neither pinned nor pruned".into(),
                ));
                return;
            }
            if let Err(why) = pins::create_ref(git, checkout, &[], &pin, oid) {
                self.journal(&self.row("kept", name, oid, Some(&pin), &why));
                self.done
                    .kept
                    .push((checkout.to_path_buf(), name.into(), why));
                return;
            }
            self.done
                .pinned
                .push((checkout.to_path_buf(), pin.clone(), oid.to_owned()));
            if let Err(why) = self.send(checkout, &pin, oid) {
                let why = format!("its pin's push could not be recorded, so it stays: {why}");
                self.journal(&self.row("kept", name, oid, Some(&pin), &why));
                self.done
                    .kept
                    .push((checkout.to_path_buf(), name.into(), why));
                return;
            }
            Some(pin)
        } else {
            None
        };
        let text = match &pin {
            Some(pin) => format!(
                "the server no longer has its branch; its commits stand on {pin}@{}",
                short(oid)
            ),
            None => "the server no longer has its branch, and origin/main or a lasting name \
                     holds its commits"
                .to_owned(),
        };
        if !self.journal(&self.row("pruning", name, oid, pin.as_deref(), &text)) {
            self.done.kept.push((
                checkout.to_path_buf(),
                name.into(),
                "the ledger took no row of its prune, so it stays".into(),
            ));
            return;
        }
        let deleted = git_out(
            git,
            checkout,
            &["update-ref", "--no-deref", "-d", name, oid],
        );
        match deleted {
            Ok(_) => {
                self.journal(&self.row("pruned", name, oid, pin.as_deref(), &text));
                self.done.pruned.push(Pruned {
                    checkout: checkout.to_path_buf(),
                    name: name.into(),
                    oid: oid.into(),
                    pin,
                });
            }
            Err(why) => {
                self.journal(&self.row("kept", name, oid, pin.as_deref(), &why));
                self.done
                    .kept
                    .push((checkout.to_path_buf(), name.into(), why));
            }
        }
    }

    /// The local branch `name` at `oid`, which `origin/main` contains and no worktree holds:
    /// deleted, its intent on record first.
    fn delete_merged(&mut self, name: &str, oid: &str) {
        let checkout = self.checkout;
        let text = "no worktree holds it, no live session used it, and origin/main holds its \
                    commits";
        if !self.journal(&self.row("pruning", name, oid, None, text)) {
            self.done.kept.push((
                checkout.to_path_buf(),
                name.into(),
                "the ledger took no row of its prune, so it stays".into(),
            ));
            return;
        }
        match super::worktree::delete_merged_branch(self.git, checkout, name, oid) {
            Ok(()) => {
                self.journal(&self.row("pruned", name, oid, None, text));
                self.done.pruned.push(Pruned {
                    checkout: checkout.to_path_buf(),
                    name: name.into(),
                    oid: oid.into(),
                    pin: None,
                });
            }
            Err(why) => {
                self.journal(&self.row("kept", name, oid, None, &why));
                self.done
                    .kept
                    .push((checkout.to_path_buf(), name.into(), why));
            }
        }
    }

    /// The unmerged local branch `name` at `oid`, which the server has no branch of: pushed
    /// under its own name, its push recorded first ([`Self::send`]); one that cannot be
    /// recorded is not sent.
    fn push_branch(&mut self, name: &str, oid: &str) {
        if let Err(why) = self.send(self.checkout, name, oid) {
            self.done.kept.push((
                self.checkout.to_path_buf(),
                name.into(),
                format!("its push could not be recorded, so it is not sent: {why}"),
            ));
        }
    }

    /// Owe `name` at `oid` to the origin of `repo` (the checkout, or a module store in its git
    /// directory) in the owed file first, then, unless an earlier run owed it already (the
    /// worktree step's rounds then own it), try it
    /// ([`pins::push`]: only where no ref of that name stands there, read back after against
    /// `oid`) and settle its outcome in the file ([`pins::settle`]): what went or was parked
    /// leaves it, what failed waits there for those rounds. `Err`: the file could not be read
    /// or written before the try, so nothing was sent and nothing is owed.
    fn send(&mut self, repo: &Path, name: &str, oid: &str) -> Result<(), String> {
        let since = pins::epoch_secs(self.now);
        let mut entry = Owed {
            push: Push {
                repo: repo.to_path_buf(),
                refs: vec![[name.to_owned(), oid.to_owned()]],
            },
            worktree: self.checkout.to_path_buf(),
            since,
            why: String::new(),
            intent: None,
            tries: 0,
            // No worktree waits on it: the worktree step's rounds try it once it is due.
            removed: true,
            next: 0,
        };
        let (added, dropped) = match pins::owe_entries(self.facts.owed, [entry.clone()]) {
            Ok(owing) => owing,
            Err(why) => {
                self.unrecorded(&why);
                return Err(why);
            }
        };
        for gone in dropped {
            self.journal(&worktree_push_row(
                self.at,
                "dropped",
                Some(&gone),
                "the owed file had no room for it; its refs stand in its checkout",
            ));
            self.done.dropped.push(gone);
        }
        if added == 0 {
            return Ok(());
        }
        let tried = pins::push(self.git, self.hooks.agh, &mut entry);
        if let Err(why) = pins::settle(self.facts.owed, &entry, &tried, since) {
            // It stays owed as first recorded: a round tries it again, and finds it there.
            self.unrecorded(&why);
        }
        match tried {
            Tried::Pushed => {
                self.journal(&worktree_push_row(
                    self.at,
                    "pushed",
                    Some(&entry),
                    "read back at origin as the local commit",
                ));
                self.done.pushed.push(entry);
            }
            Tried::Parked(why) => {
                entry.why = why;
                self.journal(&worktree_push_row(
                    self.at,
                    "parked",
                    Some(&entry),
                    &entry.why,
                ));
                self.done.parked.push(entry);
            }
            Tried::Owed(why) => {
                entry.why = why;
                entry.tries = 1;
                entry.next = since.saturating_add(pins::backoff(1));
                self.journal(&worktree_push_row(
                    self.at,
                    "owed",
                    Some(&entry),
                    &entry.why,
                ));
                self.done.owed.push(entry);
            }
        }
        Ok(())
    }

    /// The checkout's worktree registrations whose folder is gone: each dropped once what it
    /// alone holds is pinned and those pins' pushes are recorded, one `branch-ref` row for each
    /// pin before it is written, one for the drop before it happens, and one for what became of
    /// it (`pruned`, or `kept` with why).
    fn gone_entries(&mut self) {
        let (checkout, drop_gone) = (self.checkout, self.hooks.drop_gone);
        let pass = drop_gone(checkout, self);
        let pass = match pass {
            Ok(pass) => pass,
            Err(why) => {
                self.done.unread.push((checkout.to_path_buf(), why));
                return;
            }
        };
        if let Some(why) = pass.unfinished {
            self.done.unfinished.get_or_insert(why);
        }
        for (gone, pins) in pass.dropped {
            let text = format!(
                "its folder {} was gone; git's entry for it went, what it alone held on {} pins",
                gone.folder.display(),
                pins.len()
            );
            self.journal(&self.entry_row("pruned", &gone, None, &text));
            self.done
                .entries_dropped
                .push((checkout.to_path_buf(), gone, pins));
        }
        for (gone, why) in pass.kept {
            self.journal(&self.entry_row("kept", &gone, None, &why));
            self.done
                .entries_kept
                .push((checkout.to_path_buf(), gone, why));
        }
    }

    /// A `branch-ref` row of the worktree registration `gone`: its entry named as
    /// `worktrees/<id>`, at `pin`'s commit where one is given, else at its HEAD.
    fn entry_row(
        &self,
        action: &str,
        gone: &Registration,
        pin: Option<&Pin>,
        text: &str,
    ) -> String {
        let id = gone
            .entry
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let name = format!("worktrees/{id}");
        let oid = pin.map_or_else(|| gone.head.as_deref().unwrap_or(""), |p| p.oid.as_str());
        self.row(action, &name, oid, pin.map(|p| p.name.as_str()), text)
    }

    /// The owed file could not be read or written, for `why`: one `unrecorded` row.
    fn unrecorded(&mut self, why: &str) {
        self.journal(&worktree_push_row(self.at, "unrecorded", None, why));
        self.done.unrecorded = Some(why.to_owned());
    }
}

impl Custody for Checkout<'_, '_, '_> {
    fn more(&mut self) -> bool {
        self.within()
    }

    fn pinning(&mut self, gone: &Registration, pins: &[Pin]) -> bool {
        for pin in pins {
            let text = format!(
                "its folder {} is gone and nothing lasting holds this commit: it is pinned in {} \
                 before git's entry goes",
                gone.folder.display(),
                pin.repo.display()
            );
            if !self.journal(&self.entry_row("pinning", gone, Some(pin), &text)) {
                return false;
            }
        }
        true
    }

    fn dropping(&mut self, gone: &Registration, pins: &[Pin]) -> Result<(), String> {
        for pin in pins {
            self.send(&pin.repo, &pin.name, &pin.oid).map_err(|why| {
                format!("its pin's push could not be recorded, so it stays: {why}")
            })?;
        }
        let text = format!(
            "its folder {} is gone; what it alone held stands on {} pins",
            gone.folder.display(),
            pins.len()
        );
        if self.journal(&self.entry_row("pruning", gone, None, &text)) {
            Ok(())
        } else {
            Err("the ledger took no row of its drop, so it stays".to_owned())
        }
    }
}

#[cfg(all(test, unix, debug_assertions))]
#[path = "disk_refs_tests.rs"]
mod tests;
