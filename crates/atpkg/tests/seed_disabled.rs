// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A SEED WITHOUT THE INSTALL CONSENT ADOPTS NOTHING (2026-09-15, re-spelled
//! 2026-09-23), at the real process edge.
//!
//! `atpkg seed` used to write `<prefix>/adopted`, lay a pending stub for every
//! default-set / agent / extra name under `bin/` and `agents/`, lay the reroute stubs
//! and reassert the rustup seam, and only THEN notice that it was switched off. Every
//! stub then promised a pass that the same switch refuses, and the durable adoption
//! installed the whole set over the network the first tick after the switch was
//! lifted. The switch this drives is the ONE install consent, `[packages] auto_install
//! = false` in a scratch aterm.toml — the environment kill switch the test was first
//! written against, `ATPKG_DISABLE`, is gone (R2: settings, not env vars), and the
//! disabled-manager arm it reached is now only an unpinned build's. The store stays
//! untouched — then, as the non-vacuity half, the SAME fixture with the default config
//! adopts, so the first half cannot pass merely because the fixture never reaches the
//! adoption line. (It also ran once "with a (placeholder) seal", through the
//! `ATPKG_BUNDLED_SEED` seam; Phase 5 deleted the sealed-payload lane and the seam
//! with it.)
//!
//! NEVER THE REAL STORE OR MACHINE. HOME is a temp directory (so the default prefix
//! and `~/.rustup` sit under it, and the machine-settings lane refuses a synthetic
//! home), XDG_CONFIG_HOME is a temp directory holding the scratch config, and the
//! registry is an empty local dir (no network).

#![cfg(unix)]

use std::path::PathBuf;
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

    fn seed(&self, declined: bool) -> Output {
        let config = self.root.join("config");
        std::fs::create_dir_all(config.join("aterm")).unwrap();
        std::fs::write(
            config.join("aterm/aterm.toml"),
            if declined {
                "[packages]\nauto_install = false\n"
            } else {
                ""
            },
        )
        .unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg("seed")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.join("registry").display()),
            )
            .env_remove("RUSTUP_HOME")
            .env_remove("ATERM_CHILD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
fn a_declined_seed_adopts_nothing_and_lays_no_stubs() {
    let fx = Fixture::new("declined");
    let out = fx.seed(true);
    let text = describe(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "a declined seed is a quiet skip: {text}"
    );
    assert!(
        stdout.contains("[packages].auto_install = false — not installing"),
        "the skip says why: {text}"
    );
    assert_eq!(
        fx.adoption_writes(),
        Vec::<PathBuf>::new(),
        "`auto_install = false` must stop seed before adoption, stubs, reroutes or \
         the rustup seam: {text}"
    );

    // NON-VACUITY: the same fixture with the default config reaches the
    // adoption line and lays the stubs, so the empty lists above are the gate's doing.
    // (A build that pins no root key is disabled either way and has nothing to show.)
    if atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        let fx = Fixture::new("enabled");
        let out = fx.seed(false);
        let text = describe(&out);
        assert!(
            fx.layout.adopted().is_file(),
            "an enabled seed adopts: {text}"
        );
        assert!(
            fx.layout.bin_dir().is_dir(),
            "an enabled seed lays pending stubs: {text}"
        );
    }
}
