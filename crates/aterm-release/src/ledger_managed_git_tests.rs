// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

struct Store {
    _temp: aterm_tempfile::TempDir,
    layout: atpkg::Layout,
}

impl Store {
    fn new() -> Self {
        let temp = aterm_tempfile::tempdir().unwrap();
        let layout = atpkg::Layout {
            prefix: temp.path().canonicalize().unwrap().join("pkg"),
        };
        std::fs::create_dir_all(&layout.prefix).unwrap();
        Self {
            _temp: temp,
            layout,
        }
    }

    fn install(&self, build: u64) -> PathBuf {
        let root = self.layout.build_dir("git", build);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let program = root.join("bin/git");
        std::fs::write(&program, format!(
            "#!/bin/sh\nprintf 'git version 2.{build}.0\\n'\nprintf 'exec=%s\\n' \"$GIT_EXEC_PATH\"\n"
        )).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = atpkg::shim_env::ShimEnv::admit(&["GIT_EXEC_PATH=managed-exec".into()]).unwrap();
        atpkg::activate::install_shims_env(
            &self.layout,
            &root,
            &["git".into()],
            atpkg::activate::Aliases::Off,
            &env,
        )
        .unwrap();
        atpkg::activate::activate_build(&self.layout, &root).unwrap();
        atpkg::store::mark_build_ready(&root).unwrap();
        let mut status = atpkg::status::Status::default();
        status.programs.insert(
            "git".into(),
            atpkg::status::ProgramStatus {
                installed_build: Some(build),
                state: "fixture signed root".into(),
                tree_root: atpkg::tree::tree_root(&root).unwrap(),
            },
        );
        atpkg::status::write(&self.layout, &status).unwrap();
        program
    }
}

#[test]
fn absent_managed_git_keeps_the_os_fallback() {
    let store = Store::new();
    let selected = managed_git(&store.layout).unwrap();
    assert_eq!(selected.program, Path::new(aterm_uds::os_path!("git")));
    assert!(selected._lease.is_none());
}

#[test]
fn managed_git_uses_the_signed_payload_and_its_environment_for_object_reads() {
    let store = Store::new();
    let program = store.install(56);
    let selected = managed_git(&store.layout).unwrap();
    assert_eq!(selected.program, program);
    for mut cmd in [
        selected.command(),
        selected.object_command(&store.layout.prefix),
    ] {
        let output = cmd.arg("version").output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "git version 2.56.0\nexec=managed-exec\n"
        );
    }
    let subject = atpkg::lease::Subject::Build {
        program: "git".into(),
        build: 56,
    };
    assert!(matches!(
        atpkg::lease::holders(&store.layout.prefix, &subject),
        atpkg::lease::Holders::Held(_)
    ));
}

#[test]
fn broken_managed_git_never_falls_back_to_another_executable() {
    for failure in ["incomplete", "unsigned", "changed", "foreign"] {
        let store = Store::new();
        let program = store.install(56);
        match failure {
            "incomplete" => std::fs::remove_file(
                atpkg::store::ready_marker_path(&store.layout.build_dir("git", 56)).unwrap(),
            )
            .unwrap(),
            "unsigned" => {
                atpkg::status::write(&store.layout, &atpkg::status::Status::default()).unwrap()
            }
            "changed" => std::fs::write(&program, b"#!/bin/sh\nexit 0\n").unwrap(),
            "foreign" => {
                std::fs::remove_file(&program).unwrap();
                symlink(aterm_uds::os_path!("git"), &program).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(managed_git(&store.layout).is_err(), "{failure}");
    }
    let store = Store::new();
    std::fs::create_dir_all(store.layout.bin_dir()).unwrap();
    symlink(
        store.layout.prefix.join("missing"),
        store.layout.bin_dir().join("git"),
    )
    .unwrap();
    assert!(
        managed_git(&store.layout).is_err(),
        "a dangling managed shim is not absence"
    );
}

#[test]
fn an_update_does_not_retarget_a_running_release_or_drop_its_lease() {
    let store = Store::new();
    let program = store.install(56);
    let selected = managed_git(&store.layout).unwrap();
    store.install(57);
    assert_eq!(selected.program, program);
    let output = selected.command().arg("version").output().unwrap();
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("git version 2.56.0\n")
    );
    let lease = selected._lease.unwrap();
    let subject = lease.subject().clone();
    assert!(matches!(
        atpkg::lease::holders(&store.layout.prefix, &subject),
        atpkg::lease::Holders::Held(_)
    ));
    lease.release();
    assert_eq!(
        atpkg::lease::holders(&store.layout.prefix, &subject),
        atpkg::lease::Holders::Free
    );
}
