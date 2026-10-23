// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Fake GitHub state: owners, repositories, releases, assets, files, commits, refs, and faults.

use std::collections::BTreeMap;

use crate::{
    AssetSpec, Change, CommitSpec, CompareSpec, FakeOrigin, Fault, Match, Ports, ReleaseSpec,
    RepoSpec, Seen,
};

/// An owner: a User when some token's login is it, an Organization otherwise.
pub(crate) struct Owner {
    /// The login, as first named.
    pub(crate) login: String,
    /// Its id (from 100).
    pub(crate) id: u64,
}

/// One file `put_file` laid under a reference.
pub(crate) struct File {
    /// The reference it was put under, as given.
    pub(crate) reference: String,
    /// Its path, without a leading `/`.
    pub(crate) path: String,
    /// Its bytes.
    pub(crate) bytes: Vec<u8>,
}

/// What `releases/latest` answers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Latest {
    /// The newest published non-prerelease.
    Auto,
    /// This release (the automatic answer once it is gone).
    Set(u64),
    /// No latest release: a 404.
    None,
}

/// A repository.
pub(crate) struct Repo {
    /// Its id (from 1000).
    pub(crate) id: u64,
    /// The owner's login.
    pub(crate) owner: String,
    /// The name.
    pub(crate) name: String,
    /// Visibility, default branch and logins.
    pub(crate) spec: RepoSpec,
    /// Every file, in the order put.
    pub(crate) files: Vec<File>,
    /// What the tree's `truncated` says.
    pub(crate) tree_truncated: bool,
    /// The history, oldest first.
    pub(crate) commits: Vec<CommitSpec>,
    /// Branch name to sha.
    pub(crate) branches: BTreeMap<String, String>,
    /// Tag name to sha, in the order set.
    pub(crate) tags: Vec<(String, String)>,
    /// `(base, head)` to what the comparison answers.
    pub(crate) compares: Vec<(String, String, CompareSpec)>,
    /// What `releases/latest` answers.
    pub(crate) latest: Latest,
}

impl Repo {
    /// `owner/name`.
    pub(crate) fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// Whether `login` holds push.
    pub(crate) fn is_push(&self, login: Option<&str>) -> bool {
        login.is_some_and(|l| {
            self.spec
                .push_logins
                .iter()
                .any(|p| p.eq_ignore_ascii_case(l))
        })
    }

    /// Whether `login` is named on the repository at all.
    pub(crate) fn is_member(&self, login: Option<&str>) -> bool {
        self.is_push(login)
            || login.is_some_and(|l| {
                self.spec
                    .pull_logins
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(l))
            })
    }

    /// Whether `login` (or anonymous) can see the repository.
    pub(crate) fn visible_to(&self, login: Option<&str>) -> bool {
        !self.spec.private || self.is_member(login)
    }

    /// Resolves a branch or tag name (or the default branch) to its commit SHA.
    pub(crate) fn resolve(&self, reference: &str) -> String {
        if let Some(sha) = self.branches.get(reference) {
            return sha.clone();
        }
        if let Some((_, sha)) = self.tags.iter().find(|(n, _)| n == reference) {
            return sha.clone();
        }
        if reference == self.spec.default_branch
            && let Some(newest) = self.commits.last()
        {
            return newest.sha.clone();
        }
        reference.to_string()
    }

    /// Whether `reference` names something the repository holds.
    pub(crate) fn knows(&self, reference: &str) -> bool {
        let sha = self.resolve(reference);
        self.branches.contains_key(reference)
            || self.tags.iter().any(|(n, _)| n == reference)
            || self.commit_index(reference).is_some()
            || self.files.iter().any(|f| self.resolve(&f.reference) == sha)
    }

    /// The files laid under `reference` (last put wins per path).
    pub(crate) fn files_at(&self, reference: &str) -> Vec<&File> {
        let sha = self.resolve(reference);
        let mut out: Vec<&File> = Vec::new();
        for file in self
            .files
            .iter()
            .filter(|f| self.resolve(&f.reference) == sha)
        {
            match out.iter_mut().find(|f| f.path == file.path) {
                Some(slot) => *slot = file,
                None => out.push(file),
            }
        }
        out
    }

    /// The index of the commit `reference` names.
    pub(crate) fn commit_index(&self, reference: &str) -> Option<usize> {
        let sha = self.resolve(reference);
        self.commits.iter().position(|c| c.sha == sha)
    }
}

/// An asset.
pub(crate) struct Asset {
    /// Its id (from 9000).
    pub(crate) id: u64,
    /// What it holds and says.
    pub(crate) spec: AssetSpec,
    /// When it was first added.
    pub(crate) created_at: String,
    /// CDN downloads so far.
    pub(crate) download_count: u64,
}

/// A release.
pub(crate) struct Release {
    /// Its id (from 5000).
    pub(crate) id: u64,
    /// Its repository's id.
    pub(crate) repo: u64,
    /// What it says.
    pub(crate) spec: ReleaseSpec,
    /// Its assets, in the order added.
    pub(crate) assets: Vec<Asset>,
}

/// A fault waiting for its request.
pub(crate) struct Rule {
    /// Which requests.
    pub(crate) m: Match,
    /// What the fake does instead.
    pub(crate) fault: Fault,
    /// Every matching request, or the next one only.
    pub(crate) always: bool,
}

/// A change waiting for its count.
pub(crate) struct Pending {
    /// Which requests count.
    pub(crate) m: Match,
    /// How many must be answered first.
    pub(crate) count: usize,
    /// How many have been.
    pub(crate) answered: usize,
    /// The change.
    pub(crate) change: Change,
    /// The repository id bound when a `SetLatest` or `Hide` change is registered.
    pub(crate) repo: Option<u64>,
}

/// Everything the fake knows.
pub(crate) struct State {
    /// The four ports (set before the first request can arrive).
    pub(crate) ports: Ports,
    /// Token to login.
    pub(crate) tokens: Vec<(String, String)>,
    /// Every owner, in the order first named.
    pub(crate) owners: Vec<Owner>,
    /// Every repository, in the order added.
    pub(crate) repos: Vec<Repo>,
    /// A renamed repository's old `owner/name`, and its id.
    pub(crate) renames: Vec<(String, u64)>,
    /// Every release, in the order added.
    pub(crate) releases: Vec<Release>,
    /// The faults still armed.
    pub(crate) rules: Vec<Rule>,
    /// The changes still waiting.
    pub(crate) pending: Vec<Pending>,
    /// Requests used per (token, resource); `None` is anonymous.
    pub(crate) used: BTreeMap<(Option<String>, &'static str), u64>,
    /// Requests received so far.
    pub(crate) requests: u64,
    /// Every request, in order.
    pub(crate) seen: Vec<Seen>,
    /// Every breach of the wire rules.
    pub(crate) violations: Vec<String>,
    /// Every [`crate::Fault::Hang`] ends now, held or asked for later ([`crate::FakeGithub::end_hangs`]).
    pub(crate) hangs_ended: bool,
    next_owner: u64,
    next_repo: u64,
    next_release: u64,
    next_asset: u64,
}

/// `owner/name` split; a slug without a `/` is a test's mistake.
fn split_slug(slug: &str) -> (&str, &str) {
    slug.split_once('/')
        .unwrap_or_else(|| panic!("agh-fake: {slug:?} is not owner/name"))
}

/// Whether `m` covers this request.
pub(crate) fn matches(m: &Match, origin: FakeOrigin, method: &str, target: &str) -> bool {
    m.origin == origin
        && m.method
            .as_deref()
            .is_none_or(|x| x.eq_ignore_ascii_case(method))
        && target.starts_with(&m.path_prefix)
}

impl State {
    /// Creates an empty state.
    pub(crate) const fn new() -> State {
        State {
            ports: Ports {
                api: 0,
                uploads: 0,
                web: 0,
                cdn: 0,
            },
            tokens: Vec::new(),
            owners: Vec::new(),
            repos: Vec::new(),
            renames: Vec::new(),
            releases: Vec::new(),
            rules: Vec::new(),
            pending: Vec::new(),
            hangs_ended: false,
            used: BTreeMap::new(),
            requests: 0,
            seen: Vec::new(),
            violations: Vec::new(),
            next_owner: 100,
            next_repo: 1000,
            next_release: 5000,
            next_asset: 9000,
        }
    }

    /// The owner named `login`, added when new.
    fn owner(&mut self, login: &str) -> u64 {
        if let Some(o) = self
            .owners
            .iter()
            .find(|o| o.login.eq_ignore_ascii_case(login))
        {
            return o.id;
        }
        let id = self.next_owner;
        self.next_owner += 1;
        self.owners.push(Owner {
            login: login.to_string(),
            id,
        });
        id
    }

    /// The login `token` is, when the fake knows it.
    pub(crate) fn login_of(&self, token: &str) -> Option<String> {
        self.tokens
            .iter()
            .find(|(t, _)| t == token)
            .map(|(_, l)| l.clone())
    }

    /// Whether `login` is some token's login (a User).
    pub(crate) fn is_user(&self, login: &str) -> bool {
        self.tokens
            .iter()
            .any(|(_, l)| l.eq_ignore_ascii_case(login))
    }

    /// The owner named `login`.
    pub(crate) fn owner_named(&self, login: &str) -> Option<&Owner> {
        self.owners
            .iter()
            .find(|o| o.login.eq_ignore_ascii_case(login))
    }

    /// The repository now named `slug`.
    pub(crate) fn repo_index(&self, slug: &str) -> Option<usize> {
        let (owner, name) = slug.split_once('/')?;
        self.repos
            .iter()
            .position(|r| r.owner.eq_ignore_ascii_case(owner) && r.name.eq_ignore_ascii_case(name))
    }

    /// The repository with `id`.
    pub(crate) fn repo_by_id(&self, id: u64) -> Option<usize> {
        self.repos.iter().position(|r| r.id == id)
    }

    /// The repository now named `slug`; a test naming one it never added is a mistake.
    fn repo_mut(&mut self, slug: &str) -> &mut Repo {
        let index = self
            .repo_index(slug)
            .unwrap_or_else(|| panic!("agh-fake: no repository {slug}"));
        &mut self.repos[index]
    }

    /// The release with `id`.
    pub(crate) fn release_index(&self, id: u64) -> Option<usize> {
        self.releases.iter().position(|r| r.id == id)
    }

    /// See `FakeGithub::add_token`.
    pub(crate) fn add_token(&mut self, token: &str, login: &str) {
        self.owner(login);
        self.tokens.retain(|(t, _)| t != token);
        self.tokens.push((token.to_string(), login.to_string()));
    }

    /// See `FakeGithub::add_repo`.
    pub(crate) fn add_repo(&mut self, slug: &str, spec: RepoSpec) -> u64 {
        let (owner, name) = split_slug(slug);
        self.owner(owner);
        let id = self.next_repo;
        self.next_repo += 1;
        self.repos.push(Repo {
            id,
            owner: owner.to_string(),
            name: name.to_string(),
            spec,
            files: Vec::new(),
            tree_truncated: false,
            commits: Vec::new(),
            branches: BTreeMap::new(),
            tags: Vec::new(),
            compares: Vec::new(),
            latest: Latest::Auto,
        });
        id
    }

    /// See `FakeGithub::rename_repo`.
    pub(crate) fn rename_repo(&mut self, old: &str, new: &str) {
        let (owner, name) = split_slug(new);
        self.owner(owner);
        let repo = self.repo_mut(old);
        repo.owner = owner.to_string();
        repo.name = name.to_string();
        let id = repo.id;
        self.renames.push((old.to_string(), id));
    }

    /// See `FakeGithub::add_release`.
    pub(crate) fn add_release(&mut self, slug: &str, spec: ReleaseSpec) -> u64 {
        let repo = self.repo_mut(slug).id;
        let id = self.next_release;
        self.next_release += 1;
        self.releases.push(Release {
            id,
            repo,
            spec,
            assets: Vec::new(),
        });
        id
    }

    /// See `FakeGithub::add_asset`.
    pub(crate) fn add_asset(&mut self, release_id: u64, spec: AssetSpec) -> u64 {
        let index = self
            .release_index(release_id)
            .unwrap_or_else(|| panic!("agh-fake: no release {release_id}"));
        let release = &mut self.releases[index];
        if let Some(asset) = release.assets.iter_mut().find(|a| a.spec.name == spec.name) {
            asset.spec = spec;
            return asset.id;
        }
        let id = self.next_asset;
        self.next_asset += 1;
        let created_at = spec.updated_at.clone();
        release.assets.push(Asset {
            id,
            spec,
            created_at,
            download_count: 0,
        });
        id
    }

    /// See `FakeGithub::set_latest`.
    pub(crate) fn set_latest(&mut self, slug: &str, release_id: Option<u64>) {
        self.repo_mut(slug).latest = release_id.map_or(Latest::None, Latest::Set);
    }

    /// See `FakeGithub::put_file`.
    pub(crate) fn put_file(&mut self, slug: &str, reference: &str, path: &str, bytes: &[u8]) {
        let path = path.trim_matches('/').to_string();
        let repo = self.repo_mut(slug);
        repo.files
            .retain(|f| !(f.reference == reference && f.path == path));
        repo.files.push(File {
            reference: reference.to_string(),
            path,
            bytes: bytes.to_vec(),
        });
    }

    /// See `FakeGithub::set_tree_truncated`.
    pub(crate) fn set_tree_truncated(&mut self, slug: &str, truncated: bool) {
        self.repo_mut(slug).tree_truncated = truncated;
    }

    /// See `FakeGithub::add_commit`.
    pub(crate) fn add_commit(&mut self, slug: &str, spec: CommitSpec) {
        self.repo_mut(slug).commits.push(spec);
    }

    /// See `FakeGithub::set_branch`.
    pub(crate) fn set_branch(&mut self, slug: &str, name: &str, sha: &str) {
        self.repo_mut(slug)
            .branches
            .insert(name.to_string(), sha.to_string());
    }

    /// See `FakeGithub::set_tag`.
    pub(crate) fn set_tag(&mut self, slug: &str, name: &str, sha: &str) {
        let repo = self.repo_mut(slug);
        repo.tags.retain(|(n, _)| n != name);
        repo.tags.push((name.to_string(), sha.to_string()));
    }

    /// See `FakeGithub::set_compare`.
    pub(crate) fn set_compare(&mut self, slug: &str, base: &str, head: &str, spec: CompareSpec) {
        let repo = self.repo_mut(slug);
        repo.compares.retain(|(b, h, _)| !(b == base && h == head));
        repo.compares
            .push((base.to_string(), head.to_string(), spec));
    }

    /// The newest release of repository `repo` by `(created_at, id)`, drafts included.
    pub(crate) fn newest_release(&self, repo: u64) -> Option<u64> {
        self.releases
            .iter()
            .filter(|r| r.repo == repo)
            .max_by(|a, b| (&a.spec.created_at, a.id).cmp(&(&b.spec.created_at, b.id)))
            .map(|r| r.id)
    }

    /// Apply `change` now.
    pub(crate) fn apply(&mut self, change: Change) {
        if let Some(why) = self.missing(&change) {
            panic!("agh-fake: {why}");
        }
        self.apply_held(change);
    }

    /// What `change` names that the fake does not hold, if anything.
    fn missing(&self, change: &Change) -> Option<String> {
        match change {
            Change::Publish { release_id }
            | Change::AddAsset { release_id, .. }
            | Change::DeleteRelease { release_id } => self
                .release_index(*release_id)
                .is_none()
                .then(|| format!("no release {release_id}")),
            Change::SetLatest { slug, .. } | Change::Hide { slug } => self
                .repo_index(slug)
                .is_none()
                .then(|| format!("no repository {slug}")),
        }
    }

    /// Apply `change`, which `missing` has passed; it never panics.
    fn apply_held(&mut self, change: Change) {
        match change {
            Change::Publish { release_id } => {
                if let Some(index) = self.release_index(release_id) {
                    let spec = &mut self.releases[index].spec;
                    spec.draft = false;
                    spec.published_at = Some(spec.created_at.clone());
                }
            }
            Change::AddAsset { release_id, spec } => {
                if self.release_index(release_id).is_some() {
                    self.add_asset(release_id, spec);
                }
            }
            Change::SetLatest { slug, release_id } => {
                if let Some(index) = self.repo_index(&slug) {
                    self.repos[index].latest = release_id.map_or(Latest::None, Latest::Set);
                }
            }
            Change::DeleteRelease { release_id } => {
                if let Some(index) = self.release_index(release_id) {
                    self.releases.remove(index);
                }
            }
            Change::Hide { slug } => {
                if let Some(index) = self.repo_index(&slug) {
                    let repo = &mut self.repos[index];
                    repo.spec.private = true;
                    repo.spec.push_logins.clear();
                    repo.spec.pull_logins.clear();
                }
            }
        }
    }

    /// See `FakeGithub::after`.
    pub(crate) fn after(&mut self, m: Match, count: usize, change: Change) {
        if let Some(why) = self.missing(&change) {
            panic!("agh-fake: {why}");
        }
        if count == 0 {
            self.apply_held(change);
            return;
        }
        let repo = match &change {
            Change::SetLatest { slug, .. } | Change::Hide { slug } => {
                self.repo_index(slug).map(|i| self.repos[i].id)
            }
            _ => None,
        };
        self.pending.push(Pending {
            m,
            count,
            answered: 0,
            change,
            repo,
        });
    }

    /// A due change with its repository's name as it now is (the repository it named when
    /// registered, whatever it has been renamed to since).
    fn rebind(&self, change: Change, repo: Option<u64>) -> Change {
        let current = repo
            .and_then(|id| self.repo_by_id(id))
            .map(|i| self.repos[i].full_name());
        match (change, current) {
            (Change::SetLatest { release_id, .. }, Some(slug)) => {
                Change::SetLatest { slug, release_id }
            }
            (Change::Hide { .. }, Some(slug)) => Change::Hide { slug },
            (change, _) => change,
        }
    }

    /// The armed fault for this request, disarming a once-only one.
    pub(crate) fn take_fault(
        &mut self,
        origin: FakeOrigin,
        method: &str,
        target: &str,
    ) -> Option<Fault> {
        let hit = |r: &Rule| matches(&r.m, origin, method, target);
        if let Some(index) = self.rules.iter().position(|r| !r.always && hit(r)) {
            return Some(self.rules.remove(index).fault);
        }
        let index = self.rules.iter().position(|r| r.always && hit(r))?;
        Some(self.rules[index].fault.clone())
    }

    /// Count one answered request against every waiting change, applying those now due.
    pub(crate) fn count_answered(&mut self, origin: FakeOrigin, method: &str, target: &str) {
        let mut due = Vec::new();
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &mut self.pending[index];
            if matches(&pending.m, origin, method, target) {
                pending.answered += 1;
                if pending.answered >= pending.count {
                    let pending = self.pending.remove(index);
                    due.push((pending.change, pending.repo));
                    continue;
                }
            }
            index += 1;
        }
        for (change, repo) in due {
            let change = self.rebind(change, repo);
            match self.missing(&change) {
                Some(why) => self.violations.push(format!(
                    "after's change names nothing the fake holds: {why}"
                )),
                None => self.apply_held(change),
            }
        }
    }
}

impl Default for State {
    fn default() -> State {
        State::new()
    }
}
