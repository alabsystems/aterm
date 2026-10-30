// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell hook PATH ordering and reroute stub execution tests across POSIX shells.

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use atpkg::reroute::PASSTHROUGH_ENV;

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
        let warmed =
            aterm_spec::wait::past_text_busy(|| Command::new(homebrew.join("cargo")).output())
                .unwrap();
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

    fn lay_hook(&self, ext: &str) -> PathBuf {
        let shell_d = self.home.join(".aterm/shell.d");
        std::fs::create_dir_all(&shell_d).unwrap();
        let path = shell_d.join(format!("{}.{ext}", atpkg::hooks::HOOK_BASENAME));
        std::fs::write(&path, self.hook(ext)).unwrap();
        path
    }

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

struct Shell {
    name: &'static str,
    ext: &'static str,
    exe: PathBuf,
    args: &'static [&'static str],
}

struct Lanes {
    shells: Vec<Shell>,
    absent: Vec<&'static str>,
}

impl<'a> IntoIterator for &'a Lanes {
    type Item = &'a Shell;
    type IntoIter = std::slice::Iter<'a, Shell>;
    fn into_iter(self) -> Self::IntoIter {
        self.shells.iter()
    }
}

impl Drop for Lanes {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            aterm_spec::host::could_not_run_without(&self.absent);
        }
    }
}

fn posix_shells() -> Lanes {
    let mut shells = Vec::new();
    let mut absent = Vec::new();
    for (name, ext, args) in [
        ("bash", "bash", &["--noprofile", "--norc", "-c"][..]),
        ("zsh", "zsh", &["-f", "-c"][..]),
    ] {
        match aterm_spec::host::test_shell(name) {
            Some(exe) => shells.push(Shell {
                name,
                ext,
                exe,
                args,
            }),
            None => {
                assert_ne!(name, "bash", "bash must be runnable on a unix host");
                absent.push(name);
            }
        }
    }
    Lanes { shells, absent }
}

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
    cmd.args(args)
        .arg(script)
        .current_dir(&fx.home)
        .env_clear()
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &fx.config_home)
        .env("PATH", path)
        .env("ATPKG_TEST_HOOK", hook)
        .env(atpkg::build_admit::LEDGER_ENV, fx.home.join("ledger"));
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output()
        .unwrap_or_else(|error| panic!("{}: spawn: {error}", exe.display()))
}

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

#[test]
fn inside_aterm_the_hook_puts_reroute_then_agents_first_and_outside_neither() {
    let fx = Fixture::new("order");
    let rest_inside = |path: &[String]| -> Vec<String> {
        path.iter()
            .filter(|e| {
                Path::new(e) != fx.reroute && Path::new(e) != fx.agents && Path::new(e) != fx.bin
            })
            .cloned()
            .collect()
    };
    let inherited: Vec<String> = fx.inherited_path().split(':').map(str::to_owned).collect();
    for shell in &posix_shells() {
        let hook = fx.lay_hook(shell.ext);
        let who = shell.name;
        for &marker in atpkg::hooks::AGENTS_MARKERS {
            let inside = sourced_path(&fx, shell, &hook, 1, &[(marker, "1")]);
            assert_eq!(
                inside[..3],
                [
                    Fixture::s(&fx.reroute),
                    Fixture::s(&fx.agents),
                    Fixture::s(&fx.bin),
                ],
                "{who} with {marker}: PATH starts reroute:agents:bin — {inside:?}"
            );
            assert_eq!(count(&inside, &fx.reroute), 1, "{who}: {inside:?}");
            assert_eq!(count(&inside, &fx.agents), 1, "{who}: {inside:?}");
            assert_eq!(count(&inside, &fx.bin), 1, "{who}: {inside:?}");
            let want = rest_inside(&inherited);
            assert_eq!(
                rest_inside(&inside),
                want,
                "{who} with {marker}: {inside:?}"
            );
            assert_eq!(
                sourced_path(&fx, shell, &hook, 2, &[(marker, "1")]),
                inside,
                "{who} with {marker}: sourcing twice is sourcing once"
            );
        }
        for value in ["", "0"] {
            let inside = sourced_path(
                &fx,
                shell,
                &hook,
                1,
                &[("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, value)],
            );
            assert_eq!(
                inside[..3],
                [
                    Fixture::s(&fx.reroute),
                    Fixture::s(&fx.agents),
                    Fixture::s(&fx.bin),
                ],
                "{who}: a passthrough marker of {value:?} is not engaged — {inside:?}"
            );
        }
        let outside = sourced_path(&fx, shell, &hook, 1, &[]);
        assert_eq!(count(&outside, &fx.reroute), 0, "{who}: {outside:?}");
        assert_eq!(count(&outside, &fx.agents), 0, "{who}: {outside:?}");
        assert_eq!(
            outside.last().map(String::as_str),
            Some(Fixture::s(&fx.bin))
        );
        assert_eq!(
            sourced_path(&fx, shell, &hook, 2, &[]),
            outside,
            "{who}: idempotent outside aterm too"
        );
        let passthrough = [("ATERM_CHILD", "1"), (PASSTHROUGH_ENV, "1")];
        let escaped = sourced_path(&fx, shell, &hook, 1, &passthrough);
        assert_eq!(
            escaped[..2],
            [Fixture::s(&fx.agents), Fixture::s(&fx.bin)],
            "{who}: the managed agents and bin still lead under --no-reroute — {escaped:?}"
        );
        assert_eq!(count(&escaped, &fx.reroute), 0, "{who}: {escaped:?}");
        assert_eq!(count(&escaped, &fx.agents), 1, "{who}: {escaped:?}");
        assert_eq!(count(&escaped, &fx.bin), 1, "{who}: {escaped:?}");
        assert_eq!(
            sourced_path(&fx, shell, &hook, 2, &passthrough),
            escaped,
            "{who}: idempotent under the passthrough marker"
        );
    }
}

#[test]
fn a_tty_session_whose_front_door_handed_no_agents_dir_still_leads_with_reroute() {
    let fx = Fixture::new("no-agents-handoff");
    let reroute = Fixture::s(&fx.reroute).to_owned();
    let script = ". \"$ATPKG_TEST_HOOK\"; . \"$ATPKG_TEST_HOOK\"; \
                  printf 'PATH=%s\\nAGENTS=%s\\n' \"$PATH\" \"${ATPKG_AGENTS-<unset>}\"";
    let upstream_ran = "homebrew-style cargo: build --x\n";
    for shell in &posix_shells() {
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

#[test]
fn inside_aterm_a_bare_cargo_is_announced_and_then_runs_upstream_with_its_exit_code() {
    let fx = Fixture::new("stub");
    let script = ". \"$ATPKG_TEST_HOOK\"; cargo build --x";
    let upstream_ran = "homebrew-style cargo: build --x\n";
    let lanes = posix_shells();
    for shell in &lanes {
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
        for lane in ["targo --unverified build --x", "targo trust build --x"] {
            assert!(
                stderr.contains(lane),
                "{who}: inside aterm a bare cargo is announced with {lane:?} — \
                 stderr={stderr:?} stdout={stdout:?}"
            );
        }
        assert_eq!(
            stdout, upstream_ran,
            "{who}: upstream ran with the same args"
        );
        assert_eq!(code, Some(FAKE_EXIT), "{who}: upstream's exit code is kept");
        let (stdout, stderr, code) = cargo(&[]);
        assert_eq!(stderr, "", "{who}: outside aterm nothing is announced");
        assert_eq!(stdout, upstream_ran, "{who}");
        assert_eq!(code, Some(FAKE_EXIT), "{who}");
    }
    fx.silence_announcements();
    for shell in &lanes {
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
            .env(atpkg::build_admit::LEDGER_ENV, fx.home.join("ledger"))
            .output()
            .unwrap();
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).trim_end().to_owned(),
            out.status.code(),
        )
    };
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
            atpkg::reroute::oracle_notice("z3", "ay", Some("`aterm pkg install ay` installs it")),
            Some(FAKE_EXIT)
        )
    );
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

#[test]
fn inside_aterm_managed_git_and_gh_lead_ahead_of_clt_stubs_and_outside_stay_behind() {
    let fx = Fixture::new("git-gh");
    let layout = atpkg::store::Layout {
        prefix: atpkg::store::default_prefix(&fx.home),
    };
    write_executable(
        &fx.usr_local.join("git"),
        &format!(
            "#!/bin/sh\necho \"xcode-select: note: no developer tools were found\" >&2\nexit {FAKE_EXIT}\n"
        ),
    );
    write_executable(
        &fx.usr_local.join("gh"),
        &format!("#!/bin/sh\necho \"upstream gh shadow: $*\" >&2\nexit {FAKE_EXIT}\n"),
    );
    for stub in ["git", "gh"] {
        let warmed =
            aterm_spec::wait::past_text_busy(|| Command::new(fx.usr_local.join(stub)).output())
                .unwrap();
        assert_eq!(warmed.status.code(), Some(FAKE_EXIT), "{warmed:?}");
    }

    let git_build = layout.build_dir("git", 1);
    let git_core = git_build.join("libexec/git-core");
    let git_tpl = git_build.join("share/git-core/templates");
    std::fs::create_dir_all(&git_core).unwrap();
    std::fs::create_dir_all(&git_tpl).unwrap();
    std::fs::create_dir_all(git_build.join("bin")).unwrap();
    write_executable(
        &git_core.join("git"),
        "#!/bin/sh\nprintf 'atpkg-git exec=%s tpl=%s nosys=%s args=%s\\n' \"${GIT_EXEC_PATH:-}\" \"${GIT_TEMPLATE_DIR:-}\" \"${GIT_ATTR_NOSYSTEM:-}\" \"$*\"\n",
    );
    write_executable(
        &git_build.join("bin/git"),
        "#!/bin/sh\n\
         __atpkg_bin_dir=$(CDPATH='' cd -- \"$(dirname -- \"$0\")\" && pwd -P) || exit 1\n\
         __atpkg_root=$(dirname -- \"$__atpkg_bin_dir\")\n\
         export GIT_EXEC_PATH=\"${GIT_EXEC_PATH:-$__atpkg_root/libexec/git-core}\"\n\
         if [ -d \"$__atpkg_root/share/git-core/templates\" ]; then\n\
           export GIT_TEMPLATE_DIR=\"${GIT_TEMPLATE_DIR:-$__atpkg_root/share/git-core/templates}\"\n\
         fi\n\
         exec \"$__atpkg_root/libexec/git-core/git\" \"$@\"\n",
    );
    atpkg::activate::activate_build(&layout, &git_build).unwrap();
    let git_env = atpkg::shim_env::ShimEnv::admit(&["GIT_ATTR_NOSYSTEM=1".to_owned()]).unwrap();
    let refused_git = atpkg::activate::install_shims_env(
        &layout,
        &git_build,
        &["git".to_owned()],
        atpkg::activate::Aliases::Alab,
        &git_env,
    )
    .unwrap();
    assert!(refused_git.is_empty(), "{refused_git:?}");

    let gh_build = layout.build_dir("gh", 1);
    std::fs::create_dir_all(gh_build.join("bin")).unwrap();
    write_executable(
        &gh_build.join("bin/gh"),
        "#!/bin/sh\nprintf 'atpkg-gh args=%s\\n' \"$*\"\n",
    );
    atpkg::activate::activate_build(&layout, &gh_build).unwrap();
    let refused_gh = atpkg::activate::install_shims_env(
        &layout,
        &gh_build,
        &["gh".to_owned()],
        atpkg::activate::Aliases::Alab,
        &atpkg::shim_env::ShimEnv::NONE,
    )
    .unwrap();
    assert!(refused_gh.is_empty(), "{refused_gh:?}");

    let canonical_build = std::fs::canonicalize(&git_build).unwrap();
    let expected_inside = format!(
        "atpkg-git exec={}/libexec/git-core tpl={}/share/git-core/templates nosys=1 args=clone https://example.invalid/repo.git\natpkg-gh args=auth status\n",
        canonical_build.display(),
        canonical_build.display(),
    );

    let lanes = posix_shells();
    for shell in &lanes {
        let hook = fx.lay_hook(shell.ext);
        let inside = run(
            &fx,
            &shell.exe,
            shell.args,
            ". \"$ATPKG_TEST_HOOK\"; git clone https://example.invalid/repo.git && gh auth status",
            &hook,
            &fx.inherited_path(),
            &[("ATERM_CHILD", "1")],
        );
        assert!(
            inside.status.success(),
            "{}: inside failed: stdout={:?} stderr={:?}",
            shell.name,
            String::from_utf8_lossy(&inside.stdout),
            String::from_utf8_lossy(&inside.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&inside.stdout),
            expected_inside,
            "{}",
            shell.name
        );

        // Negative control: outside aterm, pkg/bin stays behind the preceding stub, while
        // alab-git and alab-gh still reach the managed builds directly.
        let outside = run(
            &fx,
            &shell.exe,
            shell.args,
            ". \"$ATPKG_TEST_HOOK\"; git clone https://example.invalid/repo.git",
            &hook,
            &fx.inherited_path(),
            &[],
        );
        assert_eq!(outside.status.code(), Some(FAKE_EXIT), "{}", shell.name);
        assert!(
            String::from_utf8_lossy(&outside.stderr).contains("xcode-select"),
            "{}: stderr={:?}",
            shell.name,
            String::from_utf8_lossy(&outside.stderr)
        );

        let alab_alias = run(
            &fx,
            &shell.exe,
            shell.args,
            ". \"$ATPKG_TEST_HOOK\"; alab-git --version && alab-gh auth status",
            &hook,
            &fx.inherited_path(),
            &[],
        );
        assert!(
            alab_alias.status.success(),
            "{}: alab-* failed: {:?}",
            shell.name,
            String::from_utf8_lossy(&alab_alias.stderr)
        );
    }
}
