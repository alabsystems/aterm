// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private local identity of repository inputs. It observes only the parsed GitHub profile,
//! its selected credential and default inventory, so unrelated init settings do not stop work.
//! Readiness observation also tracks the actual admitted Git command and payload.

use super::{Asked, Github, INPUT_MAX, TOKEN_FILE, asked, decode, inventory, profile, read_token};
use crate::model::User;
use crate::port::{OwnedFileMetadata, Stat, System};
use aterm_secrets::converge::{
    RefreshObservation, RefreshRequest, RefreshScope, RefreshVersion, RepositoryRequest,
    RepositoryWork,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

type FileVersion = Result<[u8; 32], std::io::ErrorKind>;
type PrefixFacts = Vec<(PathBuf, Result<Stat, std::io::ErrorKind>)>;

#[derive(Clone, PartialEq, Eq)]
struct PrefixPolicy(u32, u32, u32, bool, bool);

#[derive(Clone, PartialEq, Eq)]
struct PathKey {
    uid: u32,
    home: PathBuf,
    file: Result<OwnedFileMetadata, std::io::ErrorKind>,
    prefix: Vec<(PathBuf, Result<PrefixPolicy, std::io::ErrorKind>)>,
}

struct PathFrame {
    key: PathKey,
    facts: PrefixFacts,
}

impl PathFrame {
    fn read(sys: &dyn System, user: &User, path: &Path, max: usize) -> Self {
        let facts: PrefixFacts = path
            .ancestors()
            .skip(1)
            .map(|path| (path.to_path_buf(), sys.stat(path).map_err(|e| e.kind())))
            .collect();
        let prefix = facts
            .iter()
            .map(|(path, stat)| {
                (
                    path.clone(),
                    stat.map(|s| PrefixPolicy(s.uid, s.gid, s.mode, s.dir, s.symlink)),
                )
            })
            .collect();
        let file = sys
            .owned_file_metadata(path, user.uid, max, false)
            .map_err(|e| e.kind());
        Self {
            key: PathKey {
                uid: user.uid,
                home: PathBuf::from(&user.home),
                file,
                prefix,
            },
            facts,
        }
    }

    fn settled(&self, now: u64) -> bool {
        self.key.prefix.iter().all(|(_, fact)| fact.is_ok())
            && match self.key.file {
                Ok(metadata) => metadata.settled_at(now),
                Err(std::io::ErrorKind::NotFound) => true,
                Err(_) => false,
            }
    }

    fn accepts(
        &self,
        read: &std::io::Result<crate::port::OwnedFileRead>,
        after: &Self,
        started: u64,
    ) -> bool {
        let descriptor_matches = match (&self.key.file, read) {
            (Ok(expected), Ok(read)) => *expected == read.metadata,
            (Err(std::io::ErrorKind::NotFound), Err(error)) => {
                error.kind() == std::io::ErrorKind::NotFound
            }
            _ => false,
        };
        self.key == after.key && descriptor_matches && self.settled(started)
    }
}

/// Per-observer memo of declared locations alone. No credential bytes, executable
/// readiness, source authority or permission to use a file is cached.
#[derive(Default)]
pub struct RepositoryInputPathCache {
    profile: Option<(PathKey, Vec<PathBuf>)>,
    command: Option<(PathKey, Vec<PathBuf>)>,
}

/// Declared paths and the fresh prefix facts already read while deriving them.
pub struct RepositoryControlPaths {
    pub paths: Vec<PathBuf>,
    pub prefix_facts: PrefixFacts,
    /// Only coherent settled declarations permit the observer's metadata fast path.
    pub reusable: bool,
}

impl std::fmt::Debug for RepositoryInputPathCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryInputPathCache")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for RepositoryControlPaths {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryControlPaths")
            .finish_non_exhaustive()
    }
}

impl RepositoryInputPathCache {
    /// Observe current paths, resolving Git and command links every time. Only
    /// unchanged settled declarations avoid another owned content read.
    /// # Errors
    /// This process's own account cannot be identified.
    pub fn observe(&mut self, sys: &dyn System) -> Result<RepositoryControlPaths, String> {
        let user = account(sys)?;
        let path = Path::new(&user.home).join(".config/aterm/init.toml");
        let started = sys.now();
        let before = PathFrame::read(sys, &user, &path, INPUT_MAX);
        let (selected, reusable) = if let Some((_, selected)) = self
            .profile
            .as_ref()
            .filter(|(key, _)| *key == before.key && before.settled(started))
        {
            (selected.clone(), true)
        } else {
            let read = sys.read_owned_file_observed(&path, user.uid, INPUT_MAX, false);
            let selected = match &read {
                Ok(read) => selected_paths(decode(&read.bytes).map(Some), &user),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // A second read here could see a transient profile and memoize its
                    // selection under two surrounding Missing observations (ABA).
                    selected_paths(Ok(None), &user)
                }
                // Unsupported, busy or changing observations keep ordinary profile
                // reading behavior. They cannot populate the memo or skip a retry.
                Err(_) => selected_paths(profile(sys, &user), &user),
            };
            let after = PathFrame::read(sys, &user, &path, INPUT_MAX);
            let reusable = before.accepts(&read, &after, started);
            self.profile = reusable.then(|| (before.key.clone(), selected.clone()));
            (selected, reusable)
        };
        let (commands, command_facts, command_reusable) = self.commands(sys, &user, started);
        let mut paths = dynamic_paths(sys, &user, &commands);
        paths.extend(selected);
        paths.sort();
        paths.dedup();
        Ok(RepositoryControlPaths {
            paths,
            prefix_facts: before.facts.into_iter().chain(command_facts).collect(),
            reusable: reusable && command_reusable,
        })
    }

    fn commands(
        &mut self,
        sys: &dyn System,
        user: &User,
        started: u64,
    ) -> (Vec<PathBuf>, PrefixFacts, bool) {
        use crate::git_credential::Helper;
        use aterm_types::installed_command as command;
        let path = command::path(Path::new(&user.home));
        let before = PathFrame::read(sys, user, &path, command::MAX_BYTES);
        if let Some((_, locations)) = self
            .command
            .as_ref()
            .filter(|(key, _)| *key == before.key && before.settled(started))
        {
            return (locations.clone(), before.facts, true);
        }
        let read = sys.read_owned_file_observed(&path, user.uid, command::MAX_BYTES, false);
        let locations = if read
            .as_ref()
            .is_err_and(|why| why.kind() == std::io::ErrorKind::Unsupported)
        {
            Helper::locations(sys, user)
        } else {
            Helper::locations_from(
                user,
                read.as_ref()
                    .map(|read| read.bytes.as_slice())
                    .map_err(std::io::Error::kind),
            )
        }
        .unwrap_or_default();
        let after = PathFrame::read(sys, user, &path, command::MAX_BYTES);
        let reusable = before.accepts(&read, &after, started);
        self.command = reusable.then(|| (before.key.clone(), locations.clone()));
        (locations, before.facts, reusable)
    }
}

/// Local source, pinned-helper and Git changes that can invalidate a completed repository stage.
/// Observing this mark grants no authority over inputs or effects and runs no program.
#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryInputMark {
    source: Result<Stamp, String>,
    /// Refresh eligibility only; never part of the credential/source authority stamp.
    develop: Result<BTreeSet<PathBuf>, String>,
    helper: Result<(PathBuf, PathBuf, [u8; 32]), std::io::ErrorKind>,
    git: Vec<(PathBuf, FileVersion)>,
    paths: Vec<PathBuf>,
    checkouts: super::checkout_inputs::Checkouts,
    refresh: Result<aterm_secrets::converge::RefreshScope, String>,
}

/// An automatic producer's last actual source and observed input epoch floor.
/// These are scheduling facts alone, never permission to read or change a repository.
pub struct RepositoryRefreshCursor {
    scope: Option<RefreshScope>,
    basis: RefreshVersion,
    first: bool,
}

impl RepositoryRefreshCursor {
    /// `initial` must be sampled before reading the initial actual input mark.
    #[must_use]
    pub fn new(initial: RefreshObservation, mark: Option<&RepositoryInputMark>) -> Self {
        Self {
            scope: mark.and_then(RepositoryInputMark::refresh_scope),
            basis: initial.version,
            first: true,
        }
    }

    /// `mark` must come from this round's successful actual input observation.
    /// A transition binds its old epoch before the new sample raises the future floor;
    /// unchanged actual inputs also request repair when a newer foreign generation reveals
    /// a missed transition. Receipt epochs grant no source or effect authority.
    pub fn observe(
        &mut self,
        mark: Option<&RepositoryInputMark>,
        receipt: RefreshObservation,
        work: Option<RepositoryWork>,
    ) -> Option<RepositoryRequest> {
        let scope = mark.and_then(RepositoryInputMark::refresh_scope);
        let first = std::mem::take(&mut self.first);
        let missed = scope.is_some()
            && scope == self.scope
            && receipt.scope != scope
            && receipt.version.input > self.basis.input;
        let request = if work == Some(RepositoryWork::Refresh) || self.scope != scope || missed {
            Some(RepositoryRequest::Refresh(RefreshRequest {
                scope,
                base_input: if missed {
                    receipt.version.input
                } else {
                    self.basis.input
                },
                changed: true,
                after_attempt: None,
            }))
        } else if work == Some(RepositoryWork::Repair) {
            Some(RepositoryRequest::Repair)
        } else if first {
            scope.map(|scope| {
                RepositoryRequest::Refresh(RefreshRequest::ensure(Some(scope), receipt.version))
            })
        } else {
            None
        };
        self.scope = scope;
        if scope.is_some() {
            self.basis.input = self.basis.input.max(receipt.version.input);
        }
        request
    }
}

impl RepositoryInputMark {
    /// Whether only checkout facts could differ. These source, helper and Git
    /// inputs determine when a refresh is requested; the request grants no authority.
    #[must_use]
    pub fn same_source(&self, other: &Self) -> bool {
        self.source == other.source
            && self.develop == other.develop
            && self.helper == other.helper
            && self.git == other.git
            && self.paths == other.paths
            && self.refresh == other.refresh
    }

    /// Opaque scheduling scope, never permission to use its inputs. It excludes
    /// every checkout field so a refresh cannot request itself through its own fetch.
    pub fn refresh_scope(&self) -> Option<aterm_secrets::converge::RefreshScope> {
        self.refresh.as_ref().ok().copied()
    }

    /// Resample the previously derived private-inventory scope using metadata alone.
    /// This reads no private inventory, credential or worktree content and starts no program.
    #[must_use]
    pub fn checkout_metadata(&self, sys: &dyn System) -> super::RepositoryCheckoutMetadata {
        self.checkouts.observe(sys)
    }

    /// Metadata captured with this mark, before any later filesystem changes.
    #[must_use]
    pub fn captured_checkout_metadata(&self) -> super::RepositoryCheckoutMetadata {
        self.checkouts.metadata()
    }
}

impl std::fmt::Debug for RepositoryInputMark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RepositoryInputMark")
            .finish_non_exhaustive()
    }
}

fn account(sys: &dyn System) -> Result<User, String> {
    let user = sys.identity()?.user;
    if user.uid != sys.euid() || user.uid == 0 {
        return Err("repository inputs belong to this login alone".into());
    }
    Ok(user)
}

/// Observe this login's selected repository inputs, installed credential helper and admitted Git
/// command/payload. Missing or refused inputs remain stable observations, so removal and
/// later repair each move it.
///
/// # Errors
/// This process's own account cannot be identified.
pub fn repository_input_mark(sys: &dyn System) -> Result<RepositoryInputMark, String> {
    let user = account(sys)?;
    let source = Stamp::read(sys, &user);
    let refresh = source
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|stamp| refresh_scope(sys, &user, stamp));
    let helper = (|| {
        let (program, payload) = crate::git_credential::Helper::target(sys, &user)?;
        let version = sys.file_version(&payload)?;
        Ok::<_, std::io::Error>((program, payload, version))
    })()
    .map_err(|why| why.kind());
    Ok(RepositoryInputMark {
        source,
        develop: super::develop_checkouts(sys, &user),
        helper,
        git: git_paths(sys)
            .into_iter()
            .map(|path| {
                let version = sys.file_version(&path).map_err(|why| why.kind());
                (path, version)
            })
            .collect(),
        paths: watch_paths(sys, &user),
        checkouts: super::checkout_inputs::Checkouts::capture(sys, &user),
        refresh,
    })
}

/// Files whose parent directories can notify a parked repository stage. Its own selected
/// token is included; global credential helpers are excluded because every setup Git command
/// pins aterm's helper instead. The admitted Git command and payload are included, so a
/// same-build shim repair is observable. A malformed profile still watches its repair.
///
/// # Errors
/// This process's own account cannot be identified.
pub fn repository_input_watch_paths(sys: &dyn System) -> Result<Vec<PathBuf>, String> {
    let user = account(sys)?;
    let mut paths = watch_paths(sys, &user);
    paths.extend(super::checkout_inputs::Checkouts::capture(sys, &user).paths());
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Cheap control paths for metadata polling. Inventory-derived checkout paths are cached
/// separately in [`RepositoryInputMark`], rather than reparsing private inputs each tick.
///
/// # Errors
/// This process's own account cannot be identified.
pub fn repository_input_control_watch_paths(sys: &dyn System) -> Result<Vec<PathBuf>, String> {
    let user = account(sys)?;
    Ok(watch_paths(sys, &user))
}

/// Declared checkout and Git control boundaries from the selected private inventory.
/// Callers must retain no-follow admission when arming their existing filesystem watches.
///
/// # Errors
/// This process's own account cannot be identified.
pub fn repository_checkout_watch_paths(sys: &dyn System) -> Result<Vec<PathBuf>, String> {
    let user = account(sys)?;
    Ok(super::checkout_inputs::Checkouts::capture(sys, &user).paths())
}

fn watch_paths(sys: &dyn System, user: &User) -> Vec<PathBuf> {
    let commands = crate::git_credential::Helper::locations(sys, user).unwrap_or_default();
    let mut paths = dynamic_paths(sys, user, &commands);
    paths.extend(selected_paths(profile(sys, user), user));
    paths.sort();
    paths.dedup();
    paths
}

fn dynamic_paths(sys: &dyn System, user: &User, commands: &[PathBuf]) -> Vec<PathBuf> {
    let home = Path::new(&user.home);
    let mut paths = vec![
        home.join(".config/aterm/init.toml"),
        home.join(".config/aterm/secrets/source.toml"),
        aterm_types::installed_command::path(home),
    ];
    // A launcher can precede its installed target. Keep each declared target observable even
    // when canonicalization cannot reach it yet, including a dangling link in a parent. This
    // reads link metadata only and grants no permission to execute or read the target.
    for command in commands {
        if let Ok(resolved) = sys.canonicalize(command) {
            paths.push(resolved);
        }
        let mut link = command.clone();
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            if !seen.insert(link.clone()) {
                break;
            }
            paths.push(link.clone());
            let next = link.ancestors().find_map(|ancestor| {
                let target = sys.read_link(ancestor).ok()?;
                let target = if target.is_absolute() {
                    target
                } else {
                    ancestor.parent()?.join(target)
                };
                let tail = link.strip_prefix(ancestor).ok()?;
                Some(if tail.as_os_str().is_empty() {
                    target
                } else {
                    target.join(tail)
                })
            });
            let Some(next) = next else { break };
            link = next;
        }
    }
    paths.extend(git_paths(sys));
    paths
}

fn selected_paths(profile: Result<Option<super::Config>, String>, user: &User) -> Vec<PathBuf> {
    let home = Path::new(&user.home);
    let mut paths = Vec::new();
    match profile.and_then(|profile| asked(profile.and_then(|p| p.github), user)) {
        Ok(Asked::Default) => {
            paths.push(home.join(TOKEN_FILE));
            paths.push(inventory::file(&user.home));
        }
        Ok(Asked::Scoped(source) | Asked::Profile(source)) => paths.push(source.token_file),
        Ok(Asked::Off) | Err(_) => {}
    }
    paths
}

fn git_paths(sys: &dyn System) -> Vec<PathBuf> {
    let Some(git) = crate::items::github::installed_git_execution(sys) else {
        return Vec::new();
    };
    let mut paths = vec![git.program, git.payload];
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_profile_memo_requires_the_read_descriptor_and_every_prefix_to_match() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().canonicalize().unwrap();
        let parent = home.join(".config/aterm");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.join("init.toml");
        let other = parent.join("other-profile");
        for (path, bytes) in [
            (&path, b"[develop]\ncheckouts=[]\n".as_slice()),
            (&other, b"# other owned version\n".as_slice()),
        ] {
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let user = User {
            name: "fixture".into(),
            uid: path.metadata().unwrap().uid(),
            gid: 0,
            home: home.to_string_lossy().into_owned(),
            shell: String::new(),
            admin: false,
        };
        let sys = crate::port::Live::new();
        let before = PathFrame::read(&sys, &user, &path, INPUT_MAX);
        let read = sys.read_owned_file_observed(&path, user.uid, INPUT_MAX, false);
        let after = PathFrame::read(&sys, &user, &path, INPUT_MAX);
        let settled = sys.now().saturating_add(3);
        assert!(
            before.accepts(&read, &after, settled),
            "valid settled descriptor control"
        );
        assert!(
            !before.accepts(&read, &after, 0),
            "unsettled data never populate the memo"
        );
        // A different owned descriptor cannot borrow the pathname's later metadata,
        // including when a namespace changes away and back between path observations.
        let substituted = sys.read_owned_file_observed(&other, user.uid, INPUT_MAX, false);
        assert!(!before.accepts(&substituted, &after, settled));
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o750)).unwrap();
        let changed = PathFrame::read(&sys, &user, &path, INPUT_MAX);
        assert!(
            !before.accepts(&read, &changed, settled),
            "prefix admission is part of the key"
        );
    }

    #[test]
    fn a_dangling_helper_keeps_its_declared_target_observable() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let home = root_path.join("home");
        let alias = root_path.join("alias/aterm");
        let user = User {
            name: "fixture".into(),
            uid: aterm_uds::peer::our_uid(),
            gid: 0,
            home: home.to_string_lossy().into_owned(),
            shell: String::new(),
            admin: false,
        };
        let [launcher, store] = aterm_types::installed_command::legacy(Path::new(&user.home));
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&alias, &launcher).unwrap();
        let sys = crate::port::Live::new();
        for package in ["first", "second"] {
            if package == "second" {
                std::fs::remove_file(&alias).unwrap();
            }
            let relative = PathBuf::from(format!("../{package}/bin/aterm"));
            std::os::unix::fs::symlink(&relative, &alias).unwrap();
            assert!(sys.canonicalize(&launcher).is_err(), "the target is absent");
            let paths = watch_paths(&sys, &user);
            assert!(paths.contains(&launcher));
            assert!(
                paths.contains(&store),
                "both absent-declaration locations are watched"
            );
            assert!(paths.contains(&alias));
            assert!(
                paths.contains(&alias.parent().unwrap().join(relative)),
                "arrival away from the unchanged launcher must stay observable: {paths:?}"
            );
        }
        // A missing directory behind a parent link has the same arrival boundary.
        std::fs::remove_file(&alias).unwrap();
        std::fs::remove_file(&launcher).unwrap();
        std::fs::remove_dir(launcher.parent().unwrap()).unwrap();
        let package = root_path.join("third/bin");
        std::os::unix::fs::symlink(&package, launcher.parent().unwrap()).unwrap();
        assert!(sys.canonicalize(&launcher).is_err());
        assert!(watch_paths(&sys, &user).contains(&package.join("aterm")));

        use aterm_types::installed_command as command;
        use std::os::unix::fs::PermissionsExt as _;
        let declaration = command::path(&home);
        std::fs::create_dir_all(declaration.parent().unwrap()).unwrap();
        let custom = root_path.join("custom bin/aterm");
        let payload = root_path.join("custom store/aterm");
        std::fs::create_dir_all(custom.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&payload, &custom).unwrap();
        let select = |target: &Path| {
            std::fs::write(
                &declaration,
                command::Selection::new(target.to_path_buf())
                    .unwrap()
                    .encode()
                    .unwrap(),
            )
            .unwrap();
            std::fs::set_permissions(&declaration, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        select(&custom);
        let paths = watch_paths(&sys, &user);
        assert!(paths.contains(&declaration));
        assert!(paths.contains(&custom));
        assert!(
            paths.contains(&payload),
            "a custom dangling target can arrive"
        );
        assert!(
            !paths.contains(&launcher),
            "a declaration retires legacy locations"
        );
        let replacement = root_path.join("replacement store/aterm");
        select(&replacement);
        let paths = watch_paths(&sys, &user);
        assert!(paths.contains(&replacement));
        assert!(!paths.contains(&custom));
        assert!(!paths.contains(&payload), "the old selection retires");
        std::fs::write(&declaration, b"not a declaration").unwrap();
        let paths = watch_paths(&sys, &user);
        assert!(paths.contains(&declaration));
        assert!(!paths.contains(&launcher), "malformed does not fall back");
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Stamp([u8; 32]);

/// Hash only scheduling identity from an already captured credential-source
/// stamp. No credential bytes, vault authority or checkout state are retained.
pub(super) fn refresh_scope(
    sys: &dyn System,
    user: &User,
    source: &Stamp,
) -> Result<aterm_secrets::converge::RefreshScope, String> {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm repository refresh scope/1\0");
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(&source.0);
    part(&user.uid.to_le_bytes());
    part(user.home.as_bytes());
    for path in super::develop_checkouts(sys, user)? {
        part(path.as_os_str().as_encoded_bytes());
    }
    let paths = watch_paths(sys, user);
    // Include declared vault identity/source, independently of the delivered
    // credential bytes. TOML serialization ignores comment/format-only edits.
    let selected = Path::new(&user.home).join(".config/aterm/secrets/source.toml");
    match sys.read_owned_file(&selected, user.uid, 4 << 20, true) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|_| "vault source is not UTF-8")?;
            let table: aterm_toml::Value =
                aterm_toml::from_str(text).map_err(|_| "vault source cannot be decoded")?;
            part(
                aterm_toml::to_string(&table)
                    .map_err(|_| "vault source cannot be observed")?
                    .as_bytes(),
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => part(b"no configured vault"),
        Err(_) => return Err("vault source cannot be safely observed".into()),
    }
    for path in paths {
        part(path.as_os_str().as_encoded_bytes());
        // The profile's unrelated settings do not change refresh scope.
        if !path.ends_with("init.toml") && path != selected {
            part(format!("{:?}", sys.file_version(&path).map_err(|e| e.kind())).as_bytes());
        }
        for parent in path.ancestors().skip(1) {
            let policy = sys
                .stat(parent)
                .map(|s| (s.uid, s.gid, s.mode, s.dir, s.symlink))
                .map_err(|e| e.kind());
            part(parent.as_os_str().as_encoded_bytes());
            part(format!("{policy:?}").as_bytes());
        }
    }
    Ok(aterm_secrets::converge::RefreshScope::new(hash.finalize()))
}

impl Stamp {
    pub(super) fn matches_config(
        sys: &dyn System,
        user: &User,
        expected: &Github,
    ) -> Result<bool, String> {
        Ok(
            match asked(profile(sys, user)?.and_then(|profile| profile.github), user)? {
                Asked::Scoped(current) | Asked::Profile(current) => current == *expected,
                Asked::Default | Asked::Off => false,
            },
        )
    }

    pub(super) fn read(sys: &dyn System, user: &User) -> Result<Self, String> {
        let asked = asked(profile(sys, user)?.and_then(|profile| profile.github), user)?;
        let (kind, source, default) = match asked {
            Asked::Off => return Err("repository setup is disabled in init.toml".into()),
            Asked::Default => (
                b"default".as_slice(),
                Github {
                    account: String::new(),
                    owner: String::new(),
                    token_file: Path::new(&user.home).join(TOKEN_FILE),
                    token_key: None,
                },
                true,
            ),
            Asked::Scoped(source) => (b"scoped".as_slice(), source, false),
            Asked::Profile(source) => (b"profile".as_slice(), source, false),
        };
        let token = read_token(sys, user, &source).map_err(|why| why.why)?;
        let inventory = if default {
            sys.read_owned_file(
                &inventory::file(&user.home),
                user.uid,
                inventory::MAX_BYTES,
                true,
            )
            .map_err(|_| "the private repository inventory is not ready".to_owned())?
        } else {
            Vec::new()
        };
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm repository inputs/1\0");
        let account = source.account.to_ascii_lowercase();
        let owner = source.owner.to_ascii_lowercase();
        for bytes in [
            kind,
            account.as_bytes(),
            owner.as_bytes(),
            source.token_file.as_os_str().as_encoded_bytes(),
            source.token_key.as_deref().unwrap_or("").as_bytes(),
            token.bytes(),
            inventory.as_slice(),
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok(Self(digest.finalize()))
    }

    pub(super) fn is_current(&self, sys: &dyn System, user: &User) -> Result<bool, String> {
        Self::read(sys, user).map(|now| now == *self)
    }
}
