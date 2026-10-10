// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A SEED WITHOUT THE INSTALL CONSENT ADOPTS NOTHING, at the real process edge.
//!
//! A seed that is switched off must notice before it writes `<prefix>/adopted`, lays a
//! pending stub under `bin/` and `agents/`, lays the reroute stubs or reasserts the
//! rustup seam: every stub would promise a pass the switch refuses, and the adoption
//! would install the whole set the first tick after the switch was lifted. The switch is
//! the ONE install consent, `[packages] auto_install = false` in a scratch aterm.toml.
//! The store stays untouched — then, as the non-vacuity half, the SAME fixture with the
//! default config adopts, so the first half cannot pass merely because the fixture never
//! reaches the adoption line.
//!
//! NEVER THE REAL STORE OR MACHINE. HOME is a temp directory (so the default prefix
//! and `~/.rustup` sit under it, and the machine-settings lane refuses a synthetic
//! home), XDG_CONFIG_HOME is a temp directory holding the scratch config, and the
//! registry is an empty local dir (no network).

#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-seed-disabled-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(root.path().join("registry")).unwrap();
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
        let config = self.root.path().join("config");
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
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
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
