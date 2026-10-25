// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::{Path, PathBuf};

use super::*;

/// Only Claude Code's twin carries the block, only where an atpkg is named, and never on Windows,
/// whose `.cmd` twin carries no prelude.
#[test]
fn only_claudes_twin_carries_the_block() {
    let atpkg = Path::new("/Applications/aterm.app/Contents/MacOS/atpkg");
    assert!(of("claude"));
    for other in ["codex", "agy", "muse", "gemini", "codex-old", "claudex"] {
        assert!(!of(other), "{other}");
        assert_eq!(sh_prelude(other, Some(atpkg)), "", "{other}");
    }
    assert_eq!(sh_prelude("claude", None), "");
    assert_eq!(sh_prelude("claude", Some(Path::new(""))), "");
    assert_eq!(
        aterm_beside(atpkg).as_deref(),
        Some(Path::new(if cfg!(windows) {
            "/Applications/aterm.app/Contents/MacOS/aterm.exe"
        } else {
            "/Applications/aterm.app/Contents/MacOS/aterm"
        }))
    );
    if cfg!(windows) {
        assert_eq!(sh_prelude("claude", Some(atpkg)), "");
    }
}

/// The block names the aterm beside atpkg, quoted for `sh`, asks it with the launch's own
/// arguments after aterm's verb, and puts what it printed ahead of them.
#[cfg(unix)]
#[test]
fn the_block_asks_the_aterm_beside_atpkg() {
    let block = sh_prelude("claude", Some(Path::new("/it's/MacOS/atpkg")));
    assert!(block.starts_with("# atpkg: aterm's usage tap"), "{block}");
    assert!(
        block.contains(
            "__atpkg_tap=$('/it'\\''s/MacOS/aterm' harness tap --launch -- \"$@\" </dev/null \
             2>/dev/null) || __atpkg_tap=\n"
        ),
        "{block}"
    );
    assert!(
        block.contains("then set -- --settings \"$__atpkg_tap\" \"$@\"; fi\n"),
        "{block}"
    );
    assert!(!block.contains("/it's/"), "{block}");
    assert!(
        !block.lines().any(|l| l.trim().starts_with("exec ")),
        "the shim's own exec stays the one exec: {block}"
    );
}

/// A scratch layout for the executed twins: a store build that prints its argv, and a stand-in
/// aterm beside a stand-in atpkg.
#[cfg(unix)]
struct Laid {
    root: crate::testdir::Dir,
    atpkg: PathBuf,
}

#[cfg(unix)]
impl Laid {
    fn new() -> Self {
        let root = crate::testdir::dir("atpkg-launch-tap-");
        let bundle = root.join("MacOS");
        std::fs::create_dir_all(&bundle).unwrap();
        let atpkg = bundle.join("atpkg");
        exe(&atpkg, "#!/bin/sh\nexit 2\n");
        exe(
            &root.join("store-build"),
            "#!/bin/sh\nprintf '<%s>' \"$@\"\nprintf '\\n'\n",
        );
        Self { root, atpkg }
    }

    /// Lay the stand-in aterm, which answers `harness tap --launch -- <args>` with `body` (its
    /// arguments are written to `asked` first).
    fn aterm(&self, body: &str) {
        exe(
            &aterm_beside(&self.atpkg).unwrap(),
            &format!(
                "#!/bin/sh\nprintf '<%s>' \"$@\" > '{}'\n{body}\n",
                self.asked().display()
            ),
        );
    }

    fn asked(&self) -> PathBuf {
        self.root.join("asked")
    }

    /// Lay Claude Code's twin as atpkg lays it (its `--yolo` block, then the usage tap's) and run
    /// it with `args` under `env` (every other variable of the usage tap's removed).
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> String {
        let mut prelude = crate::platform::twin_prelude(
            "claude",
            &self.root.join("pkg"),
            None,
            &[],
            &crate::yolo::policy_of("claude"),
        );
        prelude.push_str(&sh_prelude("claude", Some(&self.atpkg)));
        let twin = self.root.join("claude");
        std::fs::write(
            &twin,
            crate::platform::sh_shim_content_twin(
                &self.root.join("store-build"),
                &crate::shim_env::ShimEnv::NONE,
                None,
                &prelude,
            ),
        )
        .unwrap();
        set_exec(&twin);
        let out = aterm_spec::wait::past_text_busy(|| {
            let mut command = std::process::Command::new(&twin);
            command
                .args(args)
                .env_remove("ATERM_PARENT_SESSION_ID")
                .env_remove("ATERM_NO_HARNESS");
            for (key, value) in env {
                command.env(key, value);
            }
            command.output()
        })
        .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[cfg(unix)]
fn set_exec(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
fn exe(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    set_exec(path);
}

/// THE TWIN TAPS A CLAUDE CODE STARTED IN AN ATERM TAB FROM ITS START (owner ruling R-12: "THE
/// HARNESS MUST DO THIS STOP ASKING THE USER"): inside a tab, the value aterm printed rides as
/// `--settings <value>` ahead of exactly the arguments the twin would run without it (the `--yolo`
/// default's flag among them where it applies), one argument whatever it holds, and aterm was
/// asked with exactly those arguments; outside a tab, under `--no-harness`, where aterm printed
/// nothing, failed, or is not there, the launch runs exactly as typed.
#[cfg(unix)]
#[test]
fn the_twin_taps_a_launch_in_an_aterm_tab_and_otherwise_runs_it_as_typed() {
    let laid = Laid::new();
    let tab = [("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013")];
    let value = r#"{"statusLine":{"type":"command","command":"/x/aterm harness tap","refreshInterval":10}}"#;
    laid.aterm(&format!("printf '%s\\n' '{value}'"));
    for args in [
        &["-p", "hello"][..],
        &["--model", "opus"][..],
        &["--model", "opus", "fix it's bug"][..],
        &["-p", "two  spaces", "--resume", "x"][..],
    ] {
        let _ = std::fs::remove_file(laid.asked());
        let untapped = laid.run(args, &[]);
        assert!(
            !laid.asked().exists(),
            "aterm is not asked outside a tab: {args:?}"
        );
        assert_eq!(
            laid.run(args, &tab),
            format!("<--settings><{value}>{untapped}"),
            "{args:?}"
        );
        assert_eq!(
            std::fs::read_to_string(laid.asked()).unwrap(),
            format!(
                "<harness><tap><--launch><-->{}",
                untapped.trim_end_matches('\n')
            ),
            "{args:?}"
        );
    }
    assert_eq!(
        laid.run(&["-p", "hello"], &tab),
        format!("<--settings><{value}><--dangerously-skip-permissions><-p><hello>\n"),
        "the --yolo default's flag rides behind the tap's"
    );
    let typed = "<--model><opus>\n";
    assert_eq!(laid.run(&["--model", "opus"], &[]), typed);
    let _ = std::fs::remove_file(laid.asked());
    assert_eq!(
        laid.run(&["--model", "opus"], &[tab[0], ("ATERM_NO_HARNESS", "1")]),
        typed,
        "under --no-harness"
    );
    assert_eq!(
        laid.run(&["--no-harness", "--model", "opus"], &tab),
        typed,
        "the twin's own --no-harness"
    );
    assert!(
        !laid.asked().exists(),
        "aterm is not asked under --no-harness"
    );
    laid.aterm(":");
    assert_eq!(
        laid.run(&["--model", "opus"], &tab),
        typed,
        "nothing printed"
    );
    laid.aterm(&format!("printf '%s\\n' '{value}'; exit 1"));
    assert_eq!(laid.run(&["--model", "opus"], &tab), typed, "aterm failed");
    std::fs::remove_file(aterm_beside(&laid.atpkg).unwrap()).unwrap();
    assert_eq!(
        laid.run(&["--model", "opus"], &tab),
        typed,
        "no aterm there"
    );
}
