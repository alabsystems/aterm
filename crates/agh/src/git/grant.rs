// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! The grants (design §6.9): `<state>/agh/grants/<nonce>.json`, 0600, written by the agh
//! process that spawns a git child for a release-tier repository, read by the helper,
//! removed when the child exits.
//!
//! A grant's file name is its nonce, so a process of the same user that lists `grants/`
//! learns the live nonces. A hidden name would close nothing: the nonce is also in the git
//! child's environment (`AGH_GIT_GRANT`), which a process of the same user can read, and the
//! token files are that user's to read anyway. What bounds a grant is the helper's check,
//! never the nonce's secrecy: the spawner's `(pid, start)` must be an ancestor of the asking
//! helper within `ANCESTRY_DEPTH`, the slug must be the grant's, and it must not have expired
//! (at most `GRANT_MAX`). For a long-lived lane process the ancestry includes its build
//! children, for at most the run's bound.

#[cfg(any(test, debug_assertions))]
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::config::Dirs;
#[cfg(unix)]
use crate::error::Error;
#[cfg(unix)]
use crate::error::Result;
use crate::slug::Slug;

/// The most bytes a grant file may hold.
#[cfg(unix)]
const GRANT_FILE_MAX: usize = 4096;
/// The most grant files one sweep reads.
#[cfg(unix)]
const SWEEP_MAX: usize = 256;
/// The suffix of a grant's file name, `{nonce}.json`.
#[cfg(any(unix, test, debug_assertions))]
const GRANT_SUFFIX: &str = ".json";
/// The suffix of the name a grant is staged under, `.{nonce}.tmp`, before it is linked to
/// `{nonce}.json`.
#[cfg(unix)]
const TMP_SUFFIX: &str = ".tmp";

/// One grant file's contents:
/// `{"expires_at":N,"pid":N,"profile":"…","slug":"…","start":N,"v":1}`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grant {
    /// The nonce: 32 lowercase hex from 16 random bytes. It is the file's stem.
    pub nonce: String,
    /// The repository.
    pub slug: Slug,
    /// The profile it lets the helper answer: a release-tier owner's writer for a release-tier
    /// run, or the owner's writer for a `PushDelete`.
    pub profile: String,
    /// The spawner's pid (`std::process::id()`).
    pub pid: u32,
    /// The spawner's start: `boot.0` of `aterm_uds::process::read_process_start`, which takes
    /// an `i32` pid (`i32::try_from(pid)`) and answers an `Option`. It tells the spawner from a
    /// later process with the same pid.
    pub start: u64,
    /// `Fx::wall_secs` + at most `GRANT_MAX`.
    pub expires_at: u64,
}

impl Grant {
    /// Whether this grant answers `slug` (ASCII case-insensitive) at `now`: its slug is
    /// `slug` and it has not expired. Every `GrantView` asks it, so a test's view keeps the
    /// live view's rule.
    pub(crate) fn live_for(&self, slug: &Slug, now: u64) -> bool {
        self.slug == *slug && self.expires_at >= now
    }

    /// The grant file's JSON, keys sorted (`aterm_json`'s objects are sorted maps).
    #[cfg(unix)]
    fn to_json(&self) -> String {
        aterm_json::json!({
            "expires_at": self.expires_at,
            "pid": self.pid,
            "profile": self.profile.as_str(),
            "slug": self.slug.to_string(),
            "start": self.start,
            "v": 1,
        })
        .to_string()
    }

    /// The grant `nonce`'s file holds, or `None` for anything that is not one agh wrote:
    /// not JSON, another `v`, a field missing or of the wrong type, or a slug that does not
    /// parse.
    fn from_json(nonce: &str, bytes: &[u8]) -> Option<Grant> {
        let value: aterm_json::Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("v")?.as_u64()? != 1 {
            return None;
        }
        let number = |key: &str| value.get(key).and_then(aterm_json::Value::as_u64);
        let text = |key: &str| value.get(key).and_then(aterm_json::Value::as_str);
        Some(Grant {
            nonce: nonce.to_string(),
            slug: Slug::parse(text("slug")?).ok()?,
            profile: text("profile")?.to_string(),
            pid: u32::try_from(number("pid")?).ok()?,
            start: number("start")?,
            expires_at: number("expires_at")?,
        })
    }
}

/// Whether `nonce` is 32 lowercase hex: anything else names no grant file, and is never read.
fn is_nonce(nonce: &str) -> bool {
    aterm_codec::hex::is_lower(nonce, 32)
}

/// What the helper reads grants through. The live view reads `<state>/agh/grants` and the
/// process table. A test's view is a map with no file and no process.
pub trait GrantView {
    /// The grant `nonce` names, when it is live for `slug`: not expired at `now`, its
    /// spawner `(pid, start)` an ancestor of this process within `ANCESTRY_DEPTH`, and its
    /// slug `slug` (ASCII case-insensitive).
    fn find(&self, nonce: &str, slug: &Slug, now: u64) -> Option<Grant>;
}

/// The live view: the grant files under `dirs.grants()` and the ancestry walk
/// (`aterm_uds::process::read_process_parent` and `read_process_start`).
pub struct LiveGrants {
    /// `dirs.grants()`.
    dir: PathBuf,
}

impl LiveGrants {
    /// The view over `dirs.grants()`.
    #[must_use]
    pub fn new(dirs: &Dirs) -> LiveGrants {
        LiveGrants { dir: dirs.grants() }
    }
}

impl GrantView for LiveGrants {
    fn find(&self, nonce: &str, slug: &Slug, now: u64) -> Option<Grant> {
        if !is_nonce(nonce) {
            return None;
        }
        let grant = read_grant(&self.dir, nonce)?;
        (grant.live_for(slug, now) && in_tree(grant.pid, grant.start)).then_some(grant)
    }
}

/// The grant file `{nonce}.json` in `dir`, read through `read_private` (no symlink followed,
/// one link, this user's file, mode 0600, at most 4 KiB): `None` when it is refused, missing
/// or not a grant.
#[cfg(unix)]
fn read_grant(dir: &std::path::Path, nonce: &str) -> Option<Grant> {
    let bytes =
        aterm_uds::private_file::read_private(dir, &file_name(nonce), GRANT_FILE_MAX).ok()?;
    Grant::from_json(nonce, &bytes)
}

/// Off unix no grant is ever written, so none is read.
#[cfg(not(unix))]
fn read_grant(dir: &std::path::Path, nonce: &str) -> Option<Grant> {
    let _ = (dir, nonce);
    None
}

/// The grant `nonce`'s file name in `grants/`: `{nonce}.json`.
#[cfg(any(unix, test, debug_assertions))]
fn file_name(nonce: &str) -> String {
    format!("{nonce}{GRANT_SUFFIX}")
}

/// This process's start on the boot clock, `None` when the kernel's record cannot be read.
fn start_of(pid: i32) -> Option<u64> {
    aterm_uds::process::read_process_start(pid).map(|start| start.boot.0)
}

/// Whether the process `(pid, start)` is this process or one of its ancestors within
/// `ANCESTRY_DEPTH` steps. The walk starts at this process's own pid and climbs
/// `read_process_parent`. It accepts the first pid equal to `pid` only when that process's
/// start is `start`, so a recycled pid never matches. A parent that cannot be read, or a pid
/// that does not fit an `i32`, ends the walk with no match.
fn in_tree(pid: u32, start: u64) -> bool {
    let Ok(mut at) = i32::try_from(std::process::id()) else {
        return false;
    };
    for step in 0..=super::ANCESTRY_DEPTH {
        if u32::try_from(at).is_ok_and(|me| me == pid) {
            return start_of(at) == Some(start);
        }
        if step == super::ANCESTRY_DEPTH {
            break;
        }
        let Some(parent) = aterm_uds::process::read_process_parent(at)
            .and_then(|parent| i32::try_from(parent).ok())
        else {
            return false;
        };
        at = parent;
    }
    false
}

/// A test's grants: fixed answers by nonce, with the ancestry taken as given.
#[cfg(any(test, debug_assertions))]
#[derive(Clone, Debug, Default)]
pub struct MapGrants {
    /// Each grant by its nonce, and whether its spawner is an ancestor.
    grants: BTreeMap<String, (Grant, bool)>,
}

#[cfg(any(test, debug_assertions))]
impl MapGrants {
    /// No grant: `MapGrants::default()`.
    #[must_use]
    pub fn new() -> MapGrants {
        MapGrants::default()
    }

    /// `grant`, its spawner an ancestor when `in_tree`.
    #[must_use]
    pub fn with(mut self, grant: Grant, in_tree: bool) -> MapGrants {
        self.grants.insert(grant.nonce.clone(), (grant, in_tree));
        self
    }
}

#[cfg(any(test, debug_assertions))]
impl GrantView for MapGrants {
    fn find(&self, nonce: &str, slug: &Slug, now: u64) -> Option<Grant> {
        if !is_nonce(nonce) {
            return None;
        }
        let (grant, in_tree) = self.grants.get(nonce)?;
        (*in_tree && grant.live_for(slug, now)).then(|| grant.clone())
    }
}

/// Write a grant for `slug` and `profile`, spawned by this process, for `ttl` (at most
/// `GRANT_MAX`), in `dirs.grants()`, made with its parents by
/// `config::make_state_subdir(dirs, &dirs.grants())`, mode 0600, `create_new`. So the first
/// grant on a state directory nothing has made yet is written.
///
/// The file is published whole: its JSON goes into `.{nonce}.tmp` (`create_new`, 0600), which
/// is then hard-linked to `{nonce}.json` (a link, like `create_new`, never replaces a file)
/// and unlinked. So another run's `sweep` never reads `{nonce}.json` half written and removes
/// it as unparseable, and no helper finds it before it is whole.
///
/// # Errors
///
/// could_not_run for the directory or the file, or why `this process's start cannot be read`
/// when `read_process_start` answers `None`. It is unix only, as every caller is.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AghGitGrant",
        action = "Spawn",
        project = "agh::git::grant::project_grant_file"
    )
)]
#[cfg(unix)]
pub(crate) fn write(
    dirs: &Dirs,
    slug: &Slug,
    profile: &str,
    now: u64,
    ttl: std::time::Duration,
) -> Result<Grant> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let pid = std::process::id();
    let start = i32::try_from(pid)
        .ok()
        .and_then(start_of)
        .ok_or_else(|| Error::local("this process's start cannot be read"))?;
    let nonce = aterm_uds::rand::hex_token::<16>()
        .map_err(|e| Error::local(format!("a git grant's nonce cannot be drawn: {e}")))?;
    let grant = Grant {
        nonce,
        slug: slug.clone(),
        profile: profile.to_string(),
        pid,
        start,
        expires_at: now.saturating_add(ttl.min(super::GRANT_MAX).as_secs()),
    };
    let dir = dirs.grants();
    let shown = crate::identity::tilde(&dir, &dirs.home);
    crate::config::make_state_subdir(dirs, &dir)
        .map_err(|e| Error::local(format!("the git grants directory {shown}: {e}")))?;
    let path = dir.join(file_name(&grant.nonce));
    let tmp_name = format!(".{}{TMP_SUFFIX}", grant.nonce);
    let tmp = dir.join(&tmp_name);
    let published = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| file.write_all(grant.to_json().as_bytes()))
        .and_then(|()| std::fs::hard_link(&tmp, &path));
    // The staging name goes whatever happened: once linked, the grant is `{nonce}.json`.
    let _ = aterm_uds::private_file::remove_private(&dir, &tmp_name);
    published.map_err(|e| Error::local(format!("a git grant in {shown}: {e}")))?;
    Ok(grant)
}

/// Remove the grant's file. A file already gone is fine.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AghGitGrant",
        action = "Consume",
        project = "agh::git::grant::project_grant_file"
    )
)]
#[cfg(unix)]
pub(crate) fn remove(dirs: &Dirs, grant: &Grant) {
    // A grant that cannot be removed expires on its own, and the next sweep takes it.
    let _ = aterm_uds::private_file::remove_private(&dirs.grants(), &file_name(&grant.nonce));
}

/// Remove every grant file whose `expires_at` has passed: the spawner killed before it
/// removed its own (the model's `Expire`). Each `GitRun` calls it before writing its grant.
///
/// It reads at most 256 files, each through `read_private`. A file that does not parse is
/// removed too. A file `read_private` refuses (a symlink, a second link, another owner, a
/// loose mode, over 4 KiB) is left alone: it is not one agh wrote, and nothing trusts it.
/// A staging file `.{nonce}.tmp` whose mtime is more than `GRANT_MAX` before `now` is removed:
/// its writer was killed before it published the grant, which no helper ever found.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AghGitGrant",
        action = "Expire",
        project = "agh::git::grant::project_grant_file"
    )
)]
#[cfg(unix)]
pub(crate) fn sweep(dirs: &Dirs, now: u64) {
    let dir = dirs.grants();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten().take(SWEEP_MAX) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_stale_staging(&entry, name, now) {
            let _ = aterm_uds::private_file::remove_private(&dir, name);
            continue;
        }
        let Some(nonce) = name.strip_suffix(GRANT_SUFFIX) else {
            continue;
        };
        let Ok(bytes) = aterm_uds::private_file::read_private(&dir, name, GRANT_FILE_MAX) else {
            continue;
        };
        let stale = Grant::from_json(nonce, &bytes).is_none_or(|grant| grant.expires_at < now);
        if stale {
            let _ = aterm_uds::private_file::remove_private(&dir, name);
        }
    }
}

/// Whether `name` is a grant's staging file (`.{nonce}.tmp`, a plain file) whose mtime is more
/// than `GRANT_MAX` before `now`. A staging file being written now is younger, and is kept.
#[cfg(unix)]
fn is_stale_staging(entry: &std::fs::DirEntry, name: &str, now: u64) -> bool {
    let staged = name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_suffix(TMP_SUFFIX))
        .is_some_and(is_nonce);
    if !staged {
        return false;
    }
    let Ok(meta) = entry.metadata() else {
        return false;
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| at.as_secs());
    meta.is_file() && modified.is_some_and(|at| at.saturating_add(super::GRANT_MAX.as_secs()) < now)
}

/// The model's `grant` for the grant `nonce` names, read from `dirs.grants()` at `now`: 0 when
/// its file is absent, 1 when it is there and unexpired, 2 when it is there and expired (a
/// spawner killed before it removed its own). `Spawn` (`write`), `Consume` (`remove`) and
/// `Expire` (`sweep`) are each judged by it after they run.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn project_grant_file(dirs: &Dirs, nonce: &str, now: u64) -> i64 {
    let path = dirs.grants().join(file_name(nonce));
    let Ok((bytes, _)) = aterm_uds::atomic_file::read_regular(&path, 4096) else {
        return 0;
    };
    match Grant::from_json(nonce, &bytes) {
        Some(grant) if grant.expires_at >= now => 1,
        _ => 2,
    }
}

/// The host of `AghGitGrant`'s waivers: the world's toggles, the spawner's death, and the
/// negative controls.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "NextTier",
        reason = "Bounded nondeterministic environment selection: what git sent and where"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "TogglePath",
        reason = "Bounded nondeterministic environment selection: what git sent and where"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "ToggleHost",
        reason = "Bounded nondeterministic environment selection: what git sent and where"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "ToggleTty",
        reason = "Bounded nondeterministic environment selection: what git sent and where"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "NextOp",
        reason = "Bounded nondeterministic environment selection: what git sent and where"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "ToggleSlug",
        reason = "Bounded nondeterministic environment selection: what git sent and where"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "ToggleAsker",
        reason = "Bounded nondeterministic environment selection: which process asks"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "ChildExits",
        reason = "Bounded nondeterministic environment selection: the git child ended"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "SpawnerKilled",
        reason = "Bounded nondeterministic environment selection: the spawner died before it removed its grant"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "Reset",
        reason = "Bounded nondeterministic environment selection: the next request"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyPathless",
        reason = "Buggy=1 negative control only; the helper has no such branch: PathlessGetsNothing is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyForeignHost",
        reason = "Buggy=1 negative control only; the helper has no such branch: GithubOnly is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyTtyAnswers",
        reason = "Buggy=1 negative control only; the helper has no such branch: NoTerminalDoor is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyEraseAnswers",
        reason = "Buggy=1 negative control only; the helper has no such branch: StoreEraseInert is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyReleaseByRead",
        reason = "Buggy=1 negative control only; the helper has no such branch: ReleaseOwnerOnlyByGrant is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyStaleGrant",
        reason = "Buggy=1 negative control only; the helper has no such branch: GrantLive is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyOtherPid",
        reason = "Buggy=1 negative control only; the helper has no such branch: GrantPidBound is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyWrongSlug",
        reason = "Buggy=1 negative control only; the helper has no such branch: GrantSlugBound is held by Answer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AghGitGrant",
        action = "BuggyReleaseForDev",
        reason = "Buggy=1 negative control only; the helper has no such branch: ReleaseStaysHome is held by Answer"
    )
)]
#[doc(hidden)]
pub fn git_grant_environment() {}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use aterm_uds::spawnfd::Stdio;

    use super::{in_tree, start_of};

    /// The pid as `in_tree` and `start_of` take it.
    fn pid(raw: u32) -> i32 {
        i32::try_from(raw).expect("a pid that fits an i32")
    }

    /// The walk climbs from this process: this process and its parent are in the tree, at
    /// their own starts, and a live child of this process, at its own start, is not. So a
    /// grant names a spawner the asking helper descends from, never any live process whose
    /// pid and start match (the model's `GrantPidBound`, against the real walk).
    #[test]
    fn the_ancestry_walk_holds_ancestors_and_not_a_live_child() {
        let me = std::process::id();
        let my_start = start_of(pid(me)).expect("this process's start");
        assert!(in_tree(me, my_start), "this process is in its own tree");
        assert!(!in_tree(me, my_start + 1), "a recycled pid never matches");
        let parent = aterm_uds::process::read_process_parent(pid(me)).expect("the parent");
        let parent_start = start_of(pid(parent)).expect("the parent's start");
        assert!(in_tree(parent, parent_start), "the parent is an ancestor");
        let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
        let mut child = aterm_uds::spawnfd::command(&cat)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the child");
        let stdin = aterm_uds::deadline::cat_echoes(&mut child);
        let child_pid = child.id();
        let child_start = start_of(pid(child_pid)).expect("the live child's start");
        let outside = in_tree(child_pid, child_start);
        drop(stdin);
        let _ = child.wait();
        assert!(!outside, "a live child is outside the asker's ancestry");
    }
}
