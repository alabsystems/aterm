// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! INSIDE ATERM, THE SHELL HOOK PUTS THE REROUTE DIRECTORY FIRST (owner ruling
//! 2026-09-27: *"yes, in aterm hosted shells. do not block cargo, but do wrap it so that a
//! message is printed to tell the AI and users."*).
//!
//! The stubs already announce and then run upstream (`atpkg::reroute`, "announce, do not
//! prevent"). What failed was their PLACE on PATH: measured 2026-09-24 in a TTY `aterm`
//! login shell, `/etc/zprofile`'s `path_helper` and a `.zshrc` that prepends
//! `~/.local/bin` left `<prefix>/reroute` at position 12-14, behind `/usr/local/bin` and
//! `/opt/homebrew/bin`, so a bare `cargo` ran Homebrew's cargo with no message — and that
//! lane carries no shell integration to re-assert it. These tests source the REAL
//! generated hook (`atpkg::hooks::hook_files`) in the real shells installed here, from a
//! clean environment whose PATH has that measured shape, and then run the REAL generated
//! `cargo` stub against the dev `atpkg` binary and a FAKE upstream cargo (never a real
//! one: the fake prints its arguments and exits 3).
//!
//! NEVER THE REAL STORE: `HOME` and `XDG_CONFIG_HOME` are temp directories, so the prefix
//! the stub's `atpkg __reroute` resolves is the default one under that temp `HOME`, and the
//! `[reroute] announce` setting it reads is the temp `aterm.toml`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use atpkg::reroute::{PASSTHROUGH_ENV, REROUTE_DIR_ENV};

/// The fake upstream cargo's exit code: not 0, not the reroute's own 2 or 127, so a
/// preserved 3 can only be the fake's.
const FAKE_EXIT: i32 = 3;

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    config_home: PathBuf,
    reroute: PathBuf,
    agents: PathBuf,
    bin: PathBuf,
    /// The fake `/usr/local/bin`: ahead of everything, holding no `cargo`.
    usr_local: PathBuf,
    /// The fake `/opt/homebrew/bin`: holds the fake upstream `cargo`.
    homebrew: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("atpkg-hook-reroute-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let config_home = root.join("config");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&config_home).unwrap();
        let layout = atpkg::store::Layout {
            prefix: atpkg::store::default_prefix(&home),
        };
        assert!(
            layout.prefix.starts_with(&home),
            "{}",
            layout.prefix.display()
        );
        layout.ensure_dir(&layout.prefix).unwrap();
        let (reroute, agents, bin) = (layout.reroute_dir(), layout.agents_dir(), layout.bin_dir());
        for dir in [&reroute, &agents, &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // The reroute directory as `reroute::lay` leaves it: the marker file, and the real
        // `cargo` stub, naming the dev atpkg binary this test suite was built with.
        std::fs::write(
            reroute.join(atpkg::reroute::DIR_MARKER_FILE),
            format!("{}\n", atpkg::reroute::STUB_MARKER),
        )
        .unwrap();
        write_executable(
            &reroute.join("cargo"),
            &atpkg::reroute::stub_body_sh(
                "cargo",
                Path::new(env!("CARGO_BIN_EXE_atpkg")),
                &reroute,
            ),
        );
        let usr_local = root.join("fake/usr/local/bin");
        let homebrew = root.join("fake/opt/homebrew/bin");
        std::fs::create_dir_all(&usr_local).unwrap();
        std::fs::create_dir_all(&homebrew).unwrap();
        write_executable(
            &homebrew.join("cargo"),
            &format!("#!/bin/sh\necho \"homebrew-style cargo: $*\"\nexit {FAKE_EXIT}\n"),
        );
        Self {
            root,
            home,
            config_home,
            reroute,
            agents,
            bin,
            usr_local,
            homebrew,
        }
    }

    fn s(p: &Path) -> &str {
        p.to_str().expect("UTF-8 fixture path")
    }

    /// The measured login-shell shape: `~/.local/bin` (the .zshrc prepend), then
    /// path_helper's `/usr/local/bin` and `/opt/homebrew/bin`, the system dirs, and the
    /// spawn seam's `reroute`/`agents` front-insert DEMOTED behind all of them — the
    /// reroute dir listed twice, as a nested seam leaves it.
    fn inherited_path(&self) -> String {
        let local_bin = self.home.join(".local/bin");
        format!(
            "{}:{}:{}:/usr/bin:/bin:{}:{}:{}",
            Self::s(&local_bin),
            Self::s(&self.usr_local),
            Self::s(&self.homebrew),
            Self::s(&self.reroute),
            Self::s(&self.agents),
            Self::s(&self.reroute),
        )
    }

    fn hook(&self, ext: &str) -> String {
        atpkg::hooks::hook_files(&self.bin, &self.agents, &self.reroute)
            .into_iter()
            .find(|(name, _)| name.ends_with(&format!(".{ext}")))
            .map(|(_, body)| body)
            .unwrap_or_else(|| panic!("hook_files writes no .{ext} hook"))
    }

    /// The hook for `ext`, laid where atpkg lays it.
    fn lay_hook(&self, ext: &str) -> PathBuf {
        let shell_d = self.home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        let path = shell_d.join(format!("{}.{ext}", atpkg::hooks::HOOK_BASENAME));
        std::fs::write(&path, self.hook(ext)).unwrap();
        path
    }

    /// `[reroute] announce = false` in the temp `aterm.toml` the stub's atpkg reads.
    fn silence_announcements(&self) {
        let dir = self.config_home.join("aterm");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("aterm.toml"), "[reroute]\nannounce = false\n").unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A POSIX shell the test can start, with the flags that keep every startup file out.
struct Shell {
    name: &'static str,
    ext: &'static str,
    exe: PathBuf,
    args: &'static [&'static str],
}

/// zsh and bash at their system paths — the shells a macOS login gives — and fish when
/// it is installed; only a shell that is not there is skipped, and bash never is.
fn posix_shells() -> Vec<Shell> {
    let mut shells = Vec::new();
    for (name, ext, args, candidates) in [
        (
            "bash",
            "bash",
            &["--noprofile", "--norc", "-c"][..],
            &["/bin/bash", "/usr/bin/bash"][..],
        ),
        (
            "zsh",
            "zsh",
            &["-f", "-c"][..],
            &["/bin/zsh", "/usr/bin/zsh"][..],
        ),
    ] {
        match candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
            Some(exe) => shells.push(Shell {
                name,
                ext,
                exe,
                args,
            }),
            None => {
                assert_ne!(name, "bash", "bash must be runnable on a unix host");
                eprintln!("{name} is not installed here; its dialect is text-checked only");
            }
        }
    }
    shells
}

/// fish, when it is installed on this host's PATH.
fn fish() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("fish"))
        .find(|p| p.is_file())
}

/// Run `script` in `shell` from a CLEAN environment: only `HOME`, `XDG_CONFIG_HOME`,
/// `PATH`, `ATPKG_TEST_HOOK` and the `env` pairs given.
fn run(
    fx: &Fixture,
    exe: &Path,
    args: &[&str],
    script: &str,
    hook: &Path,
    path: &str,
    env: &[(&str, &str)],
) -> Output {
    let mut cmd = Command::new(exe);
    // The fixture's own home as the working directory: a signpost names the channel a
    // `rust-toolchain.toml` up the tree pins (`atpkg::reroute::Pin`), so a shell left in
    // the checkout would read whatever the checkout pins.
    cmd.args(args)
        .arg(script)
        .current_dir(&fx.home)
        .env_clear()
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.config_home)
        .env("PATH", path)
        .env("ATPKG_TEST_HOOK", hook);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()
        .unwrap_or_else(|error| panic!("{}: spawn: {error}", exe.display()))
}

/// The PATH a shell is left with after sourcing the hook `times` times.
fn sourced_path(
    fx: &Fixture,
    shell: &Shell,
    hook: &Path,
    times: usize,
    env: &[(&str, &str)],
) -> Vec<String> {
    let script = format!(
        "{}printf 'PATH=%s\\n' \"$PATH\"",
        ". \"$ATPKG_TEST_HOOK\"; ".repeat(times)
    );
    let out = run(
        fx,
        &shell.exe,
        shell.args,
        &script,
        hook,
        &fx.inherited_path(),
        env,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}: exit {:?}; stdout={stdout:?} stderr={:?}",
        shell.name,
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .trim_end()
        .strip_prefix("PATH=")
        .unwrap_or_else(|| panic!("{}: no PATH line: {stdout:?}", shell.name))
        .split(':')
        .map(str::to_owned)
        .collect()
}

fn count(path: &[String], dir: &Path) -> usize {
    path.iter().filter(|e| Path::new(e) == dir).count()
}

/// THE ORDER, in every POSIX shell installed here: inside aterm (any one marker of the
/// agents gate) PATH starts `reroute:agents:`, each exactly once, ahead of the fake
/// `/usr/local/bin` and `/opt/homebrew/bin`; outside aterm neither is on PATH at all; under
/// `aterm --no-reroute`'s marker agents still leads and reroute is not added; and a
/// second source changes nothing.
#[test]
fn inside_aterm_the_hook_puts_reroute_then_agents_first_and_outside_neither() {
    let fx = Fixture::new("order");
    let rest = |path: &[String]| -> Vec<String> {
        path.iter()
            .filter(|e| Path::new(e) != fx.reroute && Path::new(e) != fx.agents)
            .cloned()
            .collect()
    };
    let inherited: Vec<String> = fx.inherited_path().split(':').map(str::to_owned).collect();
    for shell in posix_shells() {
        let hook = fx.lay_hook(shell.ext);
        let who = shell.name;
        for &marker in atpkg::hooks::AGENTS_MARKERS {
            let inside = sourced_path(&fx, &shell, &hook, 1, &[(marker, "1")]);
            assert_eq!(
                inside[..2],
                [Fixture::s(&fx.reroute), Fixture::s(&fx.agents)],
                "{who} with {marker}: PATH starts reroute:agents — {inside:?}"
            );
            assert_eq!(count(&inside, &fx.reroute), 1, "{who}: {inside:?}");
            assert_eq!(count(&inside, &fx.agents), 1, "{who}: {inside:?}");
            // Everything else in its order, bin/ appended last (the unconditional half).
            let mut want = rest(&inherited);
            want.push(Fixture::s(&fx.bin).to_owned());
            assert_eq!(rest(&inside), want, "{who} with {marker}: {inside:?}");
            assert_eq!(
                sourced_path(&fx, &shell, &hook, 2, &[(marker, "1")]),
                inside,
                "{who} with {marker}: sourcing twice is sourcing once"
            );
        }
        // The passthrough marker's "not engaged" spellings ("" and "0") still add it.
        for value in ["", "0"] {
            let inside = sourced_path(
                &fx,
                &shell,
                &hook,
                1,
                &[("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, value)],
            );
            assert_eq!(
                inside[..2],
                [Fixture::s(&fx.reroute), Fixture::s(&fx.agents)],
                "{who}: a passthrough marker of {value:?} is not engaged — {inside:?}"
            );
        }
        // OUTSIDE ATERM: neither, and bin/ still appended (an iTerm launched from an aterm
        // tab inherits both, and heals).
        let outside = sourced_path(&fx, &shell, &hook, 1, &[]);
        assert_eq!(count(&outside, &fx.reroute), 0, "{who}: {outside:?}");
        assert_eq!(count(&outside, &fx.agents), 0, "{who}: {outside:?}");
        assert_eq!(
            outside.last().map(String::as_str),
            Some(Fixture::s(&fx.bin))
        );
        assert_eq!(
            sourced_path(&fx, &shell, &hook, 2, &[]),
            outside,
            "{who}: idempotent outside aterm too"
        );
        // `aterm --no-reroute`: that session asked for every upstream tool.
        let passthrough = [("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, "1")];
        let escaped = sourced_path(&fx, &shell, &hook, 1, &passthrough);
        assert_eq!(
            escaped[0],
            Fixture::s(&fx.agents),
            "{who}: the managed agents still lead under --no-reroute — {escaped:?}"
        );
        assert_eq!(count(&escaped, &fx.reroute), 0, "{who}: {escaped:?}");
        assert_eq!(count(&escaped, &fx.agents), 1, "{who}: {escaped:?}");
        assert_eq!(
            sourced_path(&fx, &shell, &hook, 2, &passthrough),
            escaped,
            "{who}: idempotent under the passthrough marker"
        );
    }
}

/// THE DEGRADED TTY LAUNCH (review finding, 2026-09-27). A TTY `aterm` session carries
/// `ATERM_AGENTS_DIR` and neither `ATERM_CHILD` nor `ATERM_SESSION_ID`; when its front door
/// could hand no agents dir (a file or a link at `agents/`, a refused `mkdir`: one stderr
/// line, and the handle is blank or absent) the agents markers are all empty — yet the
/// session is still an aterm-hosted shell, its seam still front-inserted the reroute dir and
/// still exports `$ATERM_REROUTE_DIR` for it. The hook used to read that as OUTSIDE aterm and
/// take the reroute dir out, so a bare `cargo` ran the fake upstream with no message. Now
/// `$ATERM_REROUTE_DIR` (non-empty) counts for the reroute half — never for agents/, which
/// stays out with `ATPKG_AGENTS` unset — and the passthrough marker still wins.
#[test]
fn a_tty_session_whose_front_door_handed_no_agents_dir_still_leads_with_reroute() {
    let fx = Fixture::new("no-agents-handoff");
    let reroute = Fixture::s(&fx.reroute).to_owned();
    let script = ". \"$ATPKG_TEST_HOOK\"; . \"$ATPKG_TEST_HOOK\"; \
                  printf 'PATH=%s\\nAGENTS=%s\\n' \"$PATH\" \"${ATPKG_AGENTS-<unset>}\"";
    let upstream_ran = "homebrew-style cargo: build --x\n";
    for shell in posix_shells() {
        let hook = fx.lay_hook(shell.ext);
        let who = shell.name;
        let probe = |env: &[(&str, &str)]| -> (Vec<String>, String) {
            let out = run(
                &fx,
                &shell.exe,
                shell.args,
                script,
                &hook,
                &fx.inherited_path(),
                env,
            );
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(out.status.success(), "{who}: {out:?}");
            let mut lines = stdout.lines();
            let path = lines
                .next()
                .and_then(|l| l.strip_prefix("PATH="))
                .unwrap_or_else(|| panic!("{who}: no PATH line: {stdout:?}"))
                .split(':')
                .map(str::to_owned)
                .collect();
            let agents = lines
                .next()
                .and_then(|l| l.strip_prefix("AGENTS="))
                .unwrap_or_else(|| panic!("{who}: no AGENTS line: {stdout:?}"))
                .to_owned();
            (path, agents)
        };
        // Blank and absent handoff alike: the TTY lane blanks an inherited one.
        for degraded in [
            &[
                ("ATERM_REROUTE_DIR", reroute.as_str()),
                ("ATERM_AGENTS_DIR", ""),
            ][..],
            &[("ATERM_REROUTE_DIR", reroute.as_str())][..],
        ] {
            let (path, agents) = probe(degraded);
            assert_eq!(
                path[0], reroute,
                "{who} {degraded:?}: the reroute dir leads — {path:?}"
            );
            assert_eq!(count(&path, &fx.reroute), 1, "{who}: {path:?}");
            assert_eq!(
                count(&path, &fx.agents),
                0,
                "{who} {degraded:?}: no agents marker, so agents/ stays out — {path:?}"
            );
            assert_eq!(agents, "<unset>", "{who} {degraded:?}: ATPKG_AGENTS unset");
            assert_eq!(
                path.last().map(String::as_str),
                Some(Fixture::s(&fx.bin)),
                "{who}: bin/ still appended"
            );
        }
        // A BLANK `$ATERM_REROUTE_DIR` is not a marker — what every lane leaves under the
        // escape or with nothing laid — and the passthrough marker wins over a set one.
        for no_reroute in [
            &[("ATERM_REROUTE_DIR", "")][..],
            &[
                ("ATERM_REROUTE_DIR", reroute.as_str()),
                (PASSTHROUGH_ENV, "1"),
            ][..],
        ] {
            let (path, agents) = probe(no_reroute);
            assert_eq!(
                count(&path, &fx.reroute) + count(&path, &fx.agents),
                0,
                "{who} {no_reroute:?}: neither dir — {path:?}"
            );
            assert_eq!(agents, "<unset>", "{who} {no_reroute:?}");
        }
        // End to end: the degraded session's bare `cargo` meets the signpost first.
        let out = run(
            &fx,
            &shell.exe,
            shell.args,
            ". \"$ATPKG_TEST_HOOK\"; cargo build --x",
            &hook,
            &fx.inherited_path(),
            &[
                ("ATERM_REROUTE_DIR", reroute.as_str()),
                ("ATERM_AGENTS_DIR", ""),
            ],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("targo --unverified build --x"),
            "{who}: the degraded TTY session announces a bare cargo — stderr={stderr:?}"
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), upstream_ran, "{who}");
        assert_eq!(out.status.code(), Some(FAKE_EXIT), "{who}");
    }
}

/// THE MESSAGE, END TO END. Inside aterm, after the hook, a bare `cargo build --x` in
/// every POSIX shell installed here reaches the reroute stub ahead of the fake Homebrew
/// cargo: stderr carries the signpost naming `targo --unverified build --x` and
/// `targo trust build --x`, the fake then runs with the same arguments, and its exit code
/// is the command's. `[reroute] announce = false` drops the line and nothing else. The
/// negative control: outside aterm the same command runs the fake SILENTLY — the message
/// comes from the order the hook sets, not from the fake or the fixture.
#[test]
fn inside_aterm_a_bare_cargo_is_announced_and_then_runs_upstream_with_its_exit_code() {
    let fx = Fixture::new("stub");
    let args: Vec<String> = ["build", "--x"].map(String::from).to_vec();
    let row = atpkg::reroute::row_for("cargo").expect("cargo is rerouted");
    let atpkg::reroute::Policy::Signpost { lanes, .. } = row.policy else {
        panic!("cargo is a SIGNPOST row: {row:?}");
    };
    let signpost = atpkg::reroute::signpost_message(row, lanes, &args, None);
    for lane in ["targo --unverified build --x", "targo trust build --x"] {
        assert!(signpost.contains(lane), "{signpost}");
    }
    let script = ". \"$ATPKG_TEST_HOOK\"; cargo build --x";
    let upstream_ran = "homebrew-style cargo: build --x\n";
    for shell in posix_shells() {
        let hook = fx.lay_hook(shell.ext);
        let who = shell.name;
        let cargo = |env: &[(&str, &str)]| -> (String, String, Option<i32>) {
            let out = run(
                &fx,
                &shell.exe,
                shell.args,
                script,
                &hook,
                &fx.inherited_path(),
                env,
            );
            (
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
                out.status.code(),
            )
        };
        let (stdout, stderr, code) = cargo(&[("ATERM_CHILD", "1")]);
        assert!(
            stderr.contains(&signpost),
            "{who}: inside aterm a bare cargo is announced — stderr={stderr:?} stdout={stdout:?}"
        );
        assert_eq!(
            stdout, upstream_ran,
            "{who}: upstream ran with the same args"
        );
        assert_eq!(code, Some(FAKE_EXIT), "{who}: upstream's exit code is kept");
        // Negative control: outside aterm the hook takes the reroute dir out, so the same
        // command meets the fake first — no line, same run.
        let (stdout, stderr, code) = cargo(&[]);
        assert_eq!(stderr, "", "{who}: outside aterm nothing is announced");
        assert_eq!(stdout, upstream_ran, "{who}");
        assert_eq!(code, Some(FAKE_EXIT), "{who}");
    }
    // `[reroute] announce = false`: the line is gone, the run is not.
    fx.silence_announcements();
    for shell in posix_shells() {
        let hook = fx.lay_hook(shell.ext);
        let out = run(
            &fx,
            &shell.exe,
            shell.args,
            script,
            &hook,
            &fx.inherited_path(),
            &[("ATERM_CHILD", "1")],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr, "",
            "{}: announce = false silences the line",
            shell.name
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), upstream_ran);
        assert_eq!(out.status.code(), Some(FAKE_EXIT), "{}", shell.name);
    }
}

/// fish and PowerShell. fish is sourced for real where it is installed (it is not on a
/// stock macOS box); the `.ps1` is text-checked — no pwsh is executed here. Both carry
/// the same order: the bin half first, then the gated arm prepending reroute ahead of
/// agents unless the passthrough marker is engaged.
#[test]
fn fish_and_powershell_lead_reroute_then_agents_inside_aterm() {
    let fx = Fixture::new("fish-ps1");
    let (r, a, b) = (
        Fixture::s(&fx.reroute),
        Fixture::s(&fx.agents),
        Fixture::s(&fx.bin),
    );
    let fish_body = fx.hook("fish");
    for needle in [
        format!("set -l __atpkg_reroute \"{r}\""),
        "set -l __atpkg_front\n".to_owned(),
        "set __atpkg_front \"$__atpkg_agents\"; set -gx ATPKG_AGENTS $__atpkg_agents; \
         else; set -e ATPKG_AGENTS; end"
            .to_owned(),
        // The reroute half: any agents marker OR a non-empty `$ATERM_REROUTE_DIR` (the
        // degraded TTY launch), and never under the passthrough marker.
        format!(
            "if test -z \"${PASSTHROUGH_ENV}\"; or test \"${PASSTHROUGH_ENV}\" = 0; \
             if test -n \"$ATERM_AGENTS_DIR$ATERM_CHILD$ATERM_SESSION_ID${REROUTE_DIR_ENV}\"; \
             set __atpkg_front \"$__atpkg_reroute\" $__atpkg_front; end; end"
        ),
        "and test \"$__atpkg_d\" != \"$__atpkg_reroute\"".to_owned(),
        "set -gx PATH $__atpkg_front $__atpkg_rest\n".to_owned(),
    ] {
        assert!(
            fish_body.contains(&needle),
            "fish: {needle:?} in {fish_body}"
        );
    }
    assert!(
        fish_body.find("__atpkg_bin").unwrap() < fish_body.find("__atpkg_reroute").unwrap(),
        "fish: the bin half is emitted first"
    );
    let ps = fx.hook("ps1");
    for needle in [
        format!("$__atpkg_reroute = '{r}'"),
        "Where-Object { $_ -ne $__atpkg_agents -and $_ -ne $__atpkg_reroute }".to_owned(),
        "$__atpkg_front = @()\n".to_owned(),
        "{ $__atpkg_front = @($__atpkg_agents); $env:ATPKG_AGENTS = $__atpkg_agents } else"
            .to_owned(),
        format!(
            "if (($env:ATERM_AGENTS_DIR -or $env:ATERM_CHILD -or $env:ATERM_SESSION_ID -or \
             $env:{REROUTE_DIR_ENV}) -and (-not $env:{PASSTHROUGH_ENV} -or \
             $env:{PASSTHROUGH_ENV} -eq '0')) {{ $__atpkg_front = @($__atpkg_reroute) + $__atpkg_front }}"
        ),
        "$env:PATH = ($__atpkg_front + $__atpkg_rest) -join $__atpkg_sep\n".to_owned(),
        "Remove-Variable __atpkg_agents, __atpkg_reroute, __atpkg_front,".to_owned(),
    ] {
        assert!(ps.contains(&needle), "ps1: {needle:?} in {ps}");
    }
    assert!(
        ps.find("__atpkg_bin").unwrap() < ps.find("__atpkg_reroute").unwrap(),
        "ps1: the bin half is emitted first"
    );
    let Some(fish) = fish() else {
        eprintln!("fish is not installed here; the fish dialect is text-checked only");
        return;
    };
    let hook = fx.lay_hook("fish");
    let fish_path = |env: &[(&str, &str)]| -> Vec<String> {
        let out = run(
            &fx,
            &fish,
            &["--no-config", "-c"],
            "source $ATPKG_TEST_HOOK; source $ATPKG_TEST_HOOK; printf 'PATH=%s\\n' (string join : $PATH)",
            &hook,
            &fx.inherited_path(),
            env,
        );
        assert!(out.status.success(), "fish: {out:?}");
        String::from_utf8_lossy(&out.stdout)
            .trim_end()
            .strip_prefix("PATH=")
            .unwrap_or_else(|| panic!("fish: no PATH line: {out:?}"))
            .split(':')
            .map(str::to_owned)
            .collect()
    };
    let inside = fish_path(&[("ATERM_CHILD", "1")]);
    assert_eq!(inside[..2], [r, a], "fish: {inside:?}");
    assert_eq!(count(&inside, &fx.reroute), 1, "fish: {inside:?}");
    assert_eq!(
        inside.last().map(String::as_str),
        Some(b),
        "fish: {inside:?}"
    );
    let outside = fish_path(&[]);
    assert_eq!(
        count(&outside, &fx.reroute) + count(&outside, &fx.agents),
        0
    );
    let escaped = fish_path(&[("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, "1")]);
    assert_eq!(escaped[0], a, "fish: {escaped:?}");
    assert_eq!(count(&escaped, &fx.reroute), 0, "fish: {escaped:?}");
}
