// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Inside aterm the shell hook puts the reroute directory first on PATH. The stubs announce
//! and then run upstream (`atpkg::reroute`), which works only from ahead of
//! `/usr/local/bin` and `/opt/homebrew/bin`: a login shell's `/etc/zprofile` `path_helper`
//! and a `.zshrc` that prepends `~/.local/bin` otherwise leave `<prefix>/reroute` behind
//! them, so a bare `cargo` would run Homebrew's with no message. These tests source the real
//! generated hook (`atpkg::hooks::hook_files`) in the real shells installed here, from a
//! clean environment whose PATH has that shape, then run the real generated `cargo` stub
//! against the dev `atpkg` binary and a fake upstream cargo (it prints its arguments and
//! exits 3).
//!
//! Never the real store: `HOME` and `XDG_CONFIG_HOME` are temp directories, so the prefix
//! the stub's `atpkg __reroute` resolves is the default one under that temp `HOME`, and the
//! `[reroute] announce` setting it reads is the temp `aterm.toml`.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use atpkg::reroute::{PASSTHROUGH_ENV, REROUTE_DIR_ENV};

/// The fake upstream cargo's exit code: not 0, not the reroute's own 2 or 127, so a
/// preserved 3 can only be the fake's.
const FAKE_EXIT: i32 = 3;

struct Fixture {
    _root: aterm_tempfile::TempDir,
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
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-hook-reroute-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        let config_home = root.path().join("config");
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
                false,
            ),
        );
        let usr_local = root.path().join("fake/usr/local/bin");
        let homebrew = root.path().join("fake/opt/homebrew/bin");
        std::fs::create_dir_all(&usr_local).unwrap();
        std::fs::create_dir_all(&homebrew).unwrap();
        write_executable(
            &homebrew.join("cargo"),
            &format!("#!/bin/sh\necho \"homebrew-style cargo: $*\"\nexit {FAKE_EXIT}\n"),
        );
        // Its first exec, paid here: a fresh file waits on `syspolicyd`'s assessment the
        // first time it runs, and the reroute's bounded `-vV` probe must not pay it.
        let warmed = Command::new(homebrew.join("cargo")).output().unwrap();
        assert_eq!(warmed.status.code(), Some(FAKE_EXIT), "{warmed:?}");
        Self {
            _root: root,
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

    /// The login-shell shape: `~/.local/bin` (the .zshrc prepend), then path_helper's
    /// `/usr/local/bin` and `/opt/homebrew/bin`, the system dirs, and the spawn seam's
    /// `reroute`/`agents` front-insert demoted behind all of them — the reroute dir listed
    /// twice, as a nested seam leaves it.
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

/// Run `script` in `shell` from a clean environment: only `HOME`, `XDG_CONFIG_HOME`,
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

/// In every POSIX shell installed here: inside aterm (any one marker of the agents gate)
/// PATH starts `reroute:agents:`, each exactly once, ahead of the fake `/usr/local/bin` and
/// `/opt/homebrew/bin`; outside aterm neither is on PATH at all; under `aterm
/// --no-reroute`'s marker agents still leads and reroute is not added; and a second source
/// changes nothing.
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
        // Outside aterm: neither, and bin/ still appended (an iTerm launched from an aterm
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

/// A degraded TTY launch: a TTY `aterm` session carries `ATERM_AGENTS_DIR` and neither
/// `ATERM_CHILD` nor `ATERM_SESSION_ID`; when its front door could hand no agents dir (a
/// file or a link at `agents/`, a refused `mkdir`) the agents markers are all empty, yet the
/// session is still aterm-hosted: its seam front-inserted the reroute dir and exports
/// `$ATERM_REROUTE_DIR`. A non-empty `$ATERM_REROUTE_DIR` counts for the reroute half —
/// never for agents/, which stays out with `ATPKG_AGENTS` unset — and the passthrough marker
/// still wins.
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
        // A blank `$ATERM_REROUTE_DIR` is not a marker — what every lane leaves under the
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

/// Inside aterm, after the hook, a bare `cargo build --x` in every POSIX shell installed
/// here reaches the reroute stub ahead of the fake Homebrew cargo: stderr carries the
/// signpost — naming what `cargo` resolved to (the fake answers `-vV` like no Rust tool, so
/// an unidentified cargo) and `targo --unverified build --x` and `targo trust build --x` —
/// the fake then runs with the same arguments, and its exit code is the command's; the
/// `-vV` probe never reaches stdout. `[reroute] announce = false` drops the line and
/// nothing else. Negative control: outside aterm the same command runs the fake silently.
#[test]
fn inside_aterm_a_bare_cargo_is_announced_and_then_runs_upstream_with_its_exit_code() {
    let fx = Fixture::new("stub");
    let args: Vec<String> = ["build", "--x"].map(String::from).to_vec();
    let row = atpkg::reroute::row_for("cargo").expect("cargo is rerouted");
    let atpkg::reroute::Policy::Signpost { lanes, .. } = row.policy else {
        panic!("cargo is a SIGNPOST row: {row:?}");
    };
    // The lanes and the closing lines, as `atpkg::reroute` renders them for these args: the
    // resolved first line and the file line come between them.
    let signpost = atpkg::reroute::signpost_message_in(row, lanes, &args, None, None, None);
    let (_, lanes_text) = signpost.split_once('\n').expect("a block");
    let (lane_lines, closing) = lanes_text
        .split_once("       `aterm help rust`")
        .expect("the closing lines");
    for lane in ["targo --unverified build --x", "targo trust build --x"] {
        assert!(lane_lines.contains(lane), "{signpost}");
    }
    let first_line = format!(
        "aterm: running 'cargo' -> an unidentified cargo ({} -vV exited {FAKE_EXIT}); not known to be Trust (no proof claim); on Trust the tool is 'targo':\n",
        fx.homebrew.join("cargo").display()
    );
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
            stderr.starts_with(&first_line)
                && stderr.contains(lane_lines)
                && stderr.contains(closing),
            "{who}: inside aterm a bare cargo is announced by what it resolved to — \
             stderr={stderr:?} stdout={stdout:?}"
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
        // The reroute half: any agents marker or a non-empty `$ATERM_REROUTE_DIR` (the
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

/// Every stub runs something, through the real `atpkg __reroute` and the real stubs:
/// `rustfmt` with no trustfmt installed runs the upstream copy after one line naming
/// `aterm pkg install trust`, and `z3` runs upstream after one line naming `ay` — each with
/// the fake's own exit code. `[reroute] strict = true` in the temp `aterm.toml` puts the
/// refusals back: 127 with the install line, and z3's exit 2 naming its path.
#[test]
fn a_missing_trust_tool_and_z3_run_upstream_and_strict_restores_the_refusals() {
    let fx = Fixture::new("never-refuse");
    for name in ["rustfmt", "z3"] {
        write_executable(
            &fx.reroute.join(name),
            &atpkg::reroute::stub_body_sh(
                name,
                Path::new(env!("CARGO_BIN_EXE_atpkg")),
                &fx.reroute,
                false,
            ),
        );
        write_executable(
            &fx.homebrew.join(name),
            &format!("#!/bin/sh\necho \"upstream {name}: $*\"\nexit {FAKE_EXIT}\n"),
        );
    }
    let path = format!(
        "{}:{}:/usr/bin:/bin",
        Fixture::s(&fx.reroute),
        Fixture::s(&fx.homebrew)
    );
    let call = |command: &str| -> (String, String, Option<i32>) {
        let out = Command::new("/bin/sh")
            .args(["-c", command])
            .current_dir(&fx.home)
            .env_clear()
            .env("HOME", &fx.home)
            .env("XDG_CONFIG_HOME", &fx.config_home)
            .env("PATH", &path)
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).trim_end().to_owned(),
            out.status.code(),
        )
    };
    // The default: both run, after one line each.
    assert_eq!(
        call("rustfmt src/x.rs"),
        (
            "upstream rustfmt: src/x.rs\n".to_owned(),
            atpkg::reroute::fallback_not_installed_message("rustfmt", "trustfmt", "trust"),
            Some(FAKE_EXIT)
        )
    );
    assert_eq!(
        call("z3 -in"),
        (
            "upstream z3: -in\n".to_owned(),
            // No `ay` in this store: the line names the command that installs it.
            atpkg::reroute::oracle_notice("z3", "ay", Some("`aterm pkg install ay` installs it")),
            Some(FAKE_EXIT)
        )
    );
    // Strict: the refusals, byte for byte, and nothing upstream ran.
    let dir = fx.config_home.join("aterm");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("aterm.toml"), "[reroute]\nstrict = true\n").unwrap();
    assert_eq!(
        call("rustfmt src/x.rs"),
        (
            String::new(),
            atpkg::reroute::not_installed_message("rustfmt", "trustfmt", "trust"),
            Some(127)
        )
    );
    assert_eq!(
        call("z3 -in"),
        (
            String::new(),
            atpkg::reroute::oracle_message("z3", Some(&fx.homebrew.join("z3"))),
            Some(i32::from(atpkg::reroute::REFUSAL_EXIT))
        )
    );
}
