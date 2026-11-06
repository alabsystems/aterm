// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Executable-owned readiness observation for the subscriber's ordinary converge job.
//! Reuse the existing one-second lane; metadata gates private token/inventory reads.

use aterm_machines::init_github::{
    RepositoryInputMark, repository_input_mark, repository_input_watch_paths,
};
use aterm_machines::port::{Live, System};
use aterm_secrets::client::Paths;
use std::collections::BTreeSet;
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
}

impl Metadata {
    fn read(sys: &dyn System, paths: &Paths) -> Result<Self, String> {
        let files: Vec<_> = repository_input_watch_paths(sys)?
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
                let policy = sys
                    .stat(path)
                    .map(|stat| Policy {
                        uid: stat.uid,
                        gid: stat.gid,
                        mode: stat.mode,
                        dir: stat.dir,
                        symlink: stat.symlink,
                    })
                    .map_err(|why| why.kind());
                (path.to_path_buf(), policy)
            })
            .collect();
        Ok(Self { files, admission })
    }
}

struct Inputs {
    paths: Paths,
    metadata: Option<Metadata>,
    seen: Option<(RepositoryInputMark, Admission)>,
    held: bool,
}

impl Inputs {
    fn new(sys: &dyn System, paths: &Paths, allowed: bool) -> Self {
        let mut inputs = Self {
            paths: paths.clone(),
            metadata: None,
            seen: None,
            held: !allowed,
        };
        if allowed {
            let _ = inputs.changed(sys, true);
        }
        inputs
    }

    fn changed(&mut self, sys: &dyn System, allowed: bool) -> bool {
        if !allowed {
            self.held = true;
            return false;
        }
        let Ok(metadata) = Metadata::read(sys, &self.paths) else {
            return false;
        };
        if !self.held && self.metadata.as_ref() == Some(&metadata) {
            return false;
        }
        let Ok(mark) = repository_input_mark(sys) else {
            return false;
        };
        let observed = (mark, metadata.admission.clone());
        let changed = self.held || self.seen.as_ref() != Some(&observed);
        self.metadata = Some(metadata);
        self.seen = Some(observed);
        self.held = false;
        changed
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
        return Box::new(|| false);
    }
    let home = paths.home.clone();
    let allowed = move || {
        aterm_secrets::swarm::enabled_in_home(&home).unwrap_or(false)
            && !super::converge::setup_holds()
                .holds(aterm_types::macos_setup::Setting::Repositories)
    };
    let mut inputs = Inputs::new(&sys, paths, allowed());
    Box::new(move || inputs.changed(&sys, allowed()))
}

#[cfg(test)]
mod tests {
    use super::*;
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
            inputs.changed(&sys, true),
            "the repaired execution route is new readiness"
        );
        assert!(!inputs.changed(&sys, true), "one wake for the same repair");
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
        let metadata = Metadata::read(&sys, &paths).unwrap();
        let mut inputs = Inputs::new(&sys, &paths, true);
        assert!(!inputs.changed(&sys, true), "blocked but unchanged");

        std::fs::remove_file(&parent).unwrap();
        std::fs::rename(&held, &parent).unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        assert_eq!(std::fs::metadata(&token).unwrap().ino(), before.ino());
        sys.stat_of(parent.to_str().unwrap(), Live::new().stat(&parent).unwrap());
        assert_eq!(metadata.files, Metadata::read(&sys, &paths).unwrap().files);
        assert!(inputs.changed(&sys, true), "path admission repaired");
        assert!(!inputs.changed(&sys, true), "one wake for the repair");
        let mut policy = Live::new().stat(&parent).unwrap();
        policy.len += 1;
        sys.stat_of(parent.to_str().unwrap(), policy);
        assert!(
            !inputs.changed(&sys, true),
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
        file(&token, b"inert_token_one");
        file(
            &inventory,
            br#"{"schema":2,"account":"fixture","repositories":[]}"#,
        );
        file(&helper, b"helper one");
        file(aterm_machines::items::github::CLT_GIT, b"git one");
        let private_reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&private_reads);
        sys.on_owned_read(move |_, path| {
            if path
                .file_name()
                .is_some_and(|name| name == "gh_access_token" || name == "github-repositories.json")
            {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        });
        let paths = Paths::locate_for_home(std::path::Path::new(&user.home));
        let mut inputs = Inputs::new(&sys, &paths, true);
        private_reads.store(0, Ordering::SeqCst);
        for _ in 0..3 {
            assert!(!inputs.changed(&sys, true));
        }
        assert_eq!(
            private_reads.load(Ordering::SeqCst),
            0,
            "metadata gates private reads"
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
            assert!(inputs.changed(&sys, true), "changed declared input");
            assert!(
                !inputs.changed(&sys, true),
                "one wake for unchanged readiness"
            );
        }
        file(
            &format!("{}/.config/aterm/init.toml", user.home),
            b"[develop]\ncheckouts=[]\n",
        );
        assert!(
            !inputs.changed(&sys, true),
            "unrelated settings do not recreate a source change"
        );
        private_reads.store(0, Ordering::SeqCst);
        file(&token, b"inert_token_new");
        assert!(!inputs.changed(&sys, false), "Stop/off holds the observer");
        assert_eq!(private_reads.load(Ordering::SeqCst), 0);
        assert!(
            inputs.changed(&sys, true),
            "current authority resumes with fresh inputs"
        );
        assert!(
            sys.calls().iter().all(|call| call == "identity"),
            "{:?}",
            sys.calls()
        );
    }
}
