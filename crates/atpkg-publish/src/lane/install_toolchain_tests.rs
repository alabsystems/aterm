// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The install builder uses ordinary discovery, including its refusal boundaries.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::TargoBuild;

struct Fixture {
    _root: aterm_tempfile::TempDir,
    build: TargoBuild,
    repo: PathBuf,
    rustup: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        let rustup = root.path().join("relocated-rustup");
        std::fs::create_dir_all(&repo).expect("repo");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(
            repo.join("rust-toolchain.toml"),
            "[toolchain]\nchannel = 'trust'\n",
        )
        .expect("pin");
        let build = TargoBuild {
            prefix: home.join("pkg"),
            home,
        };
        Self {
            _root: root,
            build,
            repo,
            rustup,
        }
    }

    fn native(&self, name: &str) -> PathBuf {
        let bin = self.build.home.join(name).join("bin");
        let sysroot = bin.parent().expect("sysroot");
        std::fs::create_dir_all(sysroot.join("lib/rustlib")).expect("rustlib");
        std::fs::create_dir_all(&bin).expect("bin");
        let quoted = sysroot.to_string_lossy().replace('\'', "'\\''");
        for (name, body) in [
            ("targo", "#!/bin/sh\nexit 0\n".to_owned()),
            ("trustc", format!("#!/bin/sh\nprintf '%s\\n' '{quoted}'\n")),
        ] {
            let file = bin.join(name);
            std::fs::write(&file, body).expect("native stand-in");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))
                .expect("executable");
        }
        bin
    }

    fn rustup(&self, bin: &Path) {
        let links = self.rustup.join("toolchains");
        std::fs::create_dir_all(&links).expect("toolchains");
        std::os::unix::fs::symlink(bin.parent().expect("root"), links.join("trust"))
            .expect("rustup link");
    }

    fn lock_missing_build(&self) {
        std::fs::write(
            self.repo.join("trust.lock"),
            format!(
                "version = 1\n[trust]\nbuild = 16387\ntree_root = '{}'\ncompiler_commit = '{}'\nreceipt = 'SEED-MINT sha256:{}'\n[stock]\ntoolchain = '1.99.1'\n",
                "a".repeat(64), "b".repeat(40), "c".repeat(64),
            ),
        )
        .expect("lock");
    }
}

#[test]
fn a_native_rustup_toolchain_is_usable_without_a_managed_store() {
    let fx = Fixture::new();
    let bin = fx.native("native");
    fx.rustup(&bin);
    let resolved = fx
        .build
        .resolve(&fx.repo, None, &fx.rustup, OsStr::new(""), &mut |_| Ok(()))
        .expect("native Trust");
    assert_eq!(
        resolved.stage2_dir,
        bin.canonicalize().expect("physical bin")
    );
    assert!(!fx.build.prefix.join("store").exists());
}

#[test]
fn an_invalid_explicit_selection_never_falls_back_to_native_rustup() {
    let fx = Fixture::new();
    fx.rustup(&fx.native("native"));
    let missing = fx.build.home.join("missing/bin");
    let error = fx
        .build
        .resolve(
            &fx.repo,
            Some(&missing),
            &fx.rustup,
            OsStr::new(""),
            &mut |_| Ok(()),
        )
        .expect_err("explicit input refuses");
    assert!(error.to_string().contains("TRUST_STAGE2_BIN"), "{error}");
}

#[test]
fn a_missing_locked_build_never_falls_back_to_an_unrelated_native_toolchain() {
    let fx = Fixture::new();
    let bin = fx.native("native");
    fx.rustup(&bin);
    fx.lock_missing_build();
    let error = fx
        .build
        .resolve(&fx.repo, None, &fx.rustup, bin.as_os_str(), &mut |_| Ok(()))
        .expect_err("locked source cannot drift");
    assert!(error.to_string().contains("16387"), "{error}");
}

#[test]
fn a_malformed_lock_is_not_an_unlocked_source_tree() {
    let fx = Fixture::new();
    let bin = fx.native("native");
    fx.rustup(&bin);
    std::fs::write(fx.repo.join("trust.lock"), "not a lock\n").expect("bad lock");
    assert!(
        fx.build
            .resolve(&fx.repo, None, &fx.rustup, bin.as_os_str(), &mut |_| Ok(()))
            .is_err()
    );
}

#[test]
fn the_supported_explicit_override_keeps_its_lock_provenance() {
    let fx = Fixture::new();
    let bin = fx.native("native");
    fx.lock_missing_build();
    let resolved = fx
        .build
        .resolve(
            &fx.repo,
            Some(&bin),
            &fx.rustup,
            OsStr::new(""),
            &mut |_| Ok(()),
        )
        .expect("explicit canonical override");
    assert_eq!(
        resolved.stage2_dir,
        bin.canonicalize().expect("physical bin")
    );
    assert!(
        resolved
            .lock_override
            .as_deref()
            .is_some_and(|why| why.contains("TRUST_STAGE2_BIN") && why.contains("trust.lock"))
    );
}

#[test]
fn a_probe_custody_refusal_is_not_a_request_to_try_another_toolchain() {
    let fx = Fixture::new();
    let bin = fx.native("native");
    fx.rustup(&bin);
    let mut calls = 0;
    let error = fx
        .build
        .resolve(
            &fx.repo,
            Some(&bin),
            &fx.rustup,
            bin.as_os_str(),
            &mut |_| {
                calls += 1;
                Err("fixture custody refused".to_owned())
            },
        )
        .expect_err("guard must refuse");
    assert_eq!(calls, 1);
    assert!(
        error.to_string().contains("fixture custody refused"),
        "{error}"
    );
}
