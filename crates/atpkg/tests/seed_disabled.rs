// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A DISABLED MANAGER ADOPTS NOTHING (2026-09-15), at the real process edge.
//!
//! `atpkg seed` used to write `<prefix>/adopted`, lay a pending stub for every
//! default-set / agent / extra name under `bin/` and `agents/`, lay the reroute stubs
//! and reassert the rustup seam, and only THEN notice `ATPKG_DISABLE` and say "lane
//! skipped (fail-closed)". Every stub then promised a pass that the same switch
//! refuses, and the durable adoption installed the whole set over the network the
//! first tick after the switch was lifted. This drives the dev binary with the switch
//! set, seedless and with a (placeholder) seal, and asserts the store is untouched —
//! then, as the non-vacuity half, drives the SAME fixture with the switch unset and
//! asserts it does adopt, so the first half cannot pass merely because the fixture
//! never reaches the adoption line.
//!
//! NEVER THE REAL STORE OR MACHINE. HOME is a temp directory (so the default prefix
//! and `~/.rustup` sit under it, and the machine-settings lane refuses a synthetic
//! home), XDG_CONFIG_HOME is absent, the registry is an empty local dir (no network),
//! and `ATPKG_BUNDLED_SEED` is pinned so an app bundle's seal on the developer's
//! machine is never discovered.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-seed-disabled-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(root.join("registry")).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(
            prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            prefix.display()
        );
        Self {
            root,
            home,
            layout: atpkg::store::Layout { prefix },
        }
    }

    /// A placeholder seal: `bundled_seed_dir` accepts a directory holding the four
    /// signed-registry files by name. Its contents never matter to the disabled lane,
    /// which must refuse before reading them.
    fn seal(&self) -> PathBuf {
        let seal = self.root.join("seal");
        std::fs::create_dir_all(&seal).unwrap();
        for f in [
            "index.toml",
            "index.toml.sig",
            "aterm-machines.toml",
            "aterm-machines.toml.sig",
        ] {
            std::fs::write(seal.join(f), b"placeholder\n").unwrap();
        }
        seal
    }

    fn seed(&self, disabled: bool, seal: Option<&Path>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg("seed")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.join("registry").display()),
            )
            .env(
                "ATPKG_BUNDLED_SEED",
                seal.map_or_else(|| "off".into(), |s| s.as_os_str().to_owned()),
            )
            .env_remove("RUSTUP_HOME")
            .env_remove("ATERM_CHILD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if disabled {
            cmd.env("ATPKG_DISABLE", "1");
        } else {
            cmd.env_remove("ATPKG_DISABLE");
        }
        cmd.output().expect("run dev atpkg seed")
    }

    /// Everything the adoption lane writes, by the crate's own path accessors.
    fn adoption_writes(&self) -> Vec<PathBuf> {
        [
            self.layout.adopted(),
            self.layout.bin_dir(),
            self.layout.agents_dir(),
            atpkg::reroute::dir(&self.layout),
            self.home.join(".rustup"),
        ]
        .into_iter()
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_disabled_manager_seed_adopts_nothing_and_lays_no_stubs() {
    for sealed in [false, true] {
        let fx = Fixture::new(if sealed { "sealed" } else { "seedless" });
        let seal = sealed.then(|| fx.seal());
        let out = fx.seed(true, seal.as_deref());
        let text = describe(&out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "a disabled seed is a quiet skip: {text}"
        );
        assert!(
            stdout.contains("the manager is disabled — lane skipped (fail-closed)"),
            "the refusal says why: {text}"
        );
        assert_eq!(
            stdout.contains("bundled seed present"),
            sealed,
            "the refusal names the seal it skipped, and only when there is one: {text}"
        );
        assert_eq!(
            fx.adoption_writes(),
            Vec::<PathBuf>::new(),
            "ATPKG_DISABLE must stop seed before adoption, stubs, reroutes or the rustup \
             seam (sealed={sealed}): {text}"
        );
    }

    // NON-VACUITY: the same seedless fixture with the switch unset reaches the
    // adoption line and lays the stubs, so the empty lists above are the gate's doing.
    // (A build that pins no root key is disabled either way and has nothing to show.)
    if atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS, false) {
        let fx = Fixture::new("enabled");
        let out = fx.seed(false, None);
        let text = describe(&out);
        assert!(
            fx.layout.adopted().is_file(),
            "an enabled seedless seed adopts: {text}"
        );
        assert!(
            fx.layout.bin_dir().is_dir(),
            "an enabled seed lays pending stubs: {text}"
        );
    }
}
