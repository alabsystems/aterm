// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// While Apple's Command Line Tools are missing, a spawn puts the shim directory just ahead of
// `/usr/bin` and names it in `ATERM_CLT_SHIM` (aterm-gui `clt_shim::settle_session_path`), and
// its `.wanted` mark is there. A login shell's profile (`/etc/zprofile`'s `path_helper`, a fish
// login's `/etc/paths`, a person's own rc line) then puts `/usr/bin` back in front of it, so
// `git` would be Apple's stub, which raises the install dialog the shim exists to hold back. Each
// integration moves the directory back to just ahead of the first `/usr/bin` once the profile
// files have run, behind the person's own earlier entries, and a bash or fish started inside the
// tab does the same through `BASH_ENV` or aterm's `vendor_conf.d`; once the mark is gone, no
// shell puts it back. Nothing here runs `git`: `whence -p`, `type -P`, `command -s` and
// `Get-Command` only name it, and the shim's `git` is a stand-in of the test's own.

/// The shim directory of `fx`, with a stand-in `git` that is never run.
#[cfg(unix)]
fn lay_clt_shim(fx: &LiveFixture) -> String {
    let shim = fx.foreign.with_file_name("clt-shim");
    std::fs::create_dir_all(&shim).expect("the shim directory");
    write_executable(&shim.join("git"), "#!/bin/sh\necho shim-git\n");
    shim.to_str().expect("UTF-8 temp dir").to_owned()
}

/// Mark the shim at `shim` wanted, as the window does while it installs the tools.
#[cfg(unix)]
fn want_clt_shim(shim: &str) {
    std::fs::write(format!("{shim}.wanted"), b"").expect("the shim's wanted mark");
}

/// Whether `shim` sits just ahead of the first `/usr/bin` on `path`, once.
#[cfg(unix)]
fn shim_just_ahead(path: &str, shim: &str) -> bool {
    let entries: Vec<&str> = path.split(':').collect();
    let at_shim = entries.iter().position(|entry| *entry == shim);
    entries.iter().filter(|entry| **entry == shim).count() == 1
        && at_shim.map(|at| at + 1) == entries.iter().position(|entry| *entry == "/usr/bin")
}

/// What a profile's `path_helper` does to the shim's place: `/usr/bin` and `/bin` first.
#[cfg(unix)]
const POSIX_PATH_HELPER: &str = "export PATH=\"/usr/bin:/bin:$PATH\"\n";

/// The actual markerless hook written by 2bfb41f325, retained as an upgrade input.
/// The new hook must cooperate with these bytes, not only with another new copy.
#[cfg(unix)]
const LEGACY_CLT_BASH_ENV: &str = r#"# aterm: the Command Line Tools shim in a bash started inside a tab.
{ __aterm_clt_flags=$-; set +xv; } 2>/dev/null
if [ -n "${ATERM_CLT_BASH_ENV:-}" ] && [ -f "$ATERM_CLT_BASH_ENV" ]; then
  case $__aterm_clt_flags in *v*) set -v ;; esac
  case $__aterm_clt_flags in *x*) set -x ;; esac
  . "$ATERM_CLT_BASH_ENV"
  { __aterm_clt_flags=$-; set +xv; } 2>/dev/null
fi
if [ -n "${ATERM_CLT_SHIM:-}" ] && [ -f "${ATERM_CLT_SHIM%/}.wanted" ]; then
  if [ -d "$ATERM_CLT_SHIM" ]; then
    __aterm_clt_place() {
      local LC_ALL=C
      local d="$ATERM_CLT_SHIM" p=":${PATH-}:" q s
      while :; do
        q="${p//":$d:"/:}"
        [ "$q" = "$p" ] && break
        p="$q"
      done
      case "$p" in
        *:/usr/bin:*) s=/usr/bin ;;
        *:/bin:*) s=/bin ;;
        *) return 0 ;;
      esac
      q="${p#*":$s:"}"
      p="${p%%":$s:"*}:$d:$s:$q"
      p="${p#:}"
      p="${p%:}"
      [ "$p" = "${PATH-}" ] || export PATH="$p"
    }
    __aterm_clt_place
    unset -f __aterm_clt_place
  fi
else
  if [ -n "${ATERM_CLT_BASH_ENV+set}" ]; then
    export BASH_ENV="$ATERM_CLT_BASH_ENV"
  else
    unset BASH_ENV
  fi
  unset ATERM_CLT_BASH_ENV
fi
case $__aterm_clt_flags in
  *x*v*|*v*x*) unset __aterm_clt_flags; set -xv ;;
  *x*) unset __aterm_clt_flags; set -x ;;
  *v*) unset __aterm_clt_flags; set -v ;;
  *) unset __aterm_clt_flags ;;
esac
"#;

/// Read the shell's `git` (the line `git_line` prints `GIT=<path>`) and PATH after its startup
/// files, and hold the shim's place: the shim's own `git` when `shim` is named, just ahead of the
/// first `/usr/bin`, behind the person's own entry (`fx.foreign`); never the shim's otherwise.
#[cfg(unix)]
fn assert_shim_placed(
    label: &str,
    sh: &mut LiveShell,
    fx: &LiveFixture,
    shim: &str,
    named: bool,
    git_line: &str,
) {
    sh.send(git_line);
    let at = sh.wait_for_after("MARK42", 0);
    let git = sh.value_before("GIT=", at);
    let path = sh.value_before("LIVEPATH=", at);
    let shim_git = format!("{shim}/git");
    if !named {
        assert_ne!(
            git, shim_git,
            "{label}: CONTROL: with no ATERM_CLT_SHIM the profile's order stands: {path:?}"
        );
        return;
    }
    assert_eq!(git, shim_git, "{label}: PATH {path:?}");
    let entries: Vec<&str> = path.split(':').collect();
    let at_shim = entries.iter().position(|entry| *entry == shim);
    let first_usr_bin = entries.iter().position(|entry| *entry == "/usr/bin");
    assert_eq!(
        at_shim.map(|at| at + 1),
        first_usr_bin,
        "{label}: just ahead of the first /usr/bin: {path:?}"
    );
    assert_eq!(
        entries.iter().filter(|entry| **entry == shim).count(),
        1,
        "{label}: once: {path:?}"
    );
    let foreign = entries
        .iter()
        .position(|entry| *entry == fx.foreign.to_str().expect("UTF-8"));
    assert!(
        foreign.is_some() && foreign < at_shim,
        "{label}: never ahead of the person's own earlier entry: {path:?}"
    );
}

/// A login zsh through the crate's own zdotdir wrapper: `/etc/zprofile` (macOS's
/// `path_helper`) and the fixture home's `.zprofile` reorder PATH after the integration loaded
/// (from `.zshenv`), and `.zshrc` prepends the person's own directory.
#[cfg(unix)]
fn spawn_login_zsh(zsh: &str, fx: &LiveFixture, extra: &[(&str, &str)]) -> LiveShell {
    std::fs::write(fx.home.join(".zprofile"), POSIX_PATH_HELPER).expect(".zprofile");
    std::fs::write(
        fx.home.join(".zshrc"),
        format!("export PATH=\"{}:$PATH\"\n", fx.foreign.display()),
    )
    .expect(".zshrc");
    let InjectionEnv { env_add, .. } = prepare_into(ShellType::Zsh, &fx.base)
        .expect("prepare")
        .expect("zsh injection");
    let mut cmd = shell_command(zsh);
    cmd.args(["-l", "-i", "-o", "NO_ZLE", "-o", "NO_MONITOR"])
        .env("HOME", &fx.home)
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    cmd.env_remove("ATERM_UNSET_ZDOTDIR")
        .env("ATERM_ORIGINAL_ZDOTDIR", &fx.home);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("zsh", &mut cmd).holding(cmd.into_home())
}

/// The environment a spawn hands its shell while the shim is wanted (`shim` named) or not.
#[cfg(unix)]
fn clt_shim_env(shim: &str, named: bool) -> Vec<(&'static str, String)> {
    let mut env = vec![("PATH", format!("{shim}:/usr/bin:/bin"))];
    if named {
        env.push(("ATERM_CLT_SHIM", shim.to_owned()));
    }
    env
}

#[cfg(unix)]
fn as_extra<'a>(env: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    env.iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect()
}

#[cfg(unix)]
#[test]
fn a_login_zsh_keeps_the_shim_ahead_of_usr_bin() {
    let zsh = need_zsh();
    for named in [true, false] {
        let fx = LiveFixture::new();
        let shim = lay_clt_shim(&fx);
        want_clt_shim(&shim);
        let env = clt_shim_env(&shim, named);
        let mut sh = spawn_login_zsh(zsh, &fx, &as_extra(&env));
        assert_shim_placed(
            "zsh -l",
            &mut sh,
            &fx,
            &shim,
            named,
            "print -r -- \"GIT=$(whence -p git)\"; print -r -- \"LIVEPATH=$PATH\"; \
             print -r -- \"MARK$((40+2))\"",
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn a_bash_keeps_the_shim_ahead_of_usr_bin_after_its_profile() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        for named in [true, false] {
            let fx = LiveFixture::new();
            let shim = lay_clt_shim(&fx);
            want_clt_shim(&shim);
            let env = clt_shim_env(&shim, named);
            let mut sh = spawn_live_bash(bash, &fx, POSIX_PATH_HELPER, &as_extra(&env));
            assert_shim_placed(
                bash,
                &mut sh,
                &fx,
                &shim,
                named,
                "printf 'GIT=%s\\n' \"$(type -P git)\"; printf 'LIVEPATH=%s\\n' \"$PATH\"; \
                 echo \"MARK$((40+2))\"",
            );
            sh.finish();
        }
    }
}

#[cfg(unix)]
#[test]
fn a_fish_keeps_the_shim_ahead_of_usr_bin_after_its_config() {
    let fish = need_fish();
    for named in [true, false] {
        let fx = LiveFixture::new();
        let shim = lay_clt_shim(&fx);
        want_clt_shim(&shim);
        let env = clt_shim_env(&shim, named);
        let mut sh = spawn_live_fish_with(
            fish,
            &fx,
            "set -gx PATH /usr/bin /bin $PATH\n",
            &as_extra(&env),
        );
        assert_shim_placed(
            "fish",
            &mut sh,
            &fx,
            &shim,
            named,
            "printf 'GIT=%s\\n' (command -s git); printf 'LIVEPATH=%s\\n' (string join : -- $PATH); \
             echo MARK(math 40 + 2)",
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn a_pwsh_keeps_the_shim_ahead_of_usr_bin_after_its_profile() {
    let pwsh = need_pwsh();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        aterm_tempfile::manifest_dir().display()
    );
    for (named, wanted) in [(true, true), (true, false), (false, true)] {
        let fx = LiveFixture::new();
        let shim = lay_clt_shim(&fx);
        if wanted {
            want_clt_shim(&shim);
        }
        let mut cmd = shell_command(pwsh);
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            // A profile that puts /usr/bin first and then the person's own directory, then
            // aterm's load, as `prepare` runs it.
            "$env:PATH = '/usr/bin:/bin:' + $env:PATH; \
             $env:PATH = $env:ATERM_TEST_OWN + ':' + $env:PATH; . $env:ATERM_TEST_SCRIPT; \
             [Console]::WriteLine('GIT=' + (Get-Command git -CommandType Application \
             -ErrorAction SilentlyContinue | Select-Object -First 1).Source); \
             [Console]::WriteLine('LIVEPATH=' + $env:PATH)",
        ])
        .env("ATERM_TEST_SCRIPT", &script)
        .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
        .env("POWERSHELL_UPDATECHECK", "Off")
        .env("ATERM_TEST_OWN", &fx.foreign)
        .env("PATH", format!("{shim}:/usr/bin:/bin"));
        if named {
            cmd.env("ATERM_CLT_SHIM", &shim);
        } else {
            cmd.env_remove("ATERM_CLT_SHIM");
        }
        let out = cmd
            .output()
            .unwrap_or_else(|error| panic!("spawn {pwsh}: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let value = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_else(|| {
                    panic!(
                        "pwsh: no {key} line: {stdout:?}; stderr: {:?}",
                        String::from_utf8_lossy(&out.stderr)
                    )
                })
                .to_owned()
        };
        let (git, path) = (value("GIT="), value("LIVEPATH="));
        let shim_git = format!("{shim}/git");
        if !named {
            assert_ne!(git, shim_git, "pwsh: CONTROL: {path:?}");
            continue;
        }
        if !wanted {
            assert_ne!(
                git, shim_git,
                "pwsh: a shim nothing wants is put back ahead of /usr/bin: {path:?}"
            );
            continue;
        }
        assert_eq!(git, shim_git, "pwsh: PATH {path:?}");
        let entries: Vec<&str> = path.split(':').collect();
        let at_shim = entries.iter().position(|entry| *entry == shim);
        assert_eq!(
            at_shim.map(|at| at + 1),
            entries.iter().position(|entry| *entry == "/usr/bin"),
            "pwsh: just ahead of the first /usr/bin: {path:?}"
        );
        let foreign = entries
            .iter()
            .position(|entry| *entry == fx.foreign.to_str().expect("UTF-8"));
        assert!(
            foreign.is_some() && foreign < at_shim,
            "pwsh: never ahead of the person's own earlier entry: {path:?}"
        );
    }
}

/// Once nothing wants the shim (its mark is gone: a Stop, an install that ended unfinished, the
/// tools arrived), a shell started under a tab that had it still inherits `ATERM_CLT_SHIM`, and
/// the directory stays; no integration puts it back on PATH, and none
/// names aterm's `BASH_ENV` hook. With the mark, the same shell places it (the control).
#[cfg(unix)]
#[test]
fn a_shell_never_puts_back_a_shim_nothing_wants() {
    let mut shells = ShellLanes::default();
    let check = |label: &str, wanted: bool, shim: &str, path: &str, bash_env: &str| {
        let on_path = path.split(':').any(|entry| entry == shim);
        if wanted {
            assert!(
                on_path,
                "{label}: CONTROL: a wanted shim is placed: {path:?}"
            );
            assert!(
                bash_env.ends_with("/bash/clt_env"),
                "{label}: CONTROL: BASH_ENV names the hook: {bash_env:?}"
            );
        } else {
            assert!(
                !on_path,
                "{label}: a shim nothing wants is put back on PATH: {path:?}"
            );
            assert_eq!(bash_env, "unset", "{label}: no hook for nothing");
        }
    };
    for wanted in [false, true] {
        for bash in shells.bashes() {
            let fx = LiveFixture::new();
            let shim = lay_clt_shim(&fx);
            if wanted {
                want_clt_shim(&shim);
            }
            let env = [("ATERM_CLT_SHIM", shim.as_str())];
            let mut sh = spawn_live_bash(bash, &fx, "", &env);
            sh.send(
                "printf 'LIVEPATH=%s\\n' \"$PATH\"; printf 'ENV=%s\\n' \"${BASH_ENV-unset}\"; \
                 echo \"MARK$((40+2))\"",
            );
            let at = sh.wait_for_after("MARK42", 0);
            let (path, bash_env) = (
                sh.value_before("LIVEPATH=", at),
                sh.value_before("ENV=", at),
            );
            sh.finish();
            check(bash, wanted, &shim, &path, &bash_env);
        }
        if let Some(zsh) = shells.zsh() {
            let fx = LiveFixture::new();
            let shim = lay_clt_shim(&fx);
            if wanted {
                want_clt_shim(&shim);
            }
            let env = [("ATERM_CLT_SHIM", shim.as_str())];
            let mut sh = spawn_live_zsh(zsh, &fx, "", &env);
            sh.send(
                "print -r -- \"LIVEPATH=$PATH\"; print -r -- \"ENV=${BASH_ENV-unset}\"; \
                 print -r -- \"MARK$((40+2))\"",
            );
            let at = sh.wait_for_after("MARK42", 0);
            let (path, bash_env) = (
                sh.value_before("LIVEPATH=", at),
                sh.value_before("ENV=", at),
            );
            sh.finish();
            check("zsh", wanted, &shim, &path, &bash_env);
        }
        if let Some(fish) = shells.fish() {
            let fx = LiveFixture::new();
            let shim = lay_clt_shim(&fx);
            if wanted {
                want_clt_shim(&shim);
            }
            let env = [("ATERM_CLT_SHIM", shim.as_str())];
            let mut sh = spawn_live_fish_with(fish, &fx, "", &env);
            sh.send(
                "printf 'LIVEPATH=%s\\n' (string join : -- $PATH); \
                 if set -q BASH_ENV; printf 'ENV=%s\\n' $BASH_ENV; else; printf 'ENV=unset\\n'; end; \
                 echo MARK(math 40 + 2)",
            );
            let at = sh.wait_for_after("MARK42", 0);
            let (path, bash_env) = (
                sh.value_before("LIVEPATH=", at),
                sh.value_before("ENV=", at),
            );
            sh.finish();
            check("fish", wanted, &shim, &path, &bash_env);
        }
    }
}

/// An agent in a tab runs its commands in a login shell of its own (`bash -lc`): its profile
/// (here the fixture's, as `path_helper` does) puts `/usr/bin` in front again after the tab's
/// integration placed the shim, and it loads no integration. `BASH_ENV` names aterm's hook while
/// the shim is wanted, so that bash puts it back just ahead of `/usr/bin` after its profile, and
/// the person's own `BASH_ENV` still runs first. Once the mark goes, the tab gives the person's
/// own `BASH_ENV` back at its next prompt, and a login bash leaves the profile's order alone.
/// NEGATIVE CONTROL: with no hook the inner bash has `/usr/bin` ahead of the shim, so its `git`
/// is Apple's stub while aterm installs the tools.
#[cfg(unix)]
#[test]
fn a_login_shell_started_inside_a_tab_keeps_the_shim_ahead_of_usr_bin() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let shim = lay_clt_shim(&fx);
        want_clt_shim(&shim);
        std::fs::write(fx.home.join(".bash_profile"), POSIX_PATH_HELPER).expect(".bash_profile");
        let own = fx.home.join("own-bash-env");
        std::fs::write(&own, "OWN_RAN=yes\n").expect("the person's own BASH_ENV");
        let own = own.to_str().expect("UTF-8 temp dir").to_owned();
        let mut env = clt_shim_env(&shim, true);
        env.push(("BASH_ENV", own.clone()));
        let mut sh = spawn_live_bash(bash, &fx, "", &as_extra(&env));
        let inner = format!(
            "printf 'INNER=%s\\n' \"$({bash} -l -c 'printf \"%s|%s\" \"$PATH\" \"${{OWN_RAN-}}\"')\""
        );
        sh.send(&format!(
            "printf 'LIVEPATH=%s\\n' \"$PATH\"; {inner}; echo \"MARK$((40+2))\""
        ));
        let at = sh.wait_for_after("MARK42", 0);
        let tab = sh.value_before("LIVEPATH=", at);
        let wanted_inner = sh.value_before("INNER=", at);
        assert!(
            shim_just_ahead(&tab, &shim),
            "{bash}: CONTROL: the tab's own: {tab:?}"
        );
        let (path, own_ran) = wanted_inner.split_once('|').unwrap_or((&wanted_inner, ""));
        assert!(
            shim_just_ahead(path, &shim),
            "{bash} -l inside the tab: the shim just ahead of /usr/bin: {path:?}"
        );
        assert_eq!(own_ran, "yes", "{bash} -l: the person's own BASH_ENV ran");

        std::fs::remove_file(format!("{shim}.wanted")).expect("the mark goes");
        sh.send("echo \"MARK$((40+3))\"");
        let at = sh.wait_for_after("MARK43", at);
        sh.send(&format!(
            "printf 'ENV=%s\\n' \"${{BASH_ENV-unset}}\"; \
             printf 'KEPT=%s\\n' \"${{ATERM_CLT_BASH_ENV-unset}}\"; {inner}; echo \"MARK$((40+4))\""
        ));
        let at = sh.wait_for_after("MARK44", at);
        let bash_env = sh.value_before("ENV=", at);
        let kept = sh.value_before("KEPT=", at);
        let after = sh.value_before("INNER=", at);
        sh.finish();
        assert_eq!(bash_env, own, "{bash}: the person's own BASH_ENV is back");
        assert_eq!(kept, "unset", "{bash}: aterm keeps nothing for it");
        let (path, own_ran) = after.split_once('|').unwrap_or((&after, ""));
        assert!(
            !shim_just_ahead(path, &shim),
            "{bash} -l: a shim nothing wants is put back: {path:?}"
        );
        assert_eq!(own_ran, "yes", "{bash} -l: the person's own BASH_ENV ran");
    }
}

/// Two real prepared script directories share ownership across a handoff. A later user hook,
/// even one named `bash/clt_env`, stays theirs when the wanted mark goes.
#[cfg(unix)]
#[test]
fn a_clt_hook_handoff_keeps_its_user_predecessor_and_respects_a_replacement() {
    let mut shells = ShellLanes::default();
    let inner_bash = shells.bashes()[0];
    for (dialect, program) in shells.lanes(&["bash", "zsh", "fish"]) {
        let fx = LiveFixture::new();
        let shim = lay_clt_shim(&fx);
        want_clt_shim(&shim);
        let cache_b = prepare_into(ShellType::Bash, &fx.base.with_file_name("cache-b"))
            .unwrap()
            .unwrap()
            .env_add
            .into_iter()
            .find(|(key, _)| key == "ATERM_SHELL_INTEGRATION_DIR")
            .unwrap()
            .1;
        let own = fx.home.join("own-bash-env");
        let replacement = fx.home.join("user/bash/clt_env");
        std::fs::create_dir_all(replacement.parent().unwrap()).unwrap();
        let count = fx.home.join("user-hook-runs");
        std::fs::write(&own, "printf 'user\\n' >> \"$ATERM_TEST_HOOK_RUNS\"\n").unwrap();
        std::fs::write(
            &replacement,
            "printf 'replacement\\n' >> \"$ATERM_TEST_HOOK_RUNS\"\n",
        )
        .unwrap();
        let mut env = clt_shim_env(&shim, true);
        env.extend([
            ("BASH_ENV", own.to_str().unwrap().to_owned()),
            ("ATERM_TEST_CACHE_B", cache_b.clone()),
            ("ATERM_TEST_HOOK_RUNS", count.to_str().unwrap().to_owned()),
            (
                "ATERM_TEST_REPLACEMENT",
                replacement.to_str().unwrap().to_owned(),
            ),
        ]);
        let mut sh = match dialect {
            "bash" => spawn_live_bash(program, &fx, "", &as_extra(&env)),
            "zsh" => spawn_live_zsh(program, &fx, "", &as_extra(&env)),
            "fish" => spawn_live_fish(program, &fx, &as_extra(&env)),
            _ => unreachable!(),
        };
        let inner = format!(
            "{} -uc 'printf \"CHILD=%s\\n\" \"${{BASH_ENV-unset}}\"'",
            posix_quoted(Path::new(inner_bash))
        );
        let handoff = if dialect == "fish" {
            "set -gx ATERM_SHELL_INTEGRATION_DIR \"$ATERM_TEST_CACHE_B\""
        } else {
            "export ATERM_SHELL_INTEGRATION_DIR=\"$ATERM_TEST_CACHE_B\""
        };
        let marker = |n| {
            if dialect == "fish" {
                format!("echo MARK(math 60 + {n})")
            } else {
                format!("echo \"MARK$((60+{n}))\"")
            }
        };
        sh.send(&format!(
            "{handoff}; __aterm_clt_bash_env \"$ATERM_CLT_SHIM\"; \
             printf 'BEFORE=%s\\n' \"$ATERM_CLT_BASH_ENV_BEFORE\"; \
             printf 'OWNER=%s\\n' \"$ATERM_CLT_BASH_ENV_HOOK\"; {inner}; {}",
            marker(1)
        ));
        let at = sh.wait_for_after("MARK61", 0);
        let hook_b = format!("{cache_b}/bash/clt_env");
        assert_eq!(
            sh.value_before("BEFORE=", at),
            own.to_str().unwrap(),
            "{dialect}"
        );
        assert_eq!(sh.value_before("OWNER=", at), hook_b, "{dialect}");
        assert_eq!(sh.value_before("CHILD=", at), hook_b, "{dialect}");
        assert_eq!(
            std::fs::read_to_string(&count).unwrap(),
            "user\n",
            "{dialect}"
        );
        // A stale self-context introduced after adoption must be cleared in the parent
        // before it releases the predecessor, without needing a child to repair its copy.
        let stale = if dialect == "fish" {
            "set -gx ATERM_CLT_BASH_ENV \"$ATERM_CLT_BASH_ENV_BEFORE\""
        } else {
            "export ATERM_CLT_BASH_ENV=\"$ATERM_CLT_BASH_ENV_BEFORE\""
        };
        let legacy = if dialect == "fish" {
            "if set -q ATERM_CLT_BASH_ENV; printf 'LEGACY=%s\\n' \"$ATERM_CLT_BASH_ENV\"; else; echo LEGACY=unset; end"
        } else {
            "printf 'LEGACY=%s\\n' \"${ATERM_CLT_BASH_ENV-unset}\""
        };
        sh.send(&format!(
            "{stale}; __aterm_clt_bash_env ''; {legacy}; \
             printf 'RESTORED=%s\\n' \"$BASH_ENV\"; \
             __aterm_clt_bash_env \"$ATERM_CLT_SHIM\"; {}",
            marker(5)
        ));
        let at = sh.wait_for_after("MARK65", at);
        assert_eq!(sh.value_before("LEGACY=", at), "unset", "{dialect}");
        assert_eq!(
            sh.value_before("RESTORED=", at),
            own.to_str().unwrap(),
            "{dialect}"
        );
        let replace = if dialect == "fish" {
            "set -gx BASH_ENV \"$ATERM_TEST_REPLACEMENT\""
        } else {
            "export BASH_ENV=\"$ATERM_TEST_REPLACEMENT\""
        };
        sh.send(&format!("{replace}; {}", marker(2)));
        let at = sh.wait_for_after("MARK62", at);
        std::fs::remove_file(format!("{shim}.wanted")).unwrap();
        sh.send(&format!("__aterm_clt_bash_env ''; {}", marker(3)));
        let at = sh.wait_for_after("MARK63", at);
        sh.send(&format!(
            "printf 'ENV=%s\\n' \"$BASH_ENV\"; {inner}; {}",
            marker(4)
        ));
        let at = sh.wait_for_after("MARK64", at);
        assert_eq!(
            sh.value_before("ENV=", at),
            replacement.to_str().unwrap(),
            "{dialect}"
        );
        assert_eq!(
            sh.value_before("CHILD=", at),
            replacement.to_str().unwrap(),
            "{dialect}"
        );
        sh.finish();
        assert_eq!(
            std::fs::read_to_string(&count).unwrap(),
            "user\nreplacement\n",
            "{dialect}"
        );
    }
}

/// The PowerShell integration has the same exact-owner handoff, using its real prepared
/// script and a real nested bash, without loading a user's PowerShell profile.
#[cfg(unix)]
#[test]
fn a_pwsh_clt_hook_handoff_keeps_its_user_predecessor_and_replacement() {
    let fx = LiveFixture::new();
    let shim = lay_clt_shim(&fx);
    want_clt_shim(&shim);
    let a = prepare_into(ShellType::PowerShell, &fx.base)
        .unwrap()
        .unwrap();
    let dir_a = &a.env_add[0].1;
    let b = prepare_into(ShellType::PowerShell, &fx.base.with_file_name("cache-b"))
        .unwrap()
        .unwrap();
    let dir_b = &b.env_add[0].1;
    let own = fx.home.join("own-bash-env");
    let replacement = fx.home.join("user/bash/clt_env");
    std::fs::create_dir_all(replacement.parent().unwrap()).unwrap();
    let count = fx.home.join("user-hook-runs");
    std::fs::write(&own, "printf 'user\\n' >> \"$ATERM_TEST_HOOK_RUNS\"\n").unwrap();
    std::fs::write(
        &replacement,
        "printf 'replacement\\n' >> \"$ATERM_TEST_HOOK_RUNS\"\n",
    )
    .unwrap();
    let mut shells = ShellLanes::default();
    let mut cmd = shell_command(need_pwsh());
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-Command",
        r#"
        . $env:ATERM_TEST_SCRIPT
        $env:ATERM_SHELL_INTEGRATION_DIR = $env:ATERM_TEST_CACHE_B
        __aterm_clt_bash_env $env:ATERM_CLT_SHIM
        [Console]::WriteLine('BEFORE=' + $env:ATERM_CLT_BASH_ENV_BEFORE)
        [Console]::WriteLine('OWNER=' + $env:ATERM_CLT_BASH_ENV_HOOK)
        & $env:ATERM_TEST_INNER_BASH -uc 'printf "CHILD=%s\n" "${BASH_ENV-unset}"'
        $env:ATERM_CLT_BASH_ENV = $env:ATERM_CLT_BASH_ENV_BEFORE
        __aterm_clt_bash_env ''
        if (Test-Path -LiteralPath Env:ATERM_CLT_BASH_ENV) {
            [Console]::WriteLine('LEGACY=' + $env:ATERM_CLT_BASH_ENV)
        } else { [Console]::WriteLine('LEGACY=unset') }
        [Console]::WriteLine('RESTORED=' + $env:BASH_ENV)
        __aterm_clt_bash_env $env:ATERM_CLT_SHIM
        $env:BASH_ENV = $env:ATERM_TEST_REPLACEMENT
        Remove-Item -LiteralPath ($env:ATERM_CLT_SHIM + '.wanted')
        __aterm_clt_bash_env ''
        [Console]::WriteLine('ENV=' + $env:BASH_ENV)
        & $env:ATERM_TEST_INNER_BASH -uc 'printf "AFTER=%s\n" "${BASH_ENV-unset}"'
    "#,
    ])
    .env("HOME", &fx.home)
    .env("TERM", "dumb")
    .env("ATERM_CHILD", "1")
    .env("ATERM_SHELL_INTEGRATION_DIR", dir_a)
    .env(
        "ATERM_TEST_SCRIPT",
        format!("{dir_a}/aterm_shell_integration.ps1"),
    )
    .env("ATERM_TEST_CACHE_B", dir_b)
    .env("ATERM_TEST_INNER_BASH", shells.bashes()[0])
    .env("ATERM_TEST_HOOK_RUNS", &count)
    .env("ATERM_TEST_REPLACEMENT", &replacement)
    .env("ATERM_CLT_SHIM", &shim)
    .env("BASH_ENV", &own)
    .env("PATH", "/usr/bin:/bin")
    .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
    .env("POWERSHELL_UPDATECHECK", "Off")
    // Debian's bash treats a non-interactive shell with SSH_CLIENT set as an sshd-run
    // shell: it sources /etc/bash.bashrc and never reads BASH_ENV, so under an SSH
    // session (the Linux lane's limactl, an ssh CI) the hook chain below silently does
    // not run (measured in the lane guest: minimal env runs the hook, +SSH_CLIENT
    // sources bashrc instead). The handoff is the subject, not the ambient session.
    .env_remove("SSH_CLIENT")
    .env_remove("SSH_CONNECTION")
    .env_remove("SSH_TTY");
    let output = cmd.output().unwrap();
    assert!(output.status.success(), "pwsh: {:?}", output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap()
    };
    assert_eq!(value("BEFORE="), own.to_str().unwrap());
    assert_eq!(value("OWNER="), format!("{dir_b}/bash/clt_env"));
    assert_eq!(value("CHILD="), format!("{dir_b}/bash/clt_env"));
    assert_eq!(value("LEGACY="), "unset");
    assert_eq!(value("RESTORED="), own.to_str().unwrap());
    assert_eq!(value("ENV="), replacement.to_str().unwrap());
    assert_eq!(value("AFTER="), replacement.to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(&count).unwrap(),
        "user\nreplacement\n"
    );
}

/// A new script set must consume the actual old hook, whose backup is still meaningful to
/// that old code. Unknown predecessors are restored as found; the old hook restores its own
/// user's hook in the child after the mark goes. A corrupt self-backup must not recurse.
#[cfg(unix)]
#[test]
fn a_clt_hook_handoff_from_the_legacy_cache_preserves_its_source_context() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        for prior in ["user", "unset", "self"] {
            let fx = LiveFixture::new();
            let shim = lay_clt_shim(&fx);
            want_clt_shim(&shim);
            let legacy = fx.base.join("legacy-a/bash/clt_env");
            std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            std::fs::write(&legacy, LEGACY_CLT_BASH_ENV).unwrap();
            let own = fx.home.join("own-bash-env");
            let count = fx.home.join("user-hook-runs");
            std::fs::write(&own, "printf 'user\\n' >> \"$ATERM_TEST_HOOK_RUNS\"\n").unwrap();
            let mut env = clt_shim_env(&shim, true);
            env.extend([
                ("BASH_ENV", legacy.to_str().unwrap().to_owned()),
                ("ATERM_TEST_HOOK_RUNS", count.to_str().unwrap().to_owned()),
            ]);
            if prior != "unset" {
                env.push((
                    "ATERM_CLT_BASH_ENV",
                    if prior == "user" { &own } else { &legacy }
                        .to_str()
                        .unwrap()
                        .to_owned(),
                ));
            }
            let mut sh = spawn_live_bash(bash, &fx, "", &as_extra(&env));
            let inner = format!(
                "{} -uc 'printf \"CHILD=%s\\n\" \"${{BASH_ENV-unset}}\"'",
                posix_quoted(Path::new(bash))
            );
            sh.send(&format!("{inner}; echo \"MARK$((50+1))\""));
            let at = sh.wait_for_after("MARK51", 0);
            assert!(sh.value_before("CHILD=", at).ends_with("/bash/clt_env"));
            let runs = || std::fs::read_to_string(&count).unwrap_or_default();
            assert_eq!(runs(), if prior == "user" { "user\n" } else { "" });
            std::fs::remove_file(format!("{shim}.wanted")).unwrap();
            sh.send("echo \"MARK$((50+2))\"");
            let at = sh.wait_for_after("MARK52", at);
            sh.send(&format!(
                "printf 'ENV=%s\\n' \"${{BASH_ENV-unset}}\"; \
                 printf 'OWNER=%s\\n' \"${{ATERM_CLT_BASH_ENV_HOOK-unset}}\"; \
                 printf 'BEFORE=%s\\n' \"${{ATERM_CLT_BASH_ENV_BEFORE-unset}}\"; \
                 printf 'LEGACY=%s\\n' \"${{ATERM_CLT_BASH_ENV-unset}}\"; \
                 {inner}; echo \"MARK$((50+3))\""
            ));
            let at = sh.wait_for_after("MARK53", at);
            assert_eq!(sh.value_before("ENV=", at), legacy.to_str().unwrap());
            assert_eq!(sh.value_before("OWNER=", at), "unset");
            assert_eq!(sh.value_before("BEFORE=", at), "unset");
            assert_eq!(
                sh.value_before("LEGACY=", at),
                if prior == "user" {
                    own.to_str().unwrap()
                } else {
                    "unset"
                }
            );
            assert_eq!(
                sh.value_before("CHILD=", at),
                if prior == "user" {
                    own.to_str().unwrap()
                } else {
                    "unset"
                }
            );
            sh.finish();
            assert_eq!(runs(), if prior == "user" { "user\nuser\n" } else { "" });
        }
    }
}

/// An explicitly empty BASH_ENV differs from an unset one. Neither invents a predecessor,
/// and a stale self-saved input cannot make the new hook source itself.
#[cfg(unix)]
#[test]
fn a_clt_hook_restores_empty_or_unset_and_never_sources_its_saved_self() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        for original in [None, Some("")] {
            let fx = LiveFixture::new();
            let shim = lay_clt_shim(&fx);
            want_clt_shim(&shim);
            let mut env = clt_shim_env(&shim, true);
            if let Some(value) = original {
                env.push(("BASH_ENV", value.to_owned()));
            }
            let mut sh = spawn_live_bash(bash, &fx, "", &as_extra(&env));
            sh.send(&format!(
                "env -u ATERM_CLT_BASH_ENV_HOOK -u ATERM_CLT_BASH_ENV_BEFORE \
                 ATERM_CLT_BASH_ENV=\"$BASH_ENV\" BASH_ENV=\"$BASH_ENV\" \
                 {} -uc 'printf \"SELF=%s\\n\" \"${{ATERM_CLT_BASH_ENV-unset}}\"'; \
                 echo \"MARK$((70+1))\"",
                posix_quoted(Path::new(bash))
            ));
            let at = sh.wait_for_after("MARK71", 0);
            assert_eq!(sh.value_before("SELF=", at), "unset");
            std::fs::remove_file(format!("{shim}.wanted")).unwrap();
            sh.send("echo \"MARK$((70+2))\"");
            let at = sh.wait_for_after("MARK72", at);
            sh.send(
                "printf 'ENV=%s\\n' \"${BASH_ENV-unset}\"; \
                     printf 'OWNER=%s\\n' \"${ATERM_CLT_BASH_ENV_HOOK-unset}\"; \
                     printf 'BEFORE=%s\\n' \"${ATERM_CLT_BASH_ENV_BEFORE-unset}\"; \
                     echo \"MARK$((70+3))\"",
            );
            let at = sh.wait_for_after("MARK73", at);
            assert_eq!(sh.value_before("ENV=", at), original.unwrap_or("unset"));
            assert_eq!(sh.value_before("OWNER=", at), "unset");
            assert_eq!(sh.value_before("BEFORE=", at), "unset");
            sh.finish();
        }
    }
}

/// A login fish started inside a fish tab (`fish -l -c`) reorders PATH from `/etc/paths`, as
/// macOS's `path_helper` does, and returns before the integration loads; it reads aterm's
/// `vendor_conf.d` after that reorder, which puts the shim back just ahead of `/usr/bin` while it
/// is wanted, and leaves the order alone once it is not.
/// NEGATIVE CONTROL: a placement that sits behind the interactive guard leaves `/usr/bin` ahead
/// of the shim in the inner fish.
#[cfg(target_os = "macos")]
#[test]
fn a_login_fish_started_inside_a_tab_keeps_the_shim_ahead_of_usr_bin() {
    let fish = need_fish();
    for wanted in [true, false] {
        let fx = LiveFixture::new();
        let shim = lay_clt_shim(&fx);
        if wanted {
            want_clt_shim(&shim);
        }
        let env = clt_shim_env(&shim, true);
        let mut sh = spawn_live_fish_with(fish, &fx, "", &as_extra(&env));
        sh.send(&format!(
            "printf 'INNER=%s\\n' ({fish} -l -c 'string join : -- $PATH'); echo MARK(math 40 + 2)"
        ));
        let at = sh.wait_for_after("MARK42", 0);
        let inner = sh.value_before("INNER=", at);
        sh.finish();
        assert_eq!(
            shim_just_ahead(&inner, &shim),
            wanted,
            "{fish} -l inside the tab (wanted: {wanted}): {inner:?}"
        );
    }
}
