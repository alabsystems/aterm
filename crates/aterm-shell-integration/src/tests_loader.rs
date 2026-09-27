// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// ─── The loader and the body (2026-09-26) ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests.rs, whose
// helpers (`LiveShell`, `LiveFixture`, `shell_command`, `shell_function_body`,
// the re-key probes) it uses.

/// A folder address this suite's builds never write: the NEWER build the live
/// tests point a running shell at.
#[cfg(unix)]
const NEWER_REV: &str = "0123456789abcdef";

/// Each POSIX script is ONE body between two markers, inside a loader: the
/// body holds every mark and the revision the host reads, the loader holds what
/// a shell runs once — the channel captures and scrubs, the body check, the
/// trampoline, the wiring — and none of that is in the body, so re-sourcing it
/// never re-captures a secret or re-wires a hook. PowerShell has no body.
#[test]
fn every_posix_script_is_a_loader_around_one_body() {
    for (label, script, fish) in [
        ("zsh", scripts::ZSH, false),
        ("bash", scripts::BASH, false),
        ("fish", scripts::FISH, true),
    ] {
        assert_eq!(script.matches(scripts::BODY_BEGIN).count(), 1, "{label}");
        assert_eq!(script.matches(scripts::BODY_END).count(), 1, "{label}");
        let body = scripts::body(script).expect(label);
        assert!(body.starts_with(scripts::BODY_BEGIN), "{label}");
        assert!(
            body.lines()
                .last()
                .is_some_and(|l| l.starts_with(scripts::BODY_END)),
            "{label}: the body runs through its end marker's line"
        );
        let at = script
            .find(body)
            .expect("the body is a slice of its script");
        let (head, tail) = (&script[..at], &script[at + body.len()..]);
        for needle in [
            "__aterm_body_check",
            BODY_POINTER_VAR,
            "__aterm_fresh_load",
            "__aterm_si_root",
            "__aterm_body_rev",
        ] {
            assert!(head.contains(needle), "{label}: the loader holds {needle}");
        }
        for mark in ["133;A", "133;B", "133;C", "133;D;", "633;E;"] {
            assert!(body.contains(mark), "{label}: the body emits {mark}");
        }
        assert!(
            body.contains(&format!("633;P;{INTEGRATION_REV_KEY}=")),
            "{label}: the body signs its revision"
        );
        for fn_name in [
            "__aterm_rekey_check",
            "__aterm_managed_path_live",
            "__aterm_mark_integration_rev",
        ] {
            let _ = shell_function_body(body, fn_name, fish);
        }
        // What runs once per shell is never in the body.
        for once in [
            "unset ATERM_SHELL_NONCE",
            "set -e ATERM_SHELL_NONCE",
            "unset ATERM_REKEY_PATH",
            "set -e ATERM_REKEY_PATH",
            "unset ATERM_INTEGRATION_POINTER",
            "set -e ATERM_INTEGRATION_POINTER",
            "PROMPT_COMMAND=",
            "trap '__aterm_preexec' DEBUG",
            "add-zsh-hook precmd __aterm_precmd",
            "zle -N zle-line-init",
            "functions -c fish_prompt",
            "shell.d\"/*",
            "shell.d/*",
            "export ATERM_SHELL_INTEGRATION_INSTALLED",
            "set -gx ATERM_SHELL_INTEGRATION_INSTALLED",
        ] {
            assert!(!body.contains(once), "{label}: `{once}` is the loader's");
        }
        // The wiring comes after the body, where every name it wires exists.
        let wiring = match label {
            "zsh" => "add-zsh-hook precmd __aterm_precmd",
            "bash" => "PROMPT_COMMAND=\"__aterm_prompt_command\"",
            _ => "functions -c fish_prompt __aterm_original_fish_prompt",
        };
        assert!(tail.contains(wiring), "{label}: {wiring} follows the body");
    }
    assert_eq!(scripts::body(scripts::POWERSHELL), None);
}

/// The script folder carries each body beside its loader, as the loader's own
/// text under a header — so the body a running shell takes from a folder is the
/// body a new shell of that build runs — and a body is part of what the
/// folder's address names.
#[test]
fn the_script_set_carries_each_body_beside_its_loader() {
    let dir = aterm_tempfile::tempdir().unwrap();
    ensure_scripts(dir.path()).unwrap();
    for (ext, script) in [
        ("zsh", scripts::ZSH),
        ("bash", scripts::BASH),
        ("fish", scripts::FISH),
    ] {
        let written =
            std::fs::read_to_string(dir.path().join(format!("{BODY_FILE_STEM}.{ext}"))).unwrap();
        assert_eq!(
            written,
            format!("{BODY_FILE_HEADER}{}", scripts::body(script).unwrap()),
            "{ext}"
        );
    }
    assert!(script_set_matches(dir.path()));
    std::fs::write(
        dir.path().join(format!("{BODY_FILE_STEM}.zsh")),
        "# another body\n",
    )
    .unwrap();
    assert!(
        !script_set_matches(dir.path()),
        "a changed body is a different set"
    );
}

/// A real zsh sources the BODY ALONE without an error, from inside a function
/// under `emulate -L zsh` (how the check sources it) and at top level, and the
/// body defines what the loader's trampoline and wiring name.
#[cfg(target_os = "macos")]
#[test]
fn a_real_zsh_sources_the_body_alone_cleanly() {
    let dir = aterm_tempfile::tempdir().unwrap();
    ensure_scripts(dir.path()).unwrap();
    let body = dir.path().join(format!("{BODY_FILE_STEM}.zsh"));
    let out = shell_command("/bin/zsh")
        .args([
            "-f",
            "-c",
            "f() { emulate -L zsh; . \"$1\"; }; f \"$1\" && . \"$1\" && \
             whence -w __aterm_body_precmd __aterm_preexec __aterm_first_precmd",
            "zsh",
        ])
        .arg(&body)
        .env("HOME", dir.path())
        .output()
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stderr.is_empty(), "no error while sourcing: {stderr}");
    for name in [
        "__aterm_body_precmd",
        "__aterm_preexec",
        "__aterm_first_precmd",
    ] {
        assert!(
            stdout.contains(&format!("{name}: function")),
            "{name}: {stdout}"
        );
    }
}

/// The body of this build under a cache root, plus a NEWER build's folder whose
/// body is this build's with one probe function spliced in: what a running
/// shell finds under its root after an update.
#[cfg(unix)]
struct BodyFixture {
    fx: LiveFixture,
    /// This build's folder: `<root>/<address>`.
    ours: PathBuf,
    /// The session's body pointer file (not yet written).
    pointer: PathBuf,
}

#[cfg(unix)]
impl BodyFixture {
    fn new() -> Self {
        let fx = LiveFixture::new();
        let ours = install_script_set(&fx.base).expect("install");
        let newer = fx.base.join(NEWER_REV);
        std::fs::create_dir_all(&newer).unwrap();
        for (ext, script, probe) in [
            ("zsh", scripts::ZSH, BODY_PROBE_FN),
            ("bash", scripts::BASH, BODY_PROBE_FN),
            ("fish", scripts::FISH, BODY_PROBE_FN_FISH),
        ] {
            let body = scripts::body(script).unwrap().replacen(
                scripts::BODY_END,
                &format!("{probe}\n{}", scripts::BODY_END),
                1,
            );
            std::fs::write(
                newer.join(format!("{BODY_FILE_STEM}.{ext}")),
                format!("{BODY_FILE_HEADER}{body}"),
            )
            .unwrap();
        }
        let pointer = fx.home.join("integration-pointer");
        Self { fx, ours, pointer }
    }

    fn ours_rev(&self) -> String {
        self.ours
            .file_name()
            .and_then(|n| n.to_str())
            .expect("an address")
            .to_string()
    }
}

/// An interactive zsh started as a tab is, from the scripts in `dir` (the
/// ZDOTDIR wrapper there sources the loader there), with an empty `.zshrc`.
#[cfg(unix)]
fn spawn_live_zsh_from(
    zsh: &str,
    fx: &LiveFixture,
    dir: &Path,
    extra: &[(&str, &str)],
) -> LiveShell {
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let InjectionEnv { env_add, .. } = injection_for(ShellType::Zsh, dir).expect("zsh");
    let mut cmd = shell_command(zsh);
    cmd.args(["-i", "-o", "NO_ZLE", "-o", "NO_MONITOR"])
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    cmd.env_remove("ATERM_UNSET_ZDOTDIR")
        .env("ATERM_ORIGINAL_ZDOTDIR", &fx.home);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("zsh", &mut cmd)
}

/// An interactive bash started as a tab is, on the `--rcfile` wrapper in `dir`
/// (which sources the loader there), reading the fixture home's `.bashrc`.
#[cfg(unix)]
fn spawn_live_bash_from(fx: &LiveFixture, dir: &Path, extra: &[(&str, &str)]) -> LiveShell {
    let InjectionEnv {
        env_add,
        argv_override,
    } = injection_for(ShellType::Bash, dir).expect("bash");
    let argv = argv_override.expect("bash uses --rcfile");
    let mut cmd = shell_command(bash_shell());
    cmd.args(&argv[1..])
        .arg("-i")
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1")
        .env_remove("BASH_ENV");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("bash", &mut cmd)
}

/// The probe function the NEWER body carries (POSIX function syntax, so the
/// same text serves zsh and bash).
#[cfg(unix)]
const BODY_PROBE_FN: &str = "__aterm_body_probe() { printf '%s\\n' PROBE-NEWER; }";

/// [`BODY_PROBE_FN`] in fish.
#[cfg(unix)]
const BODY_PROBE_FN_FISH: &str = "function __aterm_body_probe; printf '%s\\n' PROBE-NEWER; end";

/// A command that prints `RAN<n>` in `label`'s own arithmetic, so the echoed
/// command line never carries the marker.
#[cfg(unix)]
fn ran(label: &str, n: u32) -> String {
    if label == "fish" {
        format!("echo RAN(math {n} - 1 + 1)")
    } else {
        format!("echo RAN$(({n} - 1 + 1))")
    }
}

/// The line that prints the running body's revision, whether the newer body's
/// probe exists, the prompt hooks and PATH, then a marker whose number is
/// computed (so the echoed command line never matches it).
#[cfg(unix)]
fn body_probe(label: &str, n: u32) -> (String, String) {
    if label == "fish" {
        // fish's hooks are its event handlers: a body re-sourced must not
        // register one twice.
        return (
            format!(
                "printf 'REV=%s\\n' \"$__aterm_body_rev\"; functions -q __aterm_body_probe; \
                 and __aterm_body_probe; or printf '%s\\n' PROBE-NONE; \
                 printf 'HOOKS=%s\\n' (functions --handlers-type generic | string join ,); \
                 printf 'PATHV=%s\\n' (string join : -- $PATH); \
                 printf 'PENV=%s\\n' (env | grep -c ATERM_INTEGRATION_POINTER); echo BP(math 300 + {n})"
            ),
            format!("BP{}", 300 + n),
        );
    }
    let hooks = if label == "zsh" {
        "${(j:,:)precmd_functions}"
    } else {
        "$PROMPT_COMMAND"
    };
    (
        format!(
            "printf 'REV=%s\\n' \"${{__aterm_body_rev-}}\"; __aterm_body_probe 2>/dev/null || \
             printf '%s\\n' PROBE-NONE; printf 'HOOKS=%s\\n' \"{hooks}\"; \
             printf 'PATHV=%s\\n' \"$PATH\"; printf 'PENV=%s\\n' \"$(env | grep -c ATERM_INTEGRATION_POINTER)\"; echo BP$((300 + {n}))"
        ),
        format!("BP{}", 300 + n),
    )
}

/// The stdout between two offsets.
#[cfg(unix)]
fn stdout_between(sh: &LiveShell, from: usize, to: usize) -> String {
    let out = sh
        .stdout
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    String::from_utf8_lossy(&out[from.min(out.len())..to.min(out.len())]).into_owned()
}

/// Drive one live shell through the body pointer: spawned from this build's
/// folder, it signs this build's revision; a malformed pointer and one naming a
/// folder that is not there are consumed and change nothing; a pointer naming
/// the NEWER folder, written while the shell idles, is taken at its next prompt
/// — the newer body's probe exists, the revision it signs is the newer one, the
/// command that ran across the switch still gets its 133;D, and the marks keep
/// flowing, signed with the same key — while PATH and the prompt hooks are what
/// they were (the body re-sources idempotently). `hooked` = the shell was
/// spawned with the pointer path: the negative control is the same shell
/// without it, which never reads the file.
#[cfg(unix)]
fn live_body_pickup(label: &str, sh: &mut LiveShell, bf: &BodyFixture, hooked: bool) {
    let ours = bf.ours_rev();
    let (line, marker) = body_probe(label, 1);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, 0);
    assert_eq!(sh.value_before("REV=", at), ours, "{label}: spawned rev");
    assert_eq!(sh.value_before("PROBE-", at), "NONE", "{label}");
    assert_eq!(
        sh.value_before("PENV=", at),
        "0",
        "{label}: no child process learns the pointer path"
    );
    let (hooks, path) = (sh.value_before("HOOKS=", at), sh.value_before("PATHV=", at));
    let signed = format!("633;P;{INTEGRATION_REV_KEY}={ours};id={REKEY_OLD}");
    assert!(
        stdout_between(sh, 0, at).contains(&signed),
        "{label}: the prompt signs the running body's revision ({signed})\n{}",
        sh.transcript()
    );

    // Malformed, and a folder that is not there: consumed, nothing changes.
    let mut at = at;
    for (n, bad) in [(2, "../../../etc/passwd"), (3, "fedcba9876543210")] {
        std::fs::write(&bf.pointer, format!("{bad}\n")).unwrap();
        sh.send("true");
        let (line, marker) = body_probe(label, n);
        sh.send(&line);
        at = sh.wait_for_after(&marker, at);
        assert_eq!(sh.value_before("REV=", at), ours, "{label}: {bad}");
        assert_eq!(sh.value_before("PROBE-", at), "NONE", "{label}: {bad}");
        assert_eq!(
            !bf.pointer.exists(),
            hooked,
            "{label}: {bad} consumed only by a hooked shell"
        );
        let _ = std::fs::remove_file(&bf.pointer);
    }

    // The newer build, pointed at while the shell idles.
    std::fs::write(&bf.pointer, format!("{NEWER_REV}\n")).unwrap();
    let from = at;
    sh.send("true");
    let (line, marker) = body_probe(label, 4);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    if !hooked {
        assert_eq!(sh.value_before("REV=", at), ours, "control: {label}");
        assert_eq!(sh.value_before("PROBE-", at), "NONE", "control: {label}");
        assert!(
            bf.pointer.exists(),
            "control: the file is left where it was"
        );
        return;
    }
    assert_eq!(sh.value_before("REV=", at), NEWER_REV, "{label}: taken");
    assert_eq!(
        sh.value_before("PROBE-", at),
        "NEWER",
        "{label}: the newer body"
    );
    assert!(
        !bf.pointer.exists(),
        "{label}: the pointer is removed once read"
    );
    assert_eq!(
        sh.value_before("PATHV=", at),
        path,
        "{label}: PATH unchanged"
    );
    // THE PROMPT THAT TOOK THE BODY: between the start of the command that ran
    // across the switch (its 133;C, emitted by the old body) and the first
    // revision the new body signs, that command's 133;D — the body was
    // re-sourced before the prompt handler ran, and the command's state survived
    // it; then the prompt start.
    let switch = stdout_between(sh, from, at);
    let newer = format!("633;P;{INTEGRATION_REV_KEY}={NEWER_REV};id={REKEY_OLD}");
    let signed_at = switch
        .find(&newer)
        .unwrap_or_else(|| panic!("{label}: the new body signs {newer}:\n{switch:?}"));
    let started_at = switch[..signed_at]
        .rfind(&format!("133;C;id={REKEY_OLD}"))
        .unwrap_or_else(|| panic!("{label}: the command's 133;C:\n{switch:?}"));
    assert!(
        switch[started_at..signed_at].contains(&format!("133;D;0;id={REKEY_OLD}")),
        "{label}: the command that ran across the switch keeps its 133;D:\n{switch:?}"
    );
    assert!(
        switch[signed_at..].contains(&format!("133;A;id={REKEY_OLD}")),
        "{label}: then the prompt starts:\n{switch:?}"
    );
    // And the marks keep flowing under the newer body.
    sh.send("true");
    let (line, marker) = body_probe(label, 5);
    sh.send(&line);
    let end = sh.wait_for_after(&marker, at);
    let after = stdout_between(sh, at, end);
    for mark in [
        format!("133;C;id={REKEY_OLD}"),
        format!("133;D;0;id={REKEY_OLD}"),
        format!("633;P;{INTEGRATION_REV_KEY}={NEWER_REV};id={REKEY_OLD}"),
    ] {
        assert!(after.contains(&mark), "{label}: {mark} after:\n{after:?}");
    }
    assert_eq!(sh.value_before("PROBE-", end), "NEWER", "{label}");
    // The hooks are the ones it had, none twice — read a prompt later, because
    // fish's body registers a ONE-SHOT fish_prompt handler (the first prompt's
    // managed-PATH assert) that a re-sourced body registers again and the next
    // prompt runs and erases.
    assert_eq!(
        sh.value_before("HOOKS=", end),
        hooks,
        "{label}: hooks unchanged"
    );
}

/// zsh ships with macOS (`/bin/zsh`), so there this is never skipped; elsewhere
/// the zsh lane is not claimed.
#[cfg(target_os = "macos")]
#[test]
fn a_live_zsh_takes_a_newer_body_at_its_next_prompt() {
    let zsh = zsh_shell().expect("macOS ships /bin/zsh");
    for hooked in [true, false] {
        let bf = BodyFixture::new();
        let pointer = bf.pointer.to_str().unwrap().to_string();
        let mut extra = vec![("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")];
        if hooked {
            extra.push((BODY_POINTER_VAR, pointer.as_str()));
        }
        let mut sh = spawn_live_zsh_from(zsh, &bf.fx, &bf.ours, &extra);
        live_body_pickup("zsh", &mut sh, &bf, hooked);
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn a_live_bash_takes_a_newer_body_at_its_next_prompt() {
    for hooked in [true, false] {
        let bf = BodyFixture::new();
        let pointer = bf.pointer.to_str().unwrap().to_string();
        let mut extra = vec![("ATERM_SHELL_NONCE", REKEY_OLD)];
        if hooked {
            extra.push((BODY_POINTER_VAR, pointer.as_str()));
        }
        let mut sh = spawn_live_bash_from(&bf.fx, &bf.ours, &extra);
        live_body_pickup("bash", &mut sh, &bf, hooked);
        sh.finish();
    }
}

/// The pre-loader scripts every shell spawned before 2026-09-26 runs (main at
/// 087a969af): the shells no body pointer reaches.
#[cfg(unix)]
const PRELOADER_ZSH: &str = include_str!("fixtures/preloader-087a969af.zsh");
#[cfg(unix)]
const PRELOADER_BASH: &str = include_str!("fixtures/preloader-087a969af.bash");
#[cfg(unix)]
const PRELOADER_FISH: &str = include_str!("fixtures/preloader-087a969af.fish");

/// A folder holding the PRE-LOADER scripts as a build of that day laid them out:
/// the scripts and their wrappers, and no body.
#[cfg(unix)]
fn preloader_folder(fx: &LiveFixture) -> PathBuf {
    let old = fx.base.join("preloader");
    std::fs::create_dir_all(old.join("zdotdir")).unwrap();
    std::fs::create_dir_all(old.join("bash")).unwrap();
    std::fs::write(old.join("aterm_shell_integration.zsh"), PRELOADER_ZSH).unwrap();
    std::fs::write(old.join("aterm_shell_integration.bash"), PRELOADER_BASH).unwrap();
    std::fs::write(old.join("zdotdir/.zshenv"), ZSH_WRAPPER).unwrap();
    std::fs::write(old.join("bash/rcfile"), BASH_WRAPPER).unwrap();
    let conf_d = old.join("fish-xdg/fish/vendor_conf.d");
    std::fs::create_dir_all(&conf_d).unwrap();
    std::fs::write(old.join("aterm_shell_integration.fish"), PRELOADER_FISH).unwrap();
    std::fs::write(conf_d.join("aterm_shell_integration.fish"), PRELOADER_FISH).unwrap();
    old
}

/// A LIVE SHELL FROM BEFORE LOADERS — spawned from main's pre-loader script,
/// with its nonce — takes the typed upgrade: before the line it signs no
/// revision (the negative control); the window's one-use file holds the key it
/// already signs with, this build's folder and its body pointer; after the line
/// (whose command still runs) the file is gone, the shell signs THIS build's
/// revision with the SAME key, its prompt hooks are the ones it had, and — the
/// point of it — a body pointer written later is taken at its next prompt like
/// any loader shell's. A second line naming a file that is gone changes
/// nothing, prints nothing, and the command after it runs.
#[cfg(unix)]
fn live_typed_upgrade(label: &str, shell: ShellType, sh: &mut LiveShell, bf: &BodyFixture) {
    let (line, marker) = body_probe(label, 1);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, 0);
    assert_eq!(
        sh.value_before("REV=", at),
        "",
        "{label}: no revision before"
    );
    let hooks = sh.value_before("HOOKS=", at);
    assert!(
        !stdout_between(sh, 0, at).contains(INTEGRATION_REV_KEY),
        "{label}: a pre-loader shell signs no revision (the negative control)"
    );

    let file = bf.fx.home.join("re key's");
    std::fs::write(
        &file,
        format!(
            "{REKEY_OLD}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey_with_loader(shell, &posix_quoted(&file)).expect("scripted");
    let sent = at;
    sh.send(&format!("{typed} {}", ran(label, 201)));
    let at = sh.wait_for_after("RAN201", at);
    // SIGNED AT THE UPGRADE, before the command the line carries runs (review
    // finding 2026-09-26): that command is the relaunched agent, which holds the
    // terminal for hours — and until the shell's next prompt the window read
    // `integration_rev=frozen` for a shell that now has a loader, and the next
    // update carried no revision to point it by.
    let signed = format!(
        "633;P;{INTEGRATION_REV_KEY}={};id={REKEY_OLD}",
        bf.ours_rev()
    );
    assert!(
        stdout_between(sh, sent, at - "RAN201".len()).contains(&signed),
        "{label}: the upgrade signs its revision before the command runs\n{}",
        sh.transcript()
    );
    let (line, marker) = body_probe(label, 2);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    assert_eq!(
        sh.value_before("REV=", at),
        bf.ours_rev(),
        "{label}: upgraded"
    );
    assert!(!file.exists(), "{label}: the one-use file is gone");
    assert_eq!(
        sh.value_before("HOOKS=", at),
        hooks,
        "{label}: the same hooks"
    );
    let (line, marker) = body_probe(label, 3);
    sh.send(&line);
    let end = sh.wait_for_after(&marker, at);
    let after = stdout_between(sh, at, end);
    for mark in [
        format!(
            "633;P;{INTEGRATION_REV_KEY}={};id={REKEY_OLD}",
            bf.ours_rev()
        ),
        format!("133;A;id={REKEY_OLD}"),
        format!("133;C;id={REKEY_OLD}"),
    ] {
        assert!(after.contains(&mark), "{label}: {mark} after:\n{after:?}");
    }

    // The pointer it was handed works like any loader shell's.
    std::fs::write(&bf.pointer, format!("{NEWER_REV}\n")).unwrap();
    sh.send("true");
    let (line, marker) = body_probe(label, 4);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, end);
    assert_eq!(
        sh.value_before("REV=", at),
        NEWER_REV,
        "{label}: pointer taken"
    );
    assert_eq!(sh.value_before("PROBE-", at), "NEWER", "{label}");

    // A withdrawn file: nothing changes, quietly, and the command runs.
    let gone = bf.fx.home.join("withdrawn");
    let typed = typed_rekey_with_loader(shell, &posix_quoted(&gone)).expect("scripted");
    let from = at;
    sh.send(&format!("{typed} {}", ran(label, 301)));
    let at = sh.wait_for_after("RAN301", at);
    let (line, marker) = body_probe(label, 5);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    assert_eq!(sh.value_before("REV=", at), NEWER_REV, "{label}: unchanged");
    // stderr for zsh and bash; fish runs on a pty, where it is the screen.
    let err = format!("{}{}", sh.stderr_text(), stdout_between(sh, from, at)).to_lowercase();
    for noise in ["no such file", "does not exist", "while redirecting"] {
        assert!(!err.contains(noise), "{label}: quiet: {err}");
    }
    assert!(!err.contains("unbound"), "{label}: set -u clean: {err}");
}

#[cfg(target_os = "macos")]
#[test]
fn a_live_zsh_from_before_loaders_is_upgraded_by_the_typed_line() {
    let zsh = zsh_shell().expect("macOS ships /bin/zsh");
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    let mut sh = spawn_live_zsh_from(
        zsh,
        &bf.fx,
        &old,
        &[("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")],
    );
    live_typed_upgrade("zsh", ShellType::Zsh, &mut sh, &bf);
    sh.finish();
}

/// Under the user's `set -u` too: every variable the typed line reads is
/// `-`-guarded, so the line cannot abort before the command it carries.
#[cfg(unix)]
#[test]
fn a_live_bash_from_before_loaders_is_upgraded_by_the_typed_line() {
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    std::fs::write(bf.fx.home.join(".bashrc"), "set -u\n").unwrap();
    let mut sh = spawn_live_bash_from(&bf.fx, &old, &[("ATERM_SHELL_NONCE", REKEY_OLD)]);
    live_typed_upgrade("bash", ShellType::Bash, &mut sh, &bf);
    sh.finish();
}

/// The KEY-ONLY text an older sweep types reads the first line of the new
/// three-line file and nothing else. (The window writes the key a healthy shell
/// already signs with there, so that text never takes an empty line for a key.)
#[cfg(unix)]
#[test]
fn the_key_only_line_takes_the_first_line_of_a_three_line_file() {
    let bf = BodyFixture::new();
    let mut sh = spawn_live_bash_from(&bf.fx, &bf.ours, &[("ATERM_SHELL_NONCE", REKEY_OLD)]);
    let file = bf.fx.home.join("three");
    std::fs::write(
        &file,
        format!(
            "{REKEY_NEW}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey(ShellType::Bash, &posix_quoted(&file)).expect("scripted");
    sh.send(&format!("{typed} echo RAN$((200 + 1))"));
    let at = sh.wait_for_after("RAN201", 0);
    let (line, marker) = rekey_probe("bash", 1);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    assert_eq!(sh.value_before("NONCE=", at), REKEY_NEW);
    assert!(!file.exists());
    sh.finish();
}

/// A NESTED SHELL is never integrated by the typed line (review finding
/// 2026-09-26). A shell started from an integrated tab — a `bash` or `zsh`
/// typed there, a `nix develop` / `pipenv shell` — inherits the EXPORTED guard,
/// so its own load stopped at the guard, and it holds no
/// `__aterm_shell_nonce`. When the agent it launched is relaunched, the harness
/// types the upgrade line into IT (it leads the tab's foreground group), with
/// the file the window issued for the tab's OWN shell. The line used to assign
/// the key to `__aterm_shell_nonce` BEFORE sourcing the loader — which then
/// read that very variable as proof the shell was its own, and upgraded the
/// nested shell "in place": a zsh got every hook wired, a bash had the body run
/// at its top level (its PATH re-fronted to the reroute dir, ahead of what the
/// nested environment put first), and the first revision the nested zsh signed
/// read `integration_rev=current` for a tab whose own shell was still frozen —
/// which also shut the window's upgrade off for it for good. The loader is
/// sourced only by a shell that held the nonce BEFORE the line ran.
#[cfg(unix)]
fn nested_shell_is_left_alone(label: &str, shell: ShellType, sh: &mut LiveShell, bf: &BodyFixture) {
    let (line, marker) = body_probe(label, 1);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, 0);
    let (hooks, path) = (sh.value_before("HOOKS=", at), sh.value_before("PATHV=", at));
    assert_eq!(sh.value_before("REV=", at), "", "{label}: nested, no body");

    let file = bf.fx.home.join("tab's key");
    std::fs::write(
        &file,
        format!(
            "{REKEY_OLD}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey_with_loader(shell, &posix_quoted(&file)).expect("scripted");
    sh.send(&format!("{typed} {}", ran(label, 201)));
    let at = sh.wait_for_after("RAN201", at);
    let (line, marker) = body_probe(label, 2);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    assert!(
        !file.exists(),
        "{label}: the one-use file is still consumed"
    );
    assert_eq!(
        sh.value_before("REV=", at),
        "",
        "{label}: no body sourced into a nested shell\n{}",
        sh.transcript()
    );
    assert_eq!(
        sh.value_before("HOOKS=", at),
        hooks,
        "{label}: no hook wired"
    );
    assert_eq!(
        sh.value_before("PATHV=", at),
        path,
        "{label}: PATH as the nested environment set it"
    );
    assert!(
        !stdout_between(sh, 0, at).contains(INTEGRATION_REV_KEY),
        "{label}: no revision signed\n{}",
        sh.transcript()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_nested_zsh_is_not_integrated_by_the_typed_line() {
    let zsh = zsh_shell().expect("macOS ships /bin/zsh");
    let bf = BodyFixture::new();
    bf.fx.lay_dirs();
    let path = format!("/usr/bin:/bin:{}", bf.fx.reroute.display());
    let mut cmd = shell_command(zsh);
    cmd.args(["-f", "-i", "-o", "NO_ZLE", "-o", "NO_MONITOR"])
        .env("HOME", &bf.fx.home)
        .env("PATH", &path)
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1")
        .env("ATERM_REROUTE_DIR", &bf.fx.reroute)
        .env("ATERM_SHELL_INTEGRATION_INSTALLED", "1");
    let mut sh = LiveShell::spawn("zsh", &mut cmd);
    nested_shell_is_left_alone("zsh", ShellType::Zsh, &mut sh, &bf);
    sh.finish();
}

#[cfg(unix)]
#[test]
fn a_nested_bash_is_not_integrated_by_the_typed_line() {
    let bf = BodyFixture::new();
    bf.fx.lay_dirs();
    let path = format!("/usr/bin:/bin:{}", bf.fx.reroute.display());
    let mut cmd = shell_command(bash_shell());
    cmd.args(["--norc", "--noprofile", "-i"])
        .env("HOME", &bf.fx.home)
        .env("PATH", &path)
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1")
        .env("ATERM_REROUTE_DIR", &bf.fx.reroute)
        .env("ATERM_SHELL_INTEGRATION_INSTALLED", "1")
        .env_remove("BASH_ENV");
    let mut sh = LiveShell::spawn("bash", &mut cmd);
    nested_shell_is_left_alone("bash", ShellType::Bash, &mut sh, &bf);
    sh.finish();
}

// ─── fish (2026-09-26 review: the lanes above, in fish) ───

/// An interactive fish started as a tab is — its conf.d on XDG_DATA_DIRS from
/// the folder `dir` — on a pty (`spawn_live_fish` says why), with `extra` in
/// its environment; `dir` = `None` is a fish with no integration of its own.
#[cfg(unix)]
fn spawn_live_fish_from(
    fish: &str,
    fx: &LiveFixture,
    dir: Option<&Path>,
    extra: &[(&str, &str)],
) -> LiveShell {
    std::fs::create_dir_all(fx.home.join("config").join("fish")).expect("config dir");
    let mut cmd = Command::new("script");
    if cfg!(target_os = "macos") {
        cmd.args(["-q", "/dev/null", fish, "-i"]);
    } else {
        cmd.args(["-q", "-c", &format!("{fish} -i"), "/dev/null"]);
    }
    let hermetic = shell_command(fish);
    for (key, value) in hermetic.get_envs() {
        match value {
            Some(value) => {
                cmd.env(key, value);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    cmd.env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.home.join("config"))
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1");
    let data = hermetic_home().join("data");
    match dir {
        Some(dir) => {
            let InjectionEnv { env_add, .. } = injection_for(ShellType::Fish, dir).expect("fish");
            for (key, value) in env_add {
                if key == "XDG_DATA_DIRS" {
                    cmd.env(
                        key,
                        format!("{}:{}", dir.join("fish-xdg").display(), data.display()),
                    );
                } else {
                    cmd.env(key, value);
                }
            }
        }
        None => {
            cmd.env("XDG_DATA_DIRS", &data);
        }
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("fish", &mut cmd)
}

/// The fish lanes need fish and a pty. Like every other live fish test in this
/// crate, a host without them runs none of them — and says so.
#[cfg(unix)]
fn live_fish() -> Option<&'static str> {
    let Some(fish) = fish_shell() else {
        eprintln!("fish not installed; skipping (ATERM_TEST_FISH=<fish> runs it)");
        return None;
    };
    if !script_can_allocate_a_pty() {
        eprintln!("no `script` to allocate a pty; skipping");
        return None;
    }
    Some(fish)
}

#[cfg(unix)]
#[test]
fn a_live_fish_takes_a_newer_body_at_its_next_prompt() {
    let Some(fish) = live_fish() else { return };
    for hooked in [true, false] {
        let bf = BodyFixture::new();
        let pointer = bf.pointer.to_str().unwrap().to_string();
        let mut extra = vec![("ATERM_SHELL_NONCE", REKEY_OLD)];
        if hooked {
            extra.push((BODY_POINTER_VAR, pointer.as_str()));
        }
        let mut sh = spawn_live_fish_from(fish, &bf.fx, Some(&bf.ours), &extra);
        live_body_pickup("fish", &mut sh, &bf, hooked);
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn a_live_fish_from_before_loaders_is_upgraded_by_the_typed_line() {
    let Some(fish) = live_fish() else { return };
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    let mut sh = spawn_live_fish_from(
        fish,
        &bf.fx,
        Some(&old),
        &[("ATERM_SHELL_NONCE", REKEY_OLD)],
    );
    live_typed_upgrade("fish", ShellType::Fish, &mut sh, &bf);
    sh.finish();
}

#[cfg(unix)]
#[test]
fn a_nested_fish_is_not_integrated_by_the_typed_line() {
    let Some(fish) = live_fish() else { return };
    let bf = BodyFixture::new();
    bf.fx.lay_dirs();
    let reroute = bf.fx.reroute.to_str().unwrap().to_string();
    let mut sh = spawn_live_fish_from(
        fish,
        &bf.fx,
        None,
        &[
            ("ATERM_SHELL_INTEGRATION_INSTALLED", "1"),
            ("ATERM_REROUTE_DIR", reroute.as_str()),
            ("PATH", &format!("/usr/bin:/bin:{reroute}")),
        ],
    );
    nested_shell_is_left_alone("fish", ShellType::Fish, &mut sh, &bf);
    sh.finish();
}

// ─── A body re-sourced keeps the user's key bindings (2026-09-26 review) ───

/// The line that binds Ctrl+Right to a widget of the USER's choosing, and the
/// probe that prints what it is bound to now, in `label`'s own syntax.
#[cfg(unix)]
fn user_binding(label: &str, n: u32) -> (&'static str, String, String) {
    if label == "fish" {
        (
            r"bind \e\[1\;5C nextd-or-forward-word",
            format!(r"printf 'BIND=%s\n' (bind \e\[1\;5C); echo BK(math 400 + {n})"),
            format!("BK{}", 400 + n),
        )
    } else {
        (
            "bindkey '\\e[1;5C' emacs-forward-word",
            format!("printf 'BIND=%s\\n' \"$(bindkey '\\e[1;5C')\"; echo BK$((400 + {n}))"),
            format!("BK{}", 400 + n),
        )
    }
}

/// A NEWER BODY TAKEN AT A PROMPT LEAVES THE USER'S KEY BINDINGS ALONE. The
/// body binds a handful of keys (Alt/Ctrl+arrows, Home/End, Delete,
/// Shift+Up/Down) so their sequences never leak as text — ONCE, at the shell's
/// first load, which zsh (`.zshenv`) and fish (vendor conf.d) run BEFORE the
/// user's own rc, so a binding of the user's wins. A body re-sourced live used
/// to bind them again, at the prompt that took it: every update took those keys
/// back from the user in every live tab. The binding is the loader's now, on a
/// fresh load only.
#[cfg(unix)]
fn live_pickup_keeps_user_bindings(label: &str, sh: &mut LiveShell, bf: &BodyFixture) {
    // The fresh load binds the key itself (the loader's once-per-shell call).
    let (bind, probe, marker) = user_binding(label, 0);
    sh.send(&probe);
    let at = sh.wait_for_after(&marker, 0);
    let ours = sh.value_before("BIND=", at);
    assert!(
        ours.ends_with(" forward-word") && !ours.contains("-forward-word"),
        "{label}: a fresh load binds Ctrl+Right to forward-word: {ours:?}"
    );
    sh.send(bind);
    let (_, probe, marker) = user_binding(label, 1);
    sh.send(&probe);
    let at = sh.wait_for_after(&marker, at);
    let mine = sh.value_before("BIND=", at);
    assert!(
        mine.contains("forward-word") && mine != "forward-word",
        "{label}: the user's own binding is in place: {mine:?}"
    );
    std::fs::write(&bf.pointer, format!("{NEWER_REV}\n")).unwrap();
    sh.send("true");
    let (line, marker) = body_probe(label, 2);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    assert_eq!(sh.value_before("REV=", at), NEWER_REV, "{label}: taken");
    let (_, probe, marker) = user_binding(label, 3);
    sh.send(&probe);
    let at = sh.wait_for_after(&marker, at);
    assert_eq!(
        sh.value_before("BIND=", at),
        mine,
        "{label}: the user's binding survives the newer body"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_live_zsh_keeps_the_users_key_bindings_across_a_newer_body() {
    let zsh = zsh_shell().expect("macOS ships /bin/zsh");
    let bf = BodyFixture::new();
    let pointer = bf.pointer.to_str().unwrap().to_string();
    let mut sh = spawn_live_zsh_from(
        zsh,
        &bf.fx,
        &bf.ours,
        &[
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            ("ATERM_CHILD", "1"),
            (BODY_POINTER_VAR, pointer.as_str()),
        ],
    );
    live_pickup_keeps_user_bindings("zsh", &mut sh, &bf);
    sh.finish();
}

#[cfg(unix)]
#[test]
fn a_live_fish_keeps_the_users_key_bindings_across_a_newer_body() {
    let Some(fish) = live_fish() else { return };
    let bf = BodyFixture::new();
    let pointer = bf.pointer.to_str().unwrap().to_string();
    let mut sh = spawn_live_fish_from(
        fish,
        &bf.fx,
        Some(&bf.ours),
        &[
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            (BODY_POINTER_VAR, pointer.as_str()),
        ],
    );
    live_pickup_keeps_user_bindings("fish", &mut sh, &bf);
    sh.finish();
}

// ─── An upgrade in place leaves the zle-line-init chain alone (2026-09-26 review) ───

/// A LIVE zsh WITH ZLE — on a pty (`script`), from the PRE-LOADER script in
/// `old`, whose `.zshrc` hooks `zle-line-init` the way modern plugins do
/// (`add-zle-hook-widget`, which keeps aterm's widget as the first of its
/// hooks) — takes the typed upgrade, and the next prompts each emit ONE 133;B.
///
/// The loader used to re-wire `zle-line-init` on an upgrade in place: seeing a
/// widget that was not its own, it aliased that widget — the plugin's
/// dispatcher, which calls aterm's widget — to `__aterm_orig_zle_line_init`,
/// then installed its own, which calls that alias: a loop. Measured on zsh 5.9:
/// ~250 133;B marks and `azhw:zle-line-init:9: job table full or recursion
/// limit exceeded` at EVERY prompt of the upgraded shell. A widget names its
/// function, so the chain the shell already has reaches the new body's
/// `__aterm_zle_line_init` untouched; the wiring is a fresh load's only.
#[cfg(target_os = "macos")]
#[test]
fn a_live_zsh_with_a_line_init_hook_is_upgraded_without_a_widget_loop() {
    let zsh = zsh_shell().expect("macOS ships /bin/zsh");
    assert!(script_can_allocate_a_pty(), "macOS ships script(1)");
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    std::fs::write(
        bf.fx.home.join(".zshrc"),
        "PS1='P> '\nautoload -Uz add-zle-hook-widget\n__user_init() { : }\nzle -N __user_init\n\
         add-zle-hook-widget line-init __user_init\n",
    )
    .expect(".zshrc");
    let InjectionEnv { env_add, .. } = injection_for(ShellType::Zsh, &old).expect("zsh");
    let mut cmd = Command::new("script");
    cmd.args(["-q", "/dev/null", zsh, "-i"]);
    let hermetic = shell_command(zsh);
    for (key, value) in hermetic.get_envs() {
        match value {
            Some(value) => {
                cmd.env(key, value);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    cmd.env("HOME", &bf.fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("ATERM_CHILD", "1")
        .env("ATERM_SHELL_NONCE", REKEY_OLD);
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    cmd.env_remove("ATERM_UNSET_ZDOTDIR")
        .env("ATERM_ORIGINAL_ZDOTDIR", &bf.fx.home);
    let mut sh = LiveShell::spawn("zsh", &mut cmd);
    let b_mark = format!("133;B;id={REKEY_OLD}");

    sh.send("echo UP$((10 + 1))");
    let at = sh.wait_for_after("UP11", 0);
    let file = bf.fx.home.join("key");
    std::fs::write(
        &file,
        format!(
            "{REKEY_OLD}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey_with_loader(ShellType::Zsh, &posix_quoted(&file)).expect("scripted");
    sh.send(&format!("{typed} {}", ran("zsh", 201)));
    let at = sh.wait_for_after("RAN201", at);
    sh.send("printf 'REV=%s\\n' \"$__aterm_body_rev\"; echo NX$((20 + 2))");
    let at = sh.wait_for_after("NX22", at);
    assert_eq!(sh.value_before("REV=", at), bf.ours_rev(), "upgraded");
    // One whole prompt cycle after the upgrade: its line-init, then a command.
    sh.send("echo NX$((30 + 3))");
    let end = sh.wait_for_after("NX33", at);
    let cycle = stdout_between(&sh, at, end);
    assert_eq!(
        cycle.matches(&b_mark).count(),
        1,
        "one 133;B for one prompt:\n{}",
        sh.transcript()
    );
    assert!(
        !cycle.contains("recursion limit"),
        "no widget loop:\n{cycle}"
    );
    sh.finish();
}

/// A LIVE fish from before loaders whose config.fish defines its OWN prompt
/// keeps it through the typed upgrade (review finding 2026-09-26). fish reads
/// the vendor conf.d — aterm's wrapper — BEFORE config.fish, so a prompt the
/// user defines there (a framework's, `starship init fish | source`) replaced
/// aterm's wrapper for that shell's whole life. The upgrade in place used to
/// redefine `fish_prompt` as the loader's trampoline regardless, and the user's
/// prompt was gone: the trampoline draws the prompt aterm copied at its first
/// load, before config.fish ran. It replaces only a prompt that is aterm's own.
#[cfg(unix)]
#[test]
fn a_live_fish_keeps_the_users_own_prompt_through_the_typed_upgrade() {
    let Some(fish) = live_fish() else { return };
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    let config = bf.fx.home.join("config").join("fish");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.fish"),
        "function fish_prompt\n    echo -n 'MYPROMPT> '\nend\n",
    )
    .expect("config.fish");
    let mut sh = spawn_live_fish_from(
        fish,
        &bf.fx,
        Some(&old),
        &[("ATERM_SHELL_NONCE", REKEY_OLD)],
    );
    let probe = |n: u32| {
        (
            format!(
                "functions fish_prompt | string match -q '*MYPROMPT*'; \
                 and echo MINE(math {n} + 500); or echo OTHER(math {n} + 500)"
            ),
            (format!("MINE{}", n + 500), format!("OTHER{}", n + 500)),
        )
    };
    let (line, (mine, _)) = probe(1);
    sh.send(&line);
    let at = sh.wait_for_after(&mine, 0);
    let file = bf.fx.home.join("key");
    std::fs::write(
        &file,
        format!(
            "{REKEY_OLD}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey_with_loader(ShellType::Fish, &posix_quoted(&file)).expect("scripted");
    sh.send(&format!("{typed} {}", ran("fish", 201)));
    let at = sh.wait_for_after("RAN201", at);
    let (line, (mine, other)) = probe(2);
    sh.send(&line);
    sh.send("echo DONE(math 600 + 1)");
    let end = sh.wait_for_after("DONE601", at);
    let after = stdout_between(&sh, at, end);
    assert!(
        after.contains(&mine) && !after.contains(&other),
        "the user's own prompt survives the upgrade:\n{}",
        sh.transcript()
    );
    sh.finish();
}
