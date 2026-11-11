// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable-owned readiness observation for automatic converge jobs.
//! Reuse the existing one-second lane; metadata gates private token/inventory reads, and the
//! checkout scope is read once a minute ([`CHECKOUT_LOOK`]).

use aterm_machines::init_github::{
    RepositoryCheckoutMetadata, RepositoryInputMark, RepositoryInputPathCache,
    RepositoryRefreshCursor, repository_input_mark,
};
use aterm_machines::port::{Live, System};
use aterm_secrets::client::Paths;
use aterm_secrets::converge::RepositoryWork;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How often a look reads the checkout scope (each selected checkout's Git metadata and every
/// folder above it): the most paths a look reads and most of its cost, and a watch on them would
/// hold a descriptor for each (kqueue). Every look still reads the rest of the inputs, and a
/// change to them captures the checkouts afresh with the whole mark.
const CHECKOUT_LOOK: Duration = Duration::from_secs(60);

type Version = Result<[u8; 32], io::ErrorKind>;
type Admission = Vec<(PathBuf, Result<Policy, io::ErrorKind>)>;

/// Path admission can change while every leaf inode stays fixed. Directory timestamps,
/// sizes and inode replacements with the same policy do not invalidate local readiness.
#[derive(Clone, PartialEq, Eq)]
struct Policy {
    uid: u32,
    gid: u32,
    mode: u32,
    dir: bool,
    symlink: bool,
}

#[derive(PartialEq, Eq)]
struct Metadata {
    files: Vec<(PathBuf, Version)>,
    admission: Admission,
    checkouts: Option<RepositoryCheckoutMetadata>,
}

impl Metadata {
    fn read(
        sys: &dyn System,
        paths: &Paths,
        previous: Option<&RepositoryInputMark>,
        control: &mut RepositoryInputPathCache,
    ) -> Result<(Self, bool), String> {
        let observed = control.observe(sys)?;
        let prefix: BTreeMap<_, _> = observed.prefix_facts.into_iter().collect();
        let files: Vec<_> = observed
            .paths
            .into_iter()
            .map(|path| {
                let version = sys.file_version(&path).map_err(|why| why.kind());
                (path, version)
            })
            .collect();
        let directories: BTreeSet<&std::path::Path> = files
            .iter()
            .flat_map(|(path, _)| path.ancestors().skip(1))
            // Complete materialization also requires the supplied vault's private state
            // root. It is an actual admission input, independent of credential leaf paths.
            .chain(paths.dir.ancestors())
            .collect();
        let admission = directories
            .into_iter()
            .map(|path| {
                let policy = prefix
                    .get(path)
                    .copied()
                    .unwrap_or_else(|| sys.stat(path).map_err(|why| why.kind()))
                    .map(|stat| Policy {
                        uid: stat.uid,
                        gid: stat.gid,
                        mode: stat.mode,
                        dir: stat.dir,
                        symlink: stat.symlink,
                    });
                (path.to_path_buf(), policy)
            })
            .collect();
        Ok((
            Self {
                files,
                admission,
                checkouts: previous.map(|mark| mark.checkout_metadata(sys)),
            },
            observed.reusable,
        ))
    }
}

struct Inputs {
    paths: Paths,
    metadata: Option<Metadata>,
    seen: Option<RepositoryInputMark>,
    held: bool,
    control: RepositoryInputPathCache,
    /// When a look last read the checkout scope, by itself or with a whole mark.
    checkouts_read: Option<Instant>,
}

impl Inputs {
    fn new(sys: &dyn System, paths: &Paths, allowed: bool, now: Instant) -> Self {
        let mut inputs = Self {
            paths: paths.clone(),
            metadata: None,
            seen: None,
            held: !allowed,
            control: RepositoryInputPathCache::default(),
            checkouts_read: None,
        };
        if allowed {
            let _ = inputs.changed(sys, true, now);
        }
        inputs
    }

    /// The work the inputs at `now` call for. A look within [`CHECKOUT_LOOK`] of the last read
    /// of the checkout scope compares the rest of the inputs alone; when they changed, are
    /// unsettled or were held, it captures the whole mark, the checkout scope with it.
    fn changed(
        &mut self,
        sys: &dyn System,
        allowed: bool,
        now: Instant,
    ) -> Result<Option<RepositoryWork>, ()> {
        if !allowed {
            self.held = true;
            return Ok(None);
        }
        let checkouts = self
            .checkouts_read
            .is_none_or(|at| now.saturating_duration_since(at) >= CHECKOUT_LOOK);
        let Ok((mut metadata, reusable)) = Metadata::read(
            sys,
            &self.paths,
            self.seen.as_ref().filter(|_| checkouts),
            &mut self.control,
        ) else {
            return Err(());
        };
        // A scope this look read is compared; one it left unread is not.
        let unchanged = self.metadata.as_ref().is_some_and(|before| {
            before.files == metadata.files
                && before.admission == metadata.admission
                && metadata
                    .checkouts
                    .as_ref()
                    .is_none_or(|read| before.checkouts.as_ref() == Some(read))
        });
        if reusable && !self.held && unchanged {
            if checkouts {
                self.checkouts_read = Some(now);
            }
            return Ok(None);
        }
        let Ok(mark) = repository_input_mark(sys) else {
            return Err(());
        };
        let work = if self.held
            || self
                .seen
                .as_ref()
                .is_none_or(|before| !before.same_source(&mark))
            || self
                .metadata
                .as_ref()
                .is_none_or(|prev| prev.admission != metadata.admission)
        {
            Some(RepositoryWork::Refresh)
        } else if self.seen.as_ref() != Some(&mark) {
            Some(RepositoryWork::Repair)
        } else {
            None
        };
        // Retain the state captured with the semantic scope. Resampling after capture could
        // cache a later repair next to an earlier mark and swallow that repair on the next tick.
        metadata.checkouts = Some(mark.captured_checkout_metadata());
        self.checkouts_read = Some(now);
        self.metadata = Some(metadata);
        self.seen = Some(mark);
        self.held = false;
        Ok(work)
    }

    fn request(
        &mut self,
        sys: &dyn System,
        allowed: bool,
        observed: aterm_secrets::converge::RefreshObservation,
        cursor: &mut RepositoryRefreshCursor,
        now: Instant,
    ) -> Option<aterm_secrets::converge::RepositoryRequest> {
        let work = self.changed(sys, allowed, now).ok()?;
        if allowed {
            cursor.observe(self.seen.as_ref(), observed, work)
        } else {
            None
        }
    }
}

/// This observer follows only the runtime's own real account. The ordinary job still reads
/// current source, materialization, Stop/off and path admission before every effect.
pub(super) fn observe(
    paths: &aterm_secrets::client::Paths,
    _role: aterm_secrets::client::ConvergenceRole,
) -> aterm_secrets::client::ConvergenceInputs {
    let sys = Live::new();
    if !sys.identity().is_ok_and(|identity| {
        identity.user.uid == sys.euid()
            && sys.euid() != 0
            && std::path::Path::new(&identity.user.home) == paths.home
    }) {
        return aterm_secrets::client::ConvergenceInputs::new(Box::new(|_| None));
    }
    let home = paths.home.clone();
    let allowed = move || {
        aterm_secrets::swarm::enabled_in_home(&home).unwrap_or(false)
            && !super::converge::setup_holds()
                .holds(aterm_types::macos_setup::Setting::Repositories)
    };
    let state = aterm_types::dirs::account_state_dir();
    let mut receipt = aterm_machines::sync::repository_refresh::Observer::default();
    // Establish the generation BEFORE the first source capture. Another owner may
    // fulfill the next source before this actor gets its first callback.
    let initial = state
        .as_ref()
        .map(|state| receipt.read(&sys, state, aterm_time::unix_millis().unwrap_or(0)))
        .unwrap_or_default();
    let mut inputs = Inputs::new(&sys, paths, allowed(), Instant::now());
    let mut cursor = RepositoryRefreshCursor::new(initial, inputs.seen.as_ref());
    aterm_secrets::client::ConvergenceInputs::new(Box::new(move |observed| {
        inputs.request(&sys, allowed(), observed, &mut cursor, Instant::now())
    }))
    .with_refresh(Box::new(move || {
        state
            .as_ref()
            .map(|state| receipt.read(&Live::new(), state, aterm_time::unix_millis().unwrap_or(0)))
            .unwrap_or_default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_machines::init_github::repository_input_watch_paths;
    use aterm_machines::model::User;
    use aterm_machines::port::{Fake, Stat};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn repairing_the_executed_git_shim_wakes_the_same_admitted_build() {
        // Every look here falls within one minute of the first ([`CHECKOUT_LOOK`]).
        let now = Instant::now();
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let user = User {
            name: "fixture".into(),
            uid: aterm_uds::peer::our_uid(),
            gid: 20,
            home: home.to_string_lossy().into_owned(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(&user);
        let prefix = home.join("Library/Application Support/aterm/pkg");
        let store = prefix.join("store/git");
        let build = store.join("17");
        let payload = build.join("bin/git");
        let shim = prefix.join("bin/git");
        std::fs::create_dir_all(payload.parent().unwrap()).unwrap();
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        for (path, bytes) in [
            (
                &payload,
                b"#!/bin/sh\nprintf 'fixture git ready\\n'\n".as_slice(),
            ),
            (&shim, b"#!/bin/sh\nexit 1\n".as_slice()),
        ] {
            std::fs::write(path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            sys.file(path.to_str().unwrap(), bytes)
                .stat_of(path.to_str().unwrap(), Live::new().stat(path).unwrap());
        }
        sys.file(store.join("17.ready").to_str().unwrap(), b"ready");
        sys.link(store.join("current").to_str().unwrap(), "17");
        sys.runtime_ready(&build);
        let private = [
            (
                home.join(".secrets/gh_access_token"),
                b"inert_fixture_token".as_slice(),
            ),
            (
                home.join(".secrets/github-repositories.json"),
                br#"{"schema":2,"account":"fixture","repositories":[]}"#.as_slice(),
            ),
        ];
        for (path, bytes) in &private {
            sys.file(path.to_str().unwrap(), bytes).stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: user.uid,
                    gid: user.gid,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: bytes.len() as u64,
                },
            );
        }
        let private_versions = private
            .each_ref()
            .map(|(path, _)| sys.file_version(path).unwrap());
        let paths = Paths::locate_for_home(&home);
        let mut inputs = Inputs::new(&sys, &paths, true, now);
        let installed =
            aterm_machines::items::github::installed_git_for(&sys, Some(&home)).unwrap();
        assert_eq!(installed, shim, "the actual command uses the managed shim");
        assert!(
            !aterm_uds::spawnfd::command(&installed)
                .output()
                .unwrap()
                .status
                .success()
        );
        let identity = aterm_machines::items::github::installed_git_identity(&sys);
        let payload_version = sys.file_version(&payload).unwrap();
        let before = repository_input_mark(&sys).unwrap();
        let watched = repository_input_watch_paths(&sys).unwrap();
        assert!(watched.contains(&shim) && watched.contains(&payload));

        // The package repair producer restores the execution route without reinstalling
        // the already admitted payload, touching private inputs, or changing its selection.
        atpkg::platform::install_shim_to(&shim, &payload).unwrap();
        let repaired = std::fs::read(&shim).unwrap();
        sys.file(shim.to_str().unwrap(), &repaired)
            .stat_of(shim.to_str().unwrap(), Live::new().stat(&shim).unwrap());
        assert!(
            aterm_uds::spawnfd::command(&installed)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(
            aterm_machines::items::github::installed_git_identity(&sys),
            identity
        );
        assert_eq!(sys.file_version(&payload).unwrap(), payload_version);
        assert_eq!(
            private
                .each_ref()
                .map(|(path, _)| sys.file_version(path).unwrap()),
            private_versions
        );
        assert!(
            before != repository_input_mark(&sys).unwrap(),
            "parked setup sees the same repair"
        );
        assert!(
            inputs.changed(&sys, true, now).unwrap() == Some(RepositoryWork::Refresh),
            "the repaired execution route is new readiness"
        );
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "one wake for the same repair"
        );
    }

    #[test]
    fn repaired_materialization_parent_wakes_without_changing_the_credential_inode() {
        // Every look here falls within one minute of the first ([`CHECKOUT_LOOK`]).
        let now = Instant::now();
        use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let source = scratch.path().join("vault");
        let inventory = br#"{"schema":2,"account":"fixture","repositories":[]}"#;
        std::fs::write(
            &source,
            format!(
                "gh_access_token=inert_fixture_token\ngithub-repositories.json={}\n",
                std::str::from_utf8(inventory).unwrap()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let paths = Paths::for_home(&home);
        client::join(
            &paths,
            &JoinOptions {
                source: source.to_string_lossy().into_owned(),
                host: Some("fixture".into()),
                interactive: false,
                wait: std::time::Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        let parent = paths.dir.clone();
        let token = home.join(".secrets/gh_access_token");
        let before = std::fs::metadata(&token).unwrap();
        let held = scratch.path().join("held-secrets");
        std::fs::rename(&parent, &held).unwrap();
        std::os::unix::fs::symlink(&held, &parent).unwrap();
        assert!(client::materialization_ready(&paths).is_err());

        // The synthetic input port shares the actual admitted paths and leaf identity. Only
        // its parent admission fact changes, matching the real vault's repair below.
        let user = User {
            name: "fixture".into(),
            uid: before.uid(),
            gid: before.gid(),
            home: home.to_string_lossy().into_owned(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(&user);
        for (path, bytes) in [
            (token.clone(), b"inert_fixture_token".as_slice()),
            (
                home.join(".secrets/github-repositories.json"),
                inventory.as_slice(),
            ),
        ] {
            sys.file(path.to_str().unwrap(), bytes).stat_of(
                path.to_str().unwrap(),
                Stat {
                    uid: user.uid,
                    gid: user.gid,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: bytes.len() as u64,
                },
            );
        }
        sys.stat_of(parent.to_str().unwrap(), Live::new().stat(&parent).unwrap());
        let metadata = Metadata::read(&sys, &paths, None, &mut RepositoryInputPathCache::default())
            .unwrap()
            .0;
        let mut inputs = Inputs::new(&sys, &paths, true, now);
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "blocked but unchanged"
        );

        std::fs::remove_file(&parent).unwrap();
        std::fs::rename(&held, &parent).unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        assert_eq!(std::fs::metadata(&token).unwrap().ino(), before.ino());
        sys.stat_of(parent.to_str().unwrap(), Live::new().stat(&parent).unwrap());
        assert_eq!(
            metadata.files,
            Metadata::read(&sys, &paths, None, &mut RepositoryInputPathCache::default())
                .unwrap()
                .0
                .files
        );
        assert_eq!(
            inputs.changed(&sys, true, now).unwrap(),
            Some(RepositoryWork::Refresh),
            "path admission repaired"
        );
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "one wake for the repair"
        );
        let mut policy = Live::new().stat(&parent).unwrap();
        policy.len += 1;
        sys.stat_of(parent.to_str().unwrap(), policy);
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "unrelated directory contents do not move admission"
        );
    }

    #[test]
    fn local_readiness_changes_wake_once_without_programs_or_unchanged_private_reads() {
        // The checkout scope is read at the first look and once a minute ([`CHECKOUT_LOOK`]).
        let now = Instant::now();
        let minute = |n: u32| now + CHECKOUT_LOOK * n;
        let user = User {
            name: "fixture".into(),
            uid: 501,
            gid: 20,
            home: "/input-observer-home".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Fake::root();
        sys.euid = user.uid;
        sys.logged_in(&user);
        let file = |path: &str, bytes: &[u8]| {
            sys.file(path, bytes).stat_of(
                path,
                Stat {
                    uid: user.uid,
                    gid: user.gid,
                    mode: 0o600,
                    dir: false,
                    symlink: false,
                    len: bytes.len() as u64,
                },
            );
        };
        let token = format!("{}/.secrets/gh_access_token", user.home);
        let inventory = format!("{}/.secrets/github-repositories.json", user.home);
        let helper = format!("{}/.local/bin/aterm", user.home);
        let profile = format!("{}/.config/aterm/init.toml", user.home);
        file(&profile, b"");
        file(&token, b"inert_token_one");
        file(
            &inventory,
            br#"{"schema":2,"account":"fixture","repositories":[{"id":1,"name":"code","owner":{"login":"fixture"},"full_name":"fixture/code","clone_url":"https://github.com/fixture/code.git","private":true,"default_branch":"main"}]}"#,
        );
        let directory = Stat {
            uid: user.uid,
            gid: user.gid,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        };
        sys.stat_of(&user.home, directory);
        for parent in std::path::Path::new(&profile).ancestors().skip(1) {
            sys.stat_of(parent.to_str().unwrap(), directory);
        }
        let checkout = format!("{}/code", user.home);
        sys.stat_of(&checkout, directory);
        file(&helper, b"helper one");
        sys.executable(aterm_machines::items::github::CLT_GIT, b"git one");
        let private_reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&private_reads);
        sys.on_owned_read(move |_, path| {
            if path.file_name().is_some_and(|name| {
                name == "gh_access_token"
                    || name == "github-repositories.json"
                    || name == "init.toml"
                    || name == "installed-command.json"
            }) {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        });
        let paths = Paths::locate_for_home(std::path::Path::new(&user.home));
        let mut inputs = Inputs::new(&sys, &paths, true, now);
        let context = aterm_machines::items::Ctx {
            facts: aterm_machines::model::HostFacts::default(),
            users: vec![user.clone()],
            console: None,
            host_name: String::new(),
            cache_dir: PathBuf::new(),
            disrupt: false,
        };
        let captured =
            aterm_machines::init_github::InitialRepositoryInputs::capture(&sys, &context).unwrap();
        let refresh_scope = captured.refresh_scope(&sys).unwrap();
        assert_eq!(
            inputs
                .seen
                .as_ref()
                .and_then(RepositoryInputMark::refresh_scope),
            Some(refresh_scope),
            "the actual observer and Native capture share the production scope builder"
        );
        private_reads.store(0, Ordering::SeqCst);
        for _ in 0..3 {
            assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        }
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "metadata gates private reads"
        );
        sys.stat_of(&format!("{checkout}/.git"), directory);
        assert!(
            inputs
                .changed(&sys, true, minute(1) - Duration::from_millis(1))
                .unwrap()
                .is_none(),
            "a look within the minute leaves the checkout scope unread"
        );
        assert!(
            inputs.changed(&sys, true, minute(1)).unwrap() == Some(RepositoryWork::Repair),
            "an externally completed checkout is useful input"
        );
        private_reads.store(0, Ordering::SeqCst);
        assert!(inputs.changed(&sys, true, minute(2)).unwrap().is_none());
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "cached checkout scope gates private reads too"
        );
        file(&format!("{checkout}/.git/config"), b"repaired origin");
        assert!(
            inputs
                .changed(&sys, true, minute(3) - Duration::from_millis(1))
                .unwrap()
                .is_none(),
            "a read that found the scope unchanged starts the minute too"
        );
        assert!(
            inputs.changed(&sys, true, minute(3)).unwrap() == Some(RepositoryWork::Repair),
            "in-place Git identity repair is useful input"
        );
        file(&format!("{checkout}/working.txt"), b"unrelated work");
        assert!(
            inputs.changed(&sys, true, minute(4)).unwrap().is_none(),
            "ordinary worktree content is not observed"
        );
        assert!(inputs.changed(&sys, true, minute(5)).unwrap().is_none());
        assert_eq!(
            inputs
                .seen
                .as_ref()
                .and_then(RepositoryInputMark::refresh_scope),
            Some(refresh_scope),
            "checkout-only repair cannot request its own remote refresh"
        );
        use aterm_secrets::converge::{RefreshObservation, RefreshVersion, RepositoryRequest};
        let before = RefreshObservation {
            scope: Some(refresh_scope),
            version: RefreshVersion {
                input: 7,
                attempt: 9,
                fulfilled: 9,
            },
            recheck_ms: Some(aterm_secrets::converge::RECHECK_MS),
            ..RefreshObservation::default()
        };
        // Receipt first, then local source capture, exactly as the real factory.
        let mut delayed = Inputs::new(&sys, &paths, true, now);
        let mut cursor = RepositoryRefreshCursor::new(before, delayed.seen.as_ref());
        assert!(
            delayed
                .request(&sys, true, before, &mut cursor, now)
                .is_some()
        );
        file(&token, b"inert_token_delayed");
        let next = repository_input_mark(&sys)
            .unwrap()
            .refresh_scope()
            .unwrap();
        let winner = RefreshObservation {
            scope: Some(next),
            version: RefreshVersion {
                input: 8,
                attempt: 10,
                fulfilled: 10,
            },
            ..before
        };
        let mut unavailable = user.clone();
        unavailable.uid += 1;
        sys.logged_in(&unavailable);
        assert!(
            delayed
                .request(&sys, true, winner, &mut cursor, now)
                .is_none(),
            "an unavailable own-account sample cannot relabel retained source facts"
        );
        sys.logged_in(&user);
        let Some(RepositoryRequest::Refresh(request)) =
            delayed.request(&sys, true, winner, &mut cursor, now)
        else {
            panic!("the actual callback observes the delivered source");
        };
        assert_eq!(
            request.base_input, 7,
            "a delayed actor retains its previous source basis"
        );
        assert!(
            winner.fulfills(request),
            "the winner already refreshed this materialization"
        );
        // An observed intermediate source cannot let an old same-byte success
        // acknowledge the later return before any owner admitted that transition.
        file(&token, b"inert_token_one");
        let Some(RepositoryRequest::Refresh(away)) =
            delayed.request(&sys, true, winner, &mut cursor, now)
        else {
            panic!("the intermediate source is observed");
        };
        file(&token, b"inert_token_delayed");
        let Some(RepositoryRequest::Refresh(back)) =
            delayed.request(&sys, true, winner, &mut cursor, now)
        else {
            panic!("the return is observed");
        };
        assert_eq!(back.scope, winner.scope);
        assert!(
            !winner.fulfills(away.merge(back)),
            "unadmitted ABA still owes a new generation"
        );
        // Semantic source A returns before this actor samples at all. The init
        // profile's inode is excluded from scope, so the semantic selection is A.
        let declared = b"[develop]\ncheckouts=['~/elsewhere']\n";
        file(&profile, declared);
        let other_scope = repository_input_mark(&sys)
            .unwrap()
            .refresh_scope()
            .unwrap();
        let other = RefreshObservation {
            scope: Some(other_scope),
            version: RefreshVersion {
                input: 9,
                attempt: 11,
                fulfilled: 11,
            },
            ..winner
        };
        file(&profile, b"");
        let Some(RepositoryRequest::Refresh(intervening)) =
            delayed.request(&sys, true, other, &mut cursor, now)
        else {
            panic!("a current source after a foreign admitted generation owes immediate refresh");
        };
        assert_eq!(intervening.scope, winner.scope);
        assert_eq!(intervening.base_input, 9);
        file(&profile, declared);
        let Some(RepositoryRequest::Refresh(returned)) =
            delayed.request(&sys, true, other, &mut cursor, now)
        else {
            panic!("the later semantic selection is observed");
        };
        assert_eq!(
            returned.base_input, 9,
            "the confirmed intervening source retains its temporal floor"
        );
        assert!(
            !other.fulfills(intervening.merge(returned)),
            "an earlier same-selection success cannot cover the return"
        );
        file(&profile, b"");
        assert_eq!(
            inputs.changed(&sys, true, now).unwrap(),
            Some(RepositoryWork::Refresh)
        );
        for declared in [
            b"[develop]\ncheckouts=['~/code']\n".as_slice(),
            b"".as_slice(),
        ] {
            file(&profile, declared);
            assert_eq!(
                inputs.changed(&sys, true, now).unwrap(),
                Some(RepositoryWork::Refresh)
            );
            private_reads.store(0, Ordering::SeqCst);
            assert!(inputs.changed(&sys, true, now).unwrap().is_none());
            assert_eq!(
                private_reads.load(Ordering::SeqCst),
                0,
                "settled refresh scope stays metadata-only"
            );
        }
        file(&profile, b"[develop]\ncheckouts=[]\n");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "an empty declaration changes no refresh scope"
        );
        for (path, bytes) in [
            (token.as_str(), b"inert_token_two".as_slice()),
            (helper.as_str(), b"helper two".as_slice()),
            (
                aterm_machines::items::github::CLT_GIT,
                b"git two".as_slice(),
            ),
        ] {
            file(path, bytes);
            assert_eq!(
                inputs.changed(&sys, true, now).unwrap(),
                Some(RepositoryWork::Refresh),
                "changed declared input"
            );
            assert!(
                inputs.changed(&sys, true, now).unwrap().is_none(),
                "one wake for unchanged readiness"
            );
        }
        let declaration = aterm_types::installed_command::path(std::path::Path::new(&user.home));
        let custom = format!("{}/custom store/aterm", user.home);
        let payload = format!("{}/custom payload/aterm", user.home);
        sys.link(&custom, &payload);
        let select = |path: &str| {
            let bytes = aterm_types::installed_command::Selection::new(path.into())
                .unwrap()
                .encode()
                .unwrap();
            file(declaration.to_str().unwrap(), &bytes);
        };
        select(&custom);
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "declared missing helper replaces legacy readiness"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        private_reads.store(0, Ordering::SeqCst);
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "settled declaration bytes stay unread"
        );
        file(&payload, b"arrived custom command");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "a dangling selected target repair wakes"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        file(&helper, b"old legacy command changed");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "exclusive selection retires legacy paths"
        );
        let replacement_helper = format!("{}/replacement store/aterm", user.home);
        select(&replacement_helper);
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "declaration replacement is observed"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        file(&payload, b"old custom command changed");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "old declared targets retire"
        );
        file(&replacement_helper, b"repaired replacement command");
        assert!(inputs.changed(&sys, true, now).unwrap().is_some());
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        file(
            &format!("{}/.config/aterm/init.toml", user.home),
            b"[develop]\ncheckouts=[]\n",
        );
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "unrelated settings do not recreate a source change"
        );
        let selected = format!("{}/selected-token", user.home);
        file(&selected, b"selected_token_one");
        let scoped = format!("[github]\naccount='fixture'\ntoken_file='{selected}'\n");
        file(&profile, scoped.as_bytes());
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "a profile retarget is current input"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        private_reads.store(0, Ordering::SeqCst);
        file(&token, b"old_unselected_token");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_none(),
            "old token paths retire"
        );
        assert_eq!(private_reads.load(Ordering::SeqCst), 0);
        file(&selected, b"selected_token_two");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "new token path remains observed"
        );
        let parent = std::path::Path::new(&profile).parent().unwrap();
        sys.stat_of(
            parent.to_str().unwrap(),
            Stat {
                mode: 0o770,
                ..directory
            },
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_some());
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        sys.stat_of(parent.to_str().unwrap(), directory);
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "unchanged-leaf parent admission repairs wake"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        let replacement = format!("{}/replacement-token", user.home);
        file(&replacement, b"replacement_token_one");
        let replacement_profile =
            format!("[github]\naccount='fixture'\ntoken_file='{replacement}'\n").into_bytes();
        let change_once = Arc::new(std::sync::atomic::AtomicBool::new(true));
        sys.on_owned_read({
            let profile = profile.clone();
            let change_once = Arc::clone(&change_once);
            move |sys, path| {
                if path == std::path::Path::new(&profile)
                    && change_once.swap(false, Ordering::SeqCst)
                {
                    sys.file(&profile, &replacement_profile).stat_of(
                        &profile,
                        Stat {
                            uid: 501,
                            gid: 20,
                            mode: 0o600,
                            dir: false,
                            symlink: false,
                            len: replacement_profile.len() as u64,
                        },
                    );
                }
            }
        });
        file(
            &profile,
            format!("{scoped}# new selection read\n").as_bytes(),
        );
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "a raced profile read is recaptured, not relabeled"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        file(&replacement, b"replacement_token_two");
        assert!(
            inputs.changed(&sys, true, now).unwrap().is_some(),
            "the raced-in selection remains observable"
        );
        assert!(inputs.changed(&sys, true, now).unwrap().is_none());
        private_reads.store(0, Ordering::SeqCst);
        file(&replacement, b"inert_token_new");
        assert!(
            inputs.changed(&sys, false, now).unwrap().is_none(),
            "Stop/off holds the observer"
        );
        assert_eq!(private_reads.load(Ordering::SeqCst), 0);
        assert!(
            inputs.changed(&sys, true, now).unwrap() == Some(RepositoryWork::Refresh),
            "current authority resumes with fresh inputs"
        );
        assert!(
            sys.calls().iter().all(|call| call == "identity"),
            "{:?}",
            sys.calls()
        );
    }
}
