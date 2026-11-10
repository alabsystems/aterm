// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

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
        self.run_twin(args, env, false)
    }

    /// [`Self::run`] with the token block laid too where `token` (a twin that is no dev link).
    fn run_twin(&self, args: &[&str], env: &[(&str, &str)], token: bool) -> String {
        let mut prelude = crate::platform::twin_prelude(
            "claude",
            &self.root.join("pkg"),
            None,
            &[],
            &crate::yolo::policy_of("claude"),
        );
        prelude.push_str(&sh_prelude("claude", Some(&self.atpkg)));
        let twin = self.root.join("claude");
        let body = crate::platform::sh_shim_content_twin(
            &self.root.join("store-build"),
            &crate::shim_env::ShimEnv::NONE,
            None,
            &prelude,
        );
        std::fs::write(
            &twin,
            if token {
                with_token_block("claude", body, Some(&self.atpkg))
            } else {
                body
            },
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

/// THE RENDERER BLOCK, EXECUTED: a launch in an aterm tab that names no renderer runs with
/// `CLAUDE_CODE_NO_FLICKER=1` (Claude Code's fullscreen renderer from its first start); one that
/// names a renderer in its environment (any value, empty included) or by a `"tui":` key in the
/// person's own `settings.json` (`$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`), one outside a tab,
/// and one under `--no-harness` keep what they had. NEGATIVE CONTROL: the plain twin without the
/// block (`sh_prelude` left out) leaves the variable unset in the same tab.
#[cfg(unix)]
#[test]
fn a_tab_launch_that_names_no_renderer_runs_fullscreen() {
    let laid = Laid::new();
    laid.aterm(":");
    let home = laid.root.join("home");
    let config = laid.root.join("config");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    exe(
        &laid.root.join("env-build"),
        "#!/bin/sh\nprintf '%s|%s\\n' \"${CLAUDE_CODE_NO_FLICKER-unset}\" \
         \"${CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN-unset}\"\n",
    );
    let twin = |with_block: bool| {
        let prelude = if with_block {
            sh_prelude("claude", Some(&laid.atpkg))
        } else {
            String::new()
        };
        let path = laid.root.join(if with_block { "claude" } else { "plain" });
        std::fs::write(
            &path,
            crate::platform::sh_shim_content_twin(
                &laid.root.join("env-build"),
                &crate::shim_env::ShimEnv::NONE,
                None,
                &prelude,
            ),
        )
        .unwrap();
        set_exec(&path);
        path
    };
    let run = |path: &Path, env: &[(&str, &str)]| {
        let out = aterm_spec::wait::past_text_busy(|| {
            let mut command = std::process::Command::new(path);
            command
                .env("HOME", &home)
                .env_remove("CLAUDE_CONFIG_DIR")
                .env_remove("CLAUDE_CODE_NO_FLICKER")
                .env_remove("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN")
                .env_remove("ATERM_PARENT_SESSION_ID")
                .env_remove("ATERM_NO_HARNESS");
            for (key, value) in env {
                command.env(key, value);
            }
            command.output()
        })
        .unwrap();
        assert!(out.status.success(), "{env:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let with = twin(true);
    let tab = ("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013");
    assert_eq!(run(&with, &[tab]), "1|unset\n", "a tab launch naming none");
    assert_eq!(
        run(&twin(false), &[tab]),
        "unset|unset\n",
        "negative control: no block, no variable"
    );
    assert_eq!(run(&with, &[]), "unset|unset\n", "outside a tab");
    assert_eq!(
        run(&with, &[tab, ("ATERM_NO_HARNESS", "1")]),
        "unset|unset\n",
        "under --no-harness"
    );
    for (env, want) in [
        (("CLAUDE_CODE_NO_FLICKER", "0"), "0|unset\n"),
        (("CLAUDE_CODE_NO_FLICKER", ""), "|unset\n"),
        (("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN", "1"), "unset|1\n"),
    ] {
        assert_eq!(run(&with, &[tab, env]), want, "{env:?}");
    }
    let settings = home.join(".claude/settings.json");
    std::fs::write(&settings, "{\n  \"theme\": \"auto\"\n}\n").unwrap();
    assert_eq!(run(&with, &[tab]), "1|unset\n", "settings naming no tui");
    std::fs::write(&settings, "{\"theme\":\"auto\",\"tui\" : \"default\"}").unwrap();
    assert_eq!(run(&with, &[tab]), "unset|unset\n", "the person's /tui");
    let config_str = config.to_string_lossy().into_owned();
    assert_eq!(
        run(&with, &[tab, ("CLAUDE_CONFIG_DIR", &config_str)]),
        "1|unset\n",
        "CLAUDE_CONFIG_DIR's settings, not HOME's, are the person's"
    );
    std::fs::write(config.join("settings.json"), "{\"tui\":\"fullscreen\"}").unwrap();
    assert_eq!(
        run(&with, &[tab, ("CLAUDE_CONFIG_DIR", &config_str)]),
        "unset|unset\n",
        "a tui under CLAUDE_CONFIG_DIR"
    );
}

/// THE TOKEN BLOCK (DESIGN-claude-setup-token S8, §5.4): right before the twin's last line, the
/// plain exec, which still names the build; absent for another program
/// and where atpkg names no file.
#[cfg(unix)]
#[test]
fn the_token_block_sits_before_the_last_line_of_a_claude_twin() {
    let body = crate::platform::sh_shim_content_twin(
        Path::new("/store/claude/7/bin/claude"),
        &crate::shim_env::ShimEnv::NONE,
        None,
        &sh_prelude("claude", Some(Path::new("/A/MacOS/atpkg"))),
    );
    let atpkg = Some(Path::new("/A/MacOS/atpkg"));
    let with = with_token_block("claude", body.clone(), atpkg);
    let lines: Vec<&str> = with.lines().collect();
    assert_eq!(
        lines.last().copied(),
        body.lines().last(),
        "the last line stays the plain exec"
    );
    assert_eq!(
        crate::platform::sh_exec_target(lines.last().unwrap().trim()),
        Some(PathBuf::from("/store/claude/7/bin/claude"))
    );
    let block = lines.len() - 5;
    assert!(
        lines[block].starts_with("# atpkg: aterm's token route"),
        "{with}"
    );
    assert_eq!(
        lines[block + 2],
        "  exec '/A/MacOS/aterm' harness accounts token-run --auto -- \
         '/store/claude/7/bin/claude' \"$@\"",
        "{with}"
    );
    assert_eq!(lines[block + 3], "fi");
    assert!(
        with.find("token-run") > with.find("harness tap --launch"),
        "after the tap's block"
    );
    assert_eq!(with_token_block("codex", body.clone(), atpkg), body);
    assert_eq!(with_token_block("claude", body.clone(), None), body);
    let not_plain = format!("{body}echo trailing\n");
    assert_eq!(
        with_token_block("claude", not_plain.clone(), atpkg),
        not_plain,
        "a last line that is not the plain exec"
    );
}

/// THE TWIN WITH THE TOKEN BLOCK, EXECUTED: inside a tab the launch runs as the aterm beside
/// atpkg's `harness accounts token-run --auto -- <build> <args…>`, after the usage tap's block;
/// outside a tab, under `--no-harness` and with that aterm missing, it runs exactly as typed.
#[cfg(unix)]
#[test]
fn the_twin_hands_a_tab_launch_to_the_token_wrapper_and_otherwise_runs_it_as_typed() {
    let laid = Laid::new();
    let tab = [("ATERM_PARENT_SESSION_ID", "s-60776b6e2ed1b73bd013")];
    // The stand-in aterm prints no tap value, and prints the wrapper's words.
    laid.aterm("case \"$2\" in tap) exit 0;; esac\nprintf '[%s]' \"$@\"\nprintf '\\n'");
    let typed = "<--model><opus>\n";
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &tab, true),
        format!(
            "[harness][accounts][token-run][--auto][--][{}][--model][opus]\n",
            laid.root.join("store-build").display()
        )
    );
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &[], true),
        typed,
        "outside a tab"
    );
    assert_eq!(
        laid.run_twin(
            &["--model", "opus"],
            &[tab[0], ("ATERM_NO_HARNESS", "1")],
            true
        ),
        typed,
        "under --no-harness"
    );
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &tab, false),
        typed,
        "the switch off: no block"
    );
    std::fs::remove_file(aterm_beside(&laid.atpkg).unwrap()).unwrap();
    assert_eq!(
        laid.run_twin(&["--model", "opus"], &tab, true),
        typed,
        "no aterm there"
    );
}
