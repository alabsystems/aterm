// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`FakeHost`](crate::host::fake::FakeHost): in-memory [`ReleaseHost`] for producer tests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{HostError, Probe, Release, ReleaseHost, Replace, Slug};

/// One release's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FakeRelease {
    /// A draft: the download host answers 404 for its assets.
    pub draft: bool,
    /// A prerelease.
    pub prerelease: bool,
    /// Asset name -> bytes.
    pub assets: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Default)]
struct State {
    repos: BTreeMap<Slug, BTreeMap<String, FakeRelease>>,
    clone_sources: BTreeMap<Slug, PathBuf>,
    fail_once: Vec<(Slug, String, String)>,
    unreachable_owners: BTreeSet<String>,
    private: BTreeSet<Slug>,
    not_visible: BTreeSet<Slug>,
    unanswered_repo: BTreeSet<Slug>,
    stray_drafts: BTreeMap<Slug, BTreeMap<String, usize>>,
    notes: BTreeMap<(Slug, String), String>,
    anonymous: bool,
    calls: Vec<String>,
    armed: Vec<(FakeOp, FakeFail)>,
    own: BTreeSet<(Slug, String)>,
    immutable: BTreeSet<(Slug, String)>,
    changes: usize,
}

/// The write an armed failure fires on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeOp {
    /// [`ReleaseHost::create`].
    Create,
    /// [`ReleaseHost::upload`].
    Upload,
    /// [`ReleaseHost::publish_draft`].
    PublishDraft,
    /// [`ReleaseHost::write_preflight`].
    Preflight,
}

/// What an armed write does instead of succeeding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeFail {
    /// A create killed after its draft holds every file: the draft stays, unpublished.
    KilledCreate,
    /// The write lands whole, and its answer is lost.
    LostResponse,
    /// Nothing changes, and the host refuses.
    Refuse,
    /// An upload cut short after its first file.
    Partial,
}

/// Whether `fail` is a failure `op` can be armed with (the table of plan §15, body 10).
const fn armable(op: FakeOp, fail: FakeFail) -> bool {
    matches!(
        (op, fail),
        (
            FakeOp::Create,
            FakeFail::KilledCreate | FakeFail::LostResponse | FakeFail::Refuse
        ) | (
            FakeOp::Upload,
            FakeFail::Partial | FakeFail::LostResponse | FakeFail::Refuse
        ) | (
            FakeOp::PublishDraft,
            FakeFail::LostResponse | FakeFail::Refuse
        ) | (FakeOp::Preflight, FakeFail::Refuse)
    )
}

/// The refusal every armed `Refuse` answers.
fn refused_by_the_test() -> HostError {
    HostError::Refused("refused by the test".into())
}

/// The answer every armed `LostResponse` gives once its write has landed.
fn the_answer_was_lost() -> HostError {
    HostError::Unreachable("the answer was lost".into())
}

/// The in-memory release host.
#[derive(Default)]
pub struct FakeHost {
    state: Mutex<State>,
}

impl FakeHost {
    /// An empty host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lay release `tag` on `slug` as given.
    pub fn put(&self, slug: &Slug, tag: &str, release: FakeRelease) {
        self.state()
            .repos
            .entry(slug.clone())
            .or_default()
            .insert(tag.to_string(), release);
    }

    /// Lay one asset, creating a published prerelease `tag` when there is none.
    pub fn put_asset(&self, slug: &Slug, tag: &str, name: &str, bytes: impl Into<Vec<u8>>) {
        self.state()
            .repos
            .entry(slug.clone())
            .or_default()
            .entry(tag.to_string())
            .or_insert_with(|| FakeRelease {
                prerelease: true,
                ..FakeRelease::default()
            })
            .assets
            .insert(name.to_string(), bytes.into());
    }

    /// Mark `tag` a draft (or not).
    pub fn set_draft(&self, slug: &Slug, tag: &str, draft: bool) {
        if let Some(r) = self
            .state()
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
        {
            r.draft = draft;
        }
    }

    /// The release as it stands, if any.
    #[must_use]
    pub fn release_of(&self, slug: &Slug, tag: &str) -> Option<FakeRelease> {
        self.state()
            .repos
            .get(slug)
            .and_then(|tags| tags.get(tag))
            .cloned()
    }

    /// The notes release `tag` on `slug` was created with, if this host created it.
    #[must_use]
    pub fn notes_of(&self, slug: &Slug, tag: &str) -> Option<String> {
        self.state()
            .notes
            .get(&(slug.clone(), tag.to_string()))
            .cloned()
    }

    /// One asset's bytes, if present.
    #[must_use]
    pub fn asset(&self, slug: &Slug, tag: &str, name: &str) -> Option<Vec<u8>> {
        self.release_of(slug, tag)
            .and_then(|r| r.assets.get(name).cloned())
    }

    /// The next download of this one asset fails as unreachable (an HTTP 500), once.
    pub fn fail_once(&self, slug: &Slug, tag: &str, asset: &str) {
        self.state()
            .fail_once
            .push((slug.clone(), tag.to_string(), asset.to_string()));
    }

    /// Every authenticated call on `owner`'s repos answers unreachable (or stops).
    pub fn set_unreachable(&self, owner: &str, unreachable: bool) {
        let mut s = self.state();
        if unreachable {
            s.unreachable_owners.insert(owner.to_string());
        } else {
            s.unreachable_owners.remove(owner);
        }
    }

    /// The download host serves nothing for `slug` (a private repo), or serves it again.
    pub fn set_private(&self, slug: &Slug, private: bool) {
        let mut s = self.state();
        if private {
            s.private.insert(slug.clone());
        } else {
            s.private.remove(slug);
        }
    }

    /// Every authenticated read of `slug` answers unreachable.
    pub fn set_not_visible(&self, slug: &Slug, not_visible: bool) {
        let mut s = self.state();
        if not_visible {
            s.not_visible.insert(slug.clone());
        } else {
            s.not_visible.remove(slug);
        }
    }

    /// [`ReleaseHost::repo_seen`] on `slug` cannot tell (the repository read did not answer).
    pub fn set_repo_unanswered(&self, slug: &Slug, unanswered: bool) {
        let mut s = self.state();
        if unanswered {
            s.unanswered_repo.insert(slug.clone());
        } else {
            s.unanswered_repo.remove(slug);
        }
    }

    /// A draft beside the published release `tag`.
    /// # Panics
    /// Unless `slug` holds a published release `tag`, so the mistake fails the test at its own
    /// line.
    pub fn put_stray_draft(&self, slug: &Slug, tag: &str) {
        let mut s = self.state();
        let published = s
            .repos
            .get(slug)
            .and_then(|tags| tags.get(tag))
            .is_some_and(|r| !r.draft);
        assert!(
            published,
            "put_stray_draft: {slug} holds no published release {tag}"
        );
        *s.stray_drafts
            .entry(slug.clone())
            .or_default()
            .entry(tag.to_string())
            .or_default() += 1;
    }

    /// A keyless builder ([`ReleaseHost::reads_anonymously`]).
    pub fn set_anonymous_reads(&self, anonymous: bool) {
        self.state().anonymous = anonymous;
    }

    /// `clone_repo(slug)` clones this local repository.
    pub fn add_clone_source(&self, slug: &Slug, path: &Path) {
        self.state()
            .clone_sources
            .insert(slug.clone(), path.to_path_buf());
    }

    /// A host holding everything this one holds (its releases, clone sources, armed
    /// failures and call log), with each clone source under `from` moved under `to`: a
    /// world made once and laid down again at another root.
    #[must_use]
    pub fn forked(&self, from: &Path, to: &Path) -> Self {
        let mut state = self.state().clone();
        for path in state.clone_sources.values_mut() {
            if let Ok(rel) = path.strip_prefix(from) {
                *path = to.join(rel);
            }
        }
        Self {
            state: Mutex::new(state),
        }
    }

    /// Every call so far, one line each (`<op> <owner>/<repo> [<tag> [<assets>]]`).
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        self.state().calls.clone()
    }

    /// How many writes (`create`, `upload`, `publish_draft`) changed what this host holds: a
    /// write that finds every file already there with the same bytes changes nothing.
    #[must_use]
    pub fn changes(&self) -> usize {
        self.state().changes
    }

    /// The next `op` (any slug) does `fail` instead; one arming fires once.
    ///
    /// `Refuse` fires on the next call of `op`, before anything else it decides. A write
    /// failure (`KilledCreate`, `LostResponse`, `Partial`) fires on the next call of `op` that
    /// writes: a call refused or converged before any write leaves it armed, as an agh-fake
    /// fault on a request waits for that request.
    ///
    /// # Panics
    ///
    /// For a pair no write of `op` can do (plan §15, body 10's table), naming the pair, so the
    /// mistake fails the test at its own line.
    pub fn fail_next(&self, op: FakeOp, fail: FakeFail) {
        assert!(
            armable(op, fail),
            "fail_next: {op:?} cannot be armed with {fail:?}"
        );
        self.state().armed.push((op, fail));
    }

    /// Make release `tag` of `slug` immutable, or not.
    pub fn set_immutable(&self, slug: &Slug, tag: &str, immutable: bool) {
        let key = (slug.clone(), tag.to_string());
        let mut s = self.state();
        if immutable {
            s.immutable.insert(key);
        } else {
            s.immutable.remove(&key);
        }
    }

    /// The failure armed for the next `op`, disarmed.
    fn armed(&self, op: FakeOp) -> Option<FakeFail> {
        take_armed(&mut self.state(), op)
    }

    /// Whether the next `op` is armed to `Refuse`, which fires before anything else the call
    /// decides; when it is, the arming is taken.
    fn refuses(&self, op: FakeOp) -> bool {
        let mut s = self.state();
        let next = s.armed.iter().find(|(o, _)| *o == op).map(|(_, f)| *f);
        next == Some(FakeFail::Refuse) && take_armed(&mut s, op).is_some()
    }

    /// Lay `files` as release `tag` of `slug`, marked this host's own, with `notes`.
    fn lay_own(
        &self,
        slug: &Slug,
        tag: &str,
        notes: &str,
        draft: bool,
        assets: BTreeMap<String, Vec<u8>>,
    ) {
        {
            let mut s = self.state();
            s.notes
                .insert((slug.clone(), tag.to_string()), notes.to_string());
            s.own.insert((slug.clone(), tag.to_string()));
            s.changes += 1;
        }
        self.put(
            slug,
            tag,
            FakeRelease {
                draft,
                prerelease: true,
                assets,
            },
        );
    }

    /// A `create` over release `tag`, which this host marked its own, as agh's created-intent
    /// rule completes it (plan §15). A published one is never uploaded to, and answers `Ok`
    /// only when it holds every file with the same bytes and is still a prerelease, as agh's
    /// publish guard converges only a release published as its patch would leave it. A draft
    /// is refused where a file has
    /// other bytes; otherwise it gains the files it lacks and is published only when it then
    /// holds no name the rerun did not bring, as agh's exact-set publish guard is. An armed
    /// write failure fires at the publish, so a call refused or converged before it leaves the
    /// arming for the next `create` that writes.
    fn complete_own(
        &self,
        slug: &Slug,
        tag: &str,
        incoming: Vec<(String, Vec<u8>)>,
    ) -> Result<(), HostError> {
        let mut guard = self.state();
        let s = &mut *guard;
        let release = s
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
            .ok_or(HostError::NotFound)?;
        if !release.draft {
            let same = release.assets.len() == incoming.len()
                && incoming
                    .iter()
                    .all(|(name, bytes)| release.assets.get(name) == Some(bytes));
            return if !same {
                Err(HostError::Refused(format!(
                    "{tag} on {slug} is published with other files"
                )))
            } else if !release.prerelease {
                Err(HostError::Refused(format!(
                    "{tag} on {slug} is published as a full release, not a prerelease"
                )))
            } else {
                Ok(())
            };
        }
        if let Some((name, _)) = incoming
            .iter()
            .find(|(name, bytes)| release.assets.get(name).is_some_and(|there| there != bytes))
        {
            return Err(HostError::Refused(format!(
                "{name} already exists on {slug} {tag}"
            )));
        }
        let brought: BTreeSet<String> = incoming.iter().map(|(name, _)| name.clone()).collect();
        s.changes += usize::from(lands(&mut release.assets, incoming));
        let extra: Vec<&str> = release
            .assets
            .keys()
            .filter(|name| !brought.contains(*name))
            .map(String::as_str)
            .collect();
        if !extra.is_empty() {
            return Err(HostError::Refused(format!(
                "{tag} on {slug} holds {}, which this create did not bring",
                extra.join(", ")
            )));
        }
        let armed = take_armed(s, FakeOp::Create);
        let release = s
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
            .ok_or(HostError::NotFound)?;
        match armed {
            Some(FakeFail::KilledCreate) => Err(HostError::Unreachable("killed".into())),
            Some(FakeFail::LostResponse) => {
                release.draft = false;
                s.changes += 1;
                Err(the_answer_was_lost())
            }
            Some(other) => panic!("fail_next: Create cannot be armed with {other:?}"),
            None => {
                release.draft = false;
                s.changes += 1;
                Ok(())
            }
        }
    }

    fn log(&self, line: String) {
        self.state().calls.push(line);
    }

    fn reachable(&self, slug: &Slug) -> Result<(), HostError> {
        if self.state().not_visible.contains(slug) {
            return Err(HostError::Unreachable(format!(
                "{slug} is not visible to this identity (HTTP 404)"
            )));
        }
        if self.state().anonymous {
            return Err(HostError::Unreachable(
                "there is no token file on this keyless builder".into(),
            ));
        }
        if self.state().unreachable_owners.contains(&slug.owner) {
            Err(HostError::Unreachable(format!(
                "{} did not answer (HTTP 500)",
                slug.owner
            )))
        } else {
            Ok(())
        }
    }

    fn file_name(path: &Path) -> Result<String, HostError> {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .ok_or_else(|| HostError::Refused(format!("{} names no file", path.display())))
    }

    fn write_fresh(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        let _ = std::fs::remove_file(path);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        opts.open(path)?.write_all(bytes)
    }

    fn read_files(files: &[PathBuf]) -> Result<Vec<(String, Vec<u8>)>, HostError> {
        files
            .iter()
            .map(|f| {
                let (bytes, _) = aterm_uds::atomic_file::read_regular(f, 512 * 1024 * 1024)
                    .map_err(|e| HostError::Refused(format!("read {}: {e}", f.display())))?;
                Ok((Self::file_name(f)?, bytes))
            })
            .collect()
    }

    fn public(&self, slug: &Slug, tag: &str, asset: &str) -> Option<Vec<u8>> {
        let s = self.state();
        if s.private.contains(slug) || s.not_visible.contains(slug) {
            return None;
        }
        s.repos
            .get(slug)
            .and_then(|tags| tags.get(tag))
            .filter(|r| !r.draft)
            .and_then(|r| r.assets.get(asset).cloned())
    }

    /// `fetch_public`'s answer. A final failure never removes caller-owned bytes at `dest`.
    fn fetch_public_into(
        &self,
        slug: &Slug,
        tag: &str,
        asset: &str,
        dest: &Path,
        cap: u64,
    ) -> Probe {
        let Some(bytes) = self.public(slug, tag, asset) else {
            return Probe::Absent;
        };
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap {
            return Probe::Unknown(format!("{asset} is over the {cap}-byte cap"));
        }
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty())
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Probe::Unknown(format!("mkdir {}: {e}", parent.display()));
        }
        match Self::write_fresh(dest, &bytes) {
            Ok(()) => Probe::Present,
            Err(e) => Probe::Unknown(format!("write {}: {e}", dest.display())),
        }
    }
}

impl ReleaseHost for FakeHost {
    fn releases(&self, slug: &Slug) -> Result<Vec<Release>, HostError> {
        self.log(format!("releases {slug}"));
        self.reachable(slug)?;
        let s = self.state();
        let tags = s.repos.get(slug).ok_or(HostError::NotFound)?;
        let strays = s.stray_drafts.get(slug);
        let mut out = Vec::new();
        for (tag, r) in tags {
            out.push(Release {
                tag: tag.clone(),
                draft: r.draft,
                prerelease: r.prerelease,
                assets: r.assets.keys().cloned().collect(),
            });
            let n = strays.and_then(|t| t.get(tag)).copied().unwrap_or(0);
            out.extend((0..n).map(|_| Release {
                tag: tag.clone(),
                draft: true,
                prerelease: true,
                assets: Vec::new(),
            }));
        }
        Ok(out)
    }

    fn repo_seen(&self, slug: &Slug) -> Result<bool, HostError> {
        self.log(format!("repo_seen {slug}"));
        let s = self.state();
        if s.unreachable_owners.contains(&slug.owner) || s.unanswered_repo.contains(slug) {
            return Err(HostError::Unreachable(format!(
                "{slug} did not answer (HTTP 500)"
            )));
        }
        let identity_sees = !s.anonymous && !s.not_visible.contains(slug);
        let anonymous_sees = !s.private.contains(slug);
        Ok(s.repos.contains_key(slug) && (identity_sees || anonymous_sees))
    }

    fn release(&self, slug: &Slug, tag: &str) -> Result<Release, HostError> {
        self.log(format!("release {slug} {tag}"));
        self.reachable(slug)?;
        let r = self.release_of(slug, tag).ok_or(HostError::NotFound)?;
        Ok(Release {
            tag: tag.to_string(),
            draft: r.draft,
            prerelease: r.prerelease,
            assets: r.assets.keys().cloned().collect(),
        })
    }

    fn download(
        &self,
        slug: &Slug,
        tag: &str,
        assets: &[&str],
        dir: &Path,
    ) -> Result<(), HostError> {
        self.log(format!("download {slug} {tag} {}", assets.join(" ")));
        self.reachable(slug)?;
        let release = self.release_of(slug, tag).ok_or(HostError::NotFound)?;
        std::fs::create_dir_all(dir)
            .map_err(|e| HostError::Refused(format!("mkdir {}: {e}", dir.display())))?;
        for name in assets {
            let mut s = self.state();
            let key = (slug.clone(), tag.to_string(), (*name).to_string());
            if let Some(at) = s.fail_once.iter().position(|k| *k == key) {
                s.fail_once.remove(at);
                return Err(HostError::Unreachable(format!("HTTP 500 on {name}")));
            }
            drop(s);
            let bytes = release.assets.get(*name).ok_or(HostError::NotFound)?;
            Self::write_fresh(&dir.join(name), bytes)
                .map_err(|e| HostError::Refused(format!("write {name}: {e}")))?;
        }
        Ok(())
    }

    fn create(
        &self,
        slug: &Slug,
        tag: &str,
        _title: &str,
        notes: &str,
        files: &[PathBuf],
    ) -> Result<(), HostError> {
        self.log(format!("create {slug} {tag}"));
        self.reachable(slug)?;
        let incoming = Self::read_files(files)?;
        // An armed `Refuse` fires first, whatever is there. A release that is there decides
        // next: another's is refused, as the unarmed create and agh's created-intent rule
        // refuse it, writing nothing, so an armed write failure waits for the next create that
        // writes, as agh-fake's fault on a request waits for that request; this host's own is
        // completed, and an armed write failure fires at its publish.
        if self.refuses(FakeOp::Create) {
            return Err(refused_by_the_test());
        }
        let key = (slug.clone(), tag.to_string());
        match self.release_of(slug, tag) {
            None => {}
            // agh's created-intent rule converges only a request that agrees with the intent
            // that made the release, its body among it: other notes are another's create.
            Some(_)
                if self.state().own.contains(&key)
                    && self.state().notes.get(&key).map(String::as_str) == Some(notes) =>
            {
                return self.complete_own(slug, tag, incoming);
            }
            Some(_) => {
                return Err(HostError::Refused(format!(
                    "{slug} already has a release {tag}"
                )));
            }
        }
        match self.armed(FakeOp::Create) {
            Some(FakeFail::KilledCreate) => {
                self.lay_own(slug, tag, notes, true, incoming.into_iter().collect());
                return Err(HostError::Unreachable("killed".into()));
            }
            Some(FakeFail::LostResponse) => {
                self.lay_own(slug, tag, notes, false, incoming.into_iter().collect());
                return Err(the_answer_was_lost());
            }
            Some(other) => panic!("fail_next: Create cannot be armed with {other:?}"),
            None => {}
        }
        // Marked own as every create's release is, so a rerun converges as agh's created-intent
        // rule converges it.
        self.lay_own(slug, tag, notes, false, incoming.into_iter().collect());
        Ok(())
    }

    fn upload(
        &self,
        slug: &Slug,
        tag: &str,
        files: &[PathBuf],
        replace: Replace,
    ) -> Result<(), HostError> {
        self.log(format!("upload {slug} {tag} {replace:?}"));
        self.reachable(slug)?;
        let incoming = Self::read_files(files)?;
        // An armed `Refuse` fires first. An armed write failure fires only on an upload that
        // writes: one refused (no release, an immutable one, other bytes under `Never`) or one
        // that finds every file there with the same bytes leaves it armed, as agh sends no
        // POST for either.
        if self.refuses(FakeOp::Upload) {
            return Err(refused_by_the_test());
        }
        let key = (slug.clone(), tag.to_string());
        let mut guard = self.state();
        let s = &mut *guard;
        let immutable = s.immutable.contains(&key);
        let release = s
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
            .ok_or(HostError::NotFound)?;
        if immutable {
            return Err(HostError::Refused(format!("{tag} on {slug} is immutable")));
        }
        if replace == Replace::Never
            && let Some((name, _)) = incoming
                .iter()
                .find(|(n, bytes)| release.assets.get(n).is_some_and(|there| there != bytes))
        {
            return Err(HostError::Refused(format!(
                "{name} already exists on {slug} {tag}"
            )));
        }
        if incoming
            .iter()
            .all(|(name, bytes)| release.assets.get(name) == Some(bytes))
        {
            return Ok(());
        }
        let armed = take_armed(s, FakeOp::Upload);
        let release = s
            .repos
            .get_mut(slug)
            .and_then(|tags| tags.get_mut(tag))
            .ok_or(HostError::NotFound)?;
        match armed {
            Some(FakeFail::Partial) => {
                s.changes += usize::from(lands(&mut release.assets, incoming.into_iter().take(1)));
                Err(HostError::Unreachable("cut short".into()))
            }
            Some(FakeFail::LostResponse) => {
                s.changes += usize::from(lands(&mut release.assets, incoming));
                Err(the_answer_was_lost())
            }
            Some(other) => panic!("fail_next: Upload cannot be armed with {other:?}"),
            None => {
                s.changes += usize::from(lands(&mut release.assets, incoming));
                Ok(())
            }
        }
    }

    fn publish_draft(&self, slug: &Slug, tag: &str) -> Result<(), HostError> {
        self.log(format!("publish-draft {slug} {tag}"));
        self.reachable(slug)?;
        // An armed `Refuse` fires first; an armed `LostResponse` fires only on a publish that
        // writes, so a missing tag or a release already published leaves it armed.
        if self.refuses(FakeOp::PublishDraft) {
            return Err(refused_by_the_test());
        }
        match self.release_of(slug, tag) {
            None => return Err(HostError::NotFound),
            Some(release) if release.draft => {
                self.set_draft(slug, tag, false);
                self.state().changes += 1;
            }
            Some(_) => return Ok(()),
        }
        match self.armed(FakeOp::PublishDraft) {
            Some(FakeFail::LostResponse) => Err(the_answer_was_lost()),
            Some(other) => panic!("fail_next: PublishDraft cannot be armed with {other:?}"),
            None => Ok(()),
        }
    }

    fn write_preflight(&self, slug: &Slug) -> Result<(), HostError> {
        self.log(format!("preflight {slug}"));
        match self.armed(FakeOp::Preflight) {
            Some(FakeFail::Refuse) => Err(refused_by_the_test()),
            Some(other) => panic!("fail_next: Preflight cannot be armed with {other:?}"),
            None => Ok(()),
        }
    }

    fn probe_public(&self, slug: &Slug, tag: &str, asset: &str) -> Probe {
        self.log(format!("probe {slug} {tag} {asset}"));
        if self.public(slug, tag, asset).is_some() {
            Probe::Present
        } else {
            Probe::Absent
        }
    }

    fn fetch_public(&self, slug: &Slug, tag: &str, asset: &str, dest: &Path, cap: u64) -> Probe {
        self.log(format!("fetch {slug} {tag} {asset}"));
        self.fetch_public_into(slug, tag, asset, dest, cap)
    }

    fn reads_anonymously(&self) -> bool {
        self.state().anonymous
    }

    fn clone_repo(&self, slug: &Slug, dest: &Path) -> Result<(), HostError> {
        self.log(format!("clone {slug}"));
        self.reachable(slug)?;
        let source = self.clone_source(slug)?;
        git(crate::testkit::git()
            .args(["-c", "init.templateDir=", "clone"])
            .args(["--quiet", "--config", "maintenance.auto=false"])
            .arg(&source)
            .arg(dest))
    }

    fn fetch_repo(&self, slug: &Slug, dir: &Path) -> Result<(), HostError> {
        self.log(format!("fetch-repo {slug}"));
        self.reachable(slug)?;
        let source = self.clone_source(slug)?;
        git(crate::testkit::git()
            .arg("-C")
            .arg(dir)
            .args(["fetch", "--quiet", "--tags", "--force"])
            .arg(&source)
            .arg("+refs/heads/*:refs/remotes/origin/*"))
    }

    fn fetch_main(&self, slug: &Slug, dir: &Path, into: &str) -> Result<(), HostError> {
        self.log(format!("fetch-main {slug}"));
        self.reachable(slug)?;
        let source = self.clone_source(slug)?;
        git(crate::testkit::git()
            .arg("-C")
            .arg(dir)
            .args(["fetch", "--quiet", "--no-tags", "--no-write-fetch-head"])
            .arg(&source)
            .arg(format!("+refs/heads/main:{into}")))
    }
}

impl FakeHost {
    fn clone_source(&self, slug: &Slug) -> Result<PathBuf, HostError> {
        self.state()
            .clone_sources
            .get(slug)
            .cloned()
            .ok_or(HostError::NotFound)
    }
}

/// The failure armed for the next `op` in `s`, disarmed.
fn take_armed(s: &mut State, op: FakeOp) -> Option<FakeFail> {
    let at = s.armed.iter().position(|(o, _)| *o == op)?;
    Some(s.armed.remove(at).1)
}

/// Land `incoming` in `assets`: whether any name was new or its bytes differed.
fn lands(
    assets: &mut BTreeMap<String, Vec<u8>>,
    incoming: impl IntoIterator<Item = (String, Vec<u8>)>,
) -> bool {
    let mut changed = false;
    for (name, bytes) in incoming {
        changed |= assets.get(&name) != Some(&bytes);
        assets.insert(name, bytes);
    }
    changed
}

fn git(cmd: &mut aterm_uds::spawnfd::Command) -> Result<(), HostError> {
    let out = cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| HostError::Unreachable(format!("spawn git: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(HostError::Refused(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ))
    }
}
