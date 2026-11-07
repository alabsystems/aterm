// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable-owned readiness observation for the subscriber's ordinary converge job.
//! Reuse the existing one-second lane; metadata gates private token/inventory reads.

use aterm_machines::init_github::{
    RepositoryCheckoutMetadata, RepositoryInputMark, RepositoryInputPathCache,
    repository_input_mark,
};
use aterm_machines::port::{Live, System};
use aterm_secrets::client::Paths;
use aterm_secrets::converge::RepositoryWork;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;

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
}

impl Inputs {
    fn new(sys: &dyn System, paths: &Paths, allowed: bool) -> Self {
        let mut inputs = Self {
            paths: paths.clone(),
            metadata: None,
            seen: None,
            held: !allowed,
            control: RepositoryInputPathCache::default(),
        };
        if allowed {
            let _ = inputs.changed(sys, true);
        }
        inputs
    }

    fn changed(&mut self, sys: &dyn System, allowed: bool) -> Option<RepositoryWork> {
        if !allowed {
            self.held = true;
            return None;
        }
        let Ok((mut metadata, reusable)) =
            Metadata::read(sys, &self.paths, self.seen.as_ref(), &mut self.control)
        else {
            return None;
        };
        if reusable && !self.held && self.metadata.as_ref() == Some(&metadata) {
            return None;
        }
        let Ok(mark) = repository_input_mark(sys) else {
            return None;
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
        self.metadata = Some(metadata);
        self.seen = Some(mark);
        self.held = false;
        work
    }
}

/// This observer follows only the subscriber's own real account. The ordinary job still reads
/// current source, materialization, Stop/off and path admission before every effect.
pub(super) fn observe(
    paths: &aterm_secrets::client::Paths,
) -> aterm_secrets::client::SubscriberInputs {
    let sys = Live::new();
    if !sys.identity().is_ok_and(|identity| {
        identity.user.uid == sys.euid()
            && sys.euid() != 0
            && std::path::Path::new(&identity.user.home) == paths.home
    }) {
        return aterm_secrets::client::SubscriberInputs::new(Box::new(|| None));
    }
    let home = paths.home.clone();
    let allowed = move || {
        aterm_secrets::swarm::enabled_in_home(&home).unwrap_or(false)
            && !super::converge::setup_holds()
                .holds(aterm_types::macos_setup::Setting::Repositories)
    };
    let mut inputs = Inputs::new(&sys, paths, allowed());
    aterm_secrets::client::SubscriberInputs::new(Box::new(move || inputs.changed(&sys, allowed())))
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
        let mut inputs = Inputs::new(&sys, &paths, true);
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
            inputs.changed(&sys, true) == Some(RepositoryWork::Refresh),
            "the repaired execution route is new readiness"
        );
        assert!(
            inputs.changed(&sys, true).is_none(),
            "one wake for the same repair"
        );
    }

    #[test]
    fn repaired_materialization_parent_wakes_without_changing_the_credential_inode() {
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
        let mut inputs = Inputs::new(&sys, &paths, true);
        assert!(
            inputs.changed(&sys, true).is_none(),
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
            inputs.changed(&sys, true),
            Some(RepositoryWork::Refresh),
            "path admission repaired"
        );
        assert!(
            inputs.changed(&sys, true).is_none(),
            "one wake for the repair"
        );
        let mut policy = Live::new().stat(&parent).unwrap();
        policy.len += 1;
        sys.stat_of(parent.to_str().unwrap(), policy);
        assert!(
            inputs.changed(&sys, true).is_none(),
            "unrelated directory contents do not move admission"
        );
    }

    #[test]
    fn local_readiness_changes_wake_once_without_programs_or_unchanged_private_reads() {
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
        file(aterm_machines::items::github::CLT_GIT, b"git one");
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
        let mut inputs = Inputs::new(&sys, &paths, true);
        private_reads.store(0, Ordering::SeqCst);
        for _ in 0..3 {
            assert!(inputs.changed(&sys, true).is_none());
        }
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "metadata gates private reads"
        );
        sys.stat_of(&format!("{checkout}/.git"), directory);
        assert!(
            inputs.changed(&sys, true) == Some(RepositoryWork::Repair),
            "an externally completed checkout is useful input"
        );
        private_reads.store(0, Ordering::SeqCst);
        assert!(inputs.changed(&sys, true).is_none());
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "cached checkout scope gates private reads too"
        );
        file(&format!("{checkout}/working.txt"), b"unrelated work");
        assert!(
            inputs.changed(&sys, true).is_none(),
            "ordinary worktree content is not observed"
        );
        file(&format!("{checkout}/.git/config"), b"repaired origin");
        assert!(
            inputs.changed(&sys, true) == Some(RepositoryWork::Repair),
            "in-place Git identity repair is useful input"
        );
        assert!(inputs.changed(&sys, true).is_none());
        for declared in [
            b"[develop]\ncheckouts=['~/code']\n".as_slice(),
            b"".as_slice(),
        ] {
            file(&profile, declared);
            assert_eq!(inputs.changed(&sys, true), Some(RepositoryWork::Refresh));
            private_reads.store(0, Ordering::SeqCst);
            assert!(inputs.changed(&sys, true).is_none());
            assert_eq!(
                private_reads.load(Ordering::SeqCst),
                0,
                "settled refresh scope stays metadata-only"
            );
        }
        file(&profile, b"[develop]\ncheckouts=[]\n");
        assert!(
            inputs.changed(&sys, true).is_none(),
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
                inputs.changed(&sys, true),
                Some(RepositoryWork::Refresh),
                "changed declared input"
            );
            assert!(
                inputs.changed(&sys, true).is_none(),
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
            inputs.changed(&sys, true).is_some(),
            "declared missing helper replaces legacy readiness"
        );
        assert!(inputs.changed(&sys, true).is_none());
        private_reads.store(0, Ordering::SeqCst);
        assert!(inputs.changed(&sys, true).is_none());
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "settled declaration bytes stay unread"
        );
        file(&payload, b"arrived custom command");
        assert!(
            inputs.changed(&sys, true).is_some(),
            "a dangling selected target repair wakes"
        );
        assert!(inputs.changed(&sys, true).is_none());
        file(&helper, b"old legacy command changed");
        assert!(
            inputs.changed(&sys, true).is_none(),
            "exclusive selection retires legacy paths"
        );
        let replacement_helper = format!("{}/replacement store/aterm", user.home);
        select(&replacement_helper);
        assert!(
            inputs.changed(&sys, true).is_some(),
            "declaration replacement is observed"
        );
        assert!(inputs.changed(&sys, true).is_none());
        file(&payload, b"old custom command changed");
        assert!(
            inputs.changed(&sys, true).is_none(),
            "old declared targets retire"
        );
        file(&replacement_helper, b"repaired replacement command");
        assert!(inputs.changed(&sys, true).is_some());
        assert!(inputs.changed(&sys, true).is_none());
        file(
            &format!("{}/.config/aterm/init.toml", user.home),
            b"[develop]\ncheckouts=[]\n",
        );
        assert!(
            inputs.changed(&sys, true).is_none(),
            "unrelated settings do not recreate a source change"
        );
        let selected = format!("{}/selected-token", user.home);
        file(&selected, b"selected_token_one");
        let scoped = format!("[github]\naccount='fixture'\ntoken_file='{selected}'\n");
        file(&profile, scoped.as_bytes());
        assert!(
            inputs.changed(&sys, true).is_some(),
            "a profile retarget is current input"
        );
        assert!(inputs.changed(&sys, true).is_none());
        private_reads.store(0, Ordering::SeqCst);
        file(&token, b"old_unselected_token");
        assert!(
            inputs.changed(&sys, true).is_none(),
            "old token paths retire"
        );
        assert_eq!(private_reads.load(Ordering::SeqCst), 0);
        file(&selected, b"selected_token_two");
        assert!(
            inputs.changed(&sys, true).is_some(),
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
        assert!(inputs.changed(&sys, true).is_some());
        assert!(inputs.changed(&sys, true).is_none());
        sys.stat_of(parent.to_str().unwrap(), directory);
        assert!(
            inputs.changed(&sys, true).is_some(),
            "unchanged-leaf parent admission repairs wake"
        );
        assert!(inputs.changed(&sys, true).is_none());
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
            inputs.changed(&sys, true).is_some(),
            "a raced profile read is recaptured, not relabeled"
        );
        assert!(inputs.changed(&sys, true).is_none());
        file(&replacement, b"replacement_token_two");
        assert!(
            inputs.changed(&sys, true).is_some(),
            "the raced-in selection remains observable"
        );
        assert!(inputs.changed(&sys, true).is_none());
        private_reads.store(0, Ordering::SeqCst);
        file(&replacement, b"inert_token_new");
        assert!(
            inputs.changed(&sys, false).is_none(),
            "Stop/off holds the observer"
        );
        assert_eq!(private_reads.load(Ordering::SeqCst), 0);
        assert!(
            inputs.changed(&sys, true) == Some(RepositoryWork::Refresh),
            "current authority resumes with fresh inputs"
        );
        assert!(
            sys.calls().iter().all(|call| call == "identity"),
            "{:?}",
            sys.calls()
        );
    }
}
