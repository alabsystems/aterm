// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! No landing wait (`docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md` Phase 2), at
//! the real process edge: the dev `atpkg` binary over a temp prefix.
//!
//! * `__landing` — the hidden verb a twin laid by an older client still execs while a
//!   landing marker stands — reached the way it is reached in the field: an older twin's
//!   own bytes (the self-update block, then the landing prelude) run in `/bin/sh` with a
//!   marker naming a LIVE writer (this test process) standing; `bin/<program>` runs at
//!   ONCE and nothing is printed.
//! * Every pass sweeps `landing/`: a mutating verb (`gc`, the one that needs no network)
//!   leaves no marker behind, whoever wrote it.
//!
//! Never the real store: `HOME` is a temp directory and `XDG_CONFIG_HOME` an absent one, so
//! the default prefix lands under that temp `HOME`; the registry is a local directory.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Fixture {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    config_home: PathBuf,
    registry: PathBuf,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-landing-pt-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        let config_home = root.path().join("config");
        let registry = root.path().join("registry");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&registry).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(prefix.starts_with(&home), "{}", prefix.display());
        let fx = Self {
            _root: root,
            home,
            config_home,
            registry,
            prefix,
        };
        fx.layout().ensure_dir(&fx.prefix).unwrap();
        fx
    }

    fn layout(&self) -> atpkg::store::Layout {
        atpkg::store::Layout {
            prefix: self.prefix.clone(),
        }
    }

    /// A managed `claude` at `build`: a store tree whose `bin/claude` echoes its argv and
    /// exits 7, and its `bin/` shim laid by the activation code.
    fn install_claude(&self, build: u64) -> PathBuf {
        let layout = self.layout();
        let dir = layout.build_dir("claude", build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin/claude");
        std::fs::write(&exe, b"#!/bin/sh\necho \"fake: $*\"\nexit 7\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        atpkg::activate::install_shims(
            &layout,
            &dir,
            &["claude".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
        exe
    }

    /// A landing marker naming a live writer and a build the shim does not run — the
    /// state the retired verb waited out.
    fn stand_marker(&self) -> PathBuf {
        let layout = self.layout();
        let marker = layout.landing_dir().join("claude");
        std::fs::create_dir_all(layout.landing_dir()).unwrap();
        std::fs::write(
            &marker,
            format!(
                "atpkg-landing-v1 build=2026092202 pid={} from=2026091601 version=2.1.281 \
                 from_version=2.1.280\n",
                std::process::id()
            ),
        )
        .unwrap();
        marker
    }

    /// `program argv…` confined to this fixture, both pipes captured, run to completion.
    fn run(&self, program: &Path, argv: &[OsString]) -> Output {
        Command::new(program)
            .args(argv)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("ATPKG_REGISTRY", format!("dir:{}", self.registry.display()))
            .env_remove(atpkg::cli::SPAWNER_PID_ENV)
            .stdin(Stdio::null())
            .output()
            .expect("run under the fixture")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn atpkg() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_atpkg"))
}

/// An older twin as it runs in the field: the bytes an older client laid in
/// `agents/claude` — the self-update block, then the landing prelude — run in `/bin/sh`
/// with the marker standing and this dev binary as the embedded atpkg. A non-verb takes
/// the hand-over and the store build runs at once, silently.
#[test]
fn an_older_twin_under_a_standing_marker_runs_the_tool_at_once() {
    let fx = Fixture::new("twin");
    let store = fx.install_claude(2026091601);
    let marker = fx.stand_marker();
    let q = |p: &Path| format!("'{}'", p.display());
    let twin = fx.prefix.join("agents/claude");
    std::fs::create_dir_all(twin.parent().unwrap()).unwrap();
    let body = format!(
        "#!/bin/sh\n\
         # atpkg agents twin: `claude update|upgrade|install` is answered by `atpkg __selfupdate` \
         — aterm updates this copy from its vendor and verifies every build here (aterm help \
         pkg).\n\
         case \"$1\" in\n  update|upgrade|install)\n    __atpkg={atpkg}\n    if [ -x \
         \"$__atpkg\" ]; then exec \"$__atpkg\" __selfupdate 'claude' {prefix} -- \"$@\"; fi\n    \
         ;;\nesac\n\
         # atpkg agents twin: while a newer build of this program is landing, `atpkg __landing` \
         waits for it and then runs the new one (aterm help pkg).\n\
         if [ -f {marker} ]; then\n  __atpkg={atpkg}\n  if [ -x \"$__atpkg\" ]; then exec \
         \"$__atpkg\" __landing 'claude' {prefix} -- \"$@\"; fi\nfi\n\
         exec {store} \"$@\"\n",
        atpkg = q(atpkg()),
        prefix = q(&fx.prefix),
        marker = q(&marker),
        store = q(&store),
    );
    std::fs::write(&twin, body).unwrap();
    std::fs::set_permissions(&twin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let argv = [OsString::from("--probe")];
    let out = fx.run(&twin, &argv);
    assert_eq!(out.status.code(), Some(7), "{out:?}");
    // The forward execs today's `bin/claude`, whose yolo-by-default block leads
    // the argv and says so (`crate::yolo` in atpkg's lib); the passthrough
    // itself — at once, no wait, no other word — is what this pins.
    assert_eq!(
        text(&out.stdout),
        "fake: --dangerously-skip-permissions --probe\n"
    );
    assert_eq!(
        text(&out.stderr),
        "aterm: yolo (default): running 'claude' '--dangerously-skip-permissions' \
         '--probe'\n"
    );
}

/// Every pass sweeps `landing/`: a mutating verb takes the store lock and removes the
/// directory — a live writer's marker in the old format and garbage alike, since no pass
/// of this client writes one and the lock holder is the only pass.
#[test]
fn a_pass_sweeps_every_landing_marker() {
    let fx = Fixture::new("sweep");
    fx.install_claude(2026091601);
    let marker = fx.stand_marker();
    std::fs::write(fx.layout().landing_dir().join("codex"), b"junk\n").unwrap();
    let out = fx.run(atpkg(), &[OsString::from("gc")]);
    assert!(out.status.success(), "{out:?}");
    assert!(!marker.exists(), "{out:?}");
    assert!(
        std::fs::symlink_metadata(fx.layout().landing_dir()).is_err(),
        "landing/ is gone"
    );
}
