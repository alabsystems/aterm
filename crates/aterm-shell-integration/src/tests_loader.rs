// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// ─── The loader and the body ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests.rs, whose
// helpers (`LiveShell`, `LiveFixture`, `shell_command`, the re-key probes
// and `deliver`) it uses.

/// A folder address this suite's builds never write: the newer build the live tests point
/// a running shell at.
#[cfg(unix)]
const NEWER_REV: &str = "0123456789abcdef";

/// The script folder carries each body beside its loader, as the loader's own
/// text under a header — so the body a running shell takes from a folder is the
/// body a new shell of that build runs — and a body is part of what the
/// folder's address names.
#[test]
fn the_script_set_carries_each_body_beside_its_loader() {
    let dir = aterm_tempfile::tempdir().unwrap();
    ensure_scripts(dir.path()).unwrap();
    for ext in ["zsh", "bash", "fish"] {
        let loader =
            std::fs::read_to_string(dir.path().join(format!("aterm_shell_integration.{ext}")))
                .unwrap();
        let written =
            std::fs::read_to_string(dir.path().join(format!("{BODY_FILE_STEM}.{ext}"))).unwrap();
        assert_eq!(
            written,
            format!("{BODY_FILE_HEADER}{}", scripts::body(&loader).unwrap()),
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

/// What a real `shell` reads `script` as: its canonical text, the script read as
/// the body of one function (`functions` in zsh, `declare -f` in bash), which
/// keeps every word and every string and drops every comment.
#[cfg(unix)]
fn canonical_text(shell: &str, script: &str) -> String {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("script");
    std::fs::write(&path, script).unwrap();
    let zsh = std::path::Path::new(shell)
        .file_name()
        .is_some_and(|name| name == "zsh");
    let (flags, print): (&[&str], &str) = if zsh {
        (&["-f", "-c"], "print -r -- \"${functions[__aterm_canon]}\"")
    } else {
        (&["--norc", "--noprofile", "-c"], "declare -f __aterm_canon")
    };
    let driver = format!("src=$(<\"$1\")\neval \"__aterm_canon() {{\n$src\n}}\"\n{print}\n");
    let output = shell_command(shell)
        .args(flags)
        .arg(&driver)
        .arg(shell)
        .arg(&path)
        .output()
        .unwrap_or_else(|error| panic!("spawn {shell} to read a script: {error}"));
    assert!(
        output.status.success() && output.stderr.is_empty(),
        "{shell} could not read the script: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("canonical text is UTF-8")
}

/// A script folder holds the zsh and bash loaders, and so their bodies, without
/// their comment lines (`strip_comment_lines`): the shell reads every line it
/// sources, and two thirds of these are comment. That is safe only while no
/// comment line sits where the shell reads it as text, and the shells say so
/// themselves: each reads its script with and without the comment lines to the
/// same canonical text. The control is a `#` line inside a string that spans
/// lines, which the same reading tells apart — so a script that ever puts one
/// there fails here, before a folder ships it.
#[cfg(unix)]
#[test]
fn installed_scripts_run_the_code_of_their_source() {
    let shells = [
        ("bash", bash_shell(), scripts::BASH, installed_bash()),
        ("zsh", need_zsh(), scripts::ZSH, installed_zsh()),
    ];
    for (label, shell, source, installed) in shells {
        let comment = |line: &&str| line.trim_start().starts_with('#');
        assert!(
            installed
                .lines()
                .skip(1)
                .filter(comment)
                .all(|line| line.starts_with(scripts::BODY_BEGIN)
                    || line.starts_with(scripts::BODY_END)),
            "{label}: the folder's loader keeps only its BODY markers of its comment lines"
        );
        assert_eq!(
            canonical_text(shell, installed),
            canonical_text(shell, source),
            "{label}: the folder's loader runs the source's code"
        );
        assert_eq!(
            scripts::body(installed).map(|body| canonical_text(shell, body)),
            scripts::body(source).map(|body| canonical_text(shell, body)),
            "{label}: the folder's body runs the source's body's code"
        );
        let control = "x='first\n# a line of the string\nlast'\n";
        assert_ne!(
            canonical_text(shell, &strip_comment_lines(control)),
            canonical_text(shell, control),
            "{label}: a `#` line inside a string is text, and the reading sees it go"
        );
    }
}

/// A folder's zsh loader and body, compiled (`compile_zsh_scripts`), are what a
/// zsh then sources: the wordcode beside each script, in place of its text. The
/// control is the same folder before the compile, where the text runs. The text
/// of the body is replaced by a probe, older than the wordcode, so which of the
/// two ran shows; the loader's wordcode is checked to be there. A second
/// compile keeps what the first made.
#[cfg(unix)]
#[test]
fn a_compiled_zsh_set_is_sourced_from_its_wordcode() {
    let zsh = need_zsh();
    let dir = aterm_tempfile::tempdir().unwrap();
    ensure_scripts(dir.path()).unwrap();
    let body = dir.path().join(format!("{BODY_FILE_STEM}.zsh"));
    let probe = |path: &std::path::Path| {
        let out = shell_command(zsh)
            .args([
                "-f",
                "-c",
                "source \"$1\"; print -r -- \"[defined=${+functions[__aterm_body_precmd]}]\"",
            ])
            .arg("zsh")
            .arg(path)
            .output()
            .unwrap_or_else(|error| panic!("spawn zsh to source the body: {error}"));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let old_text = |path: &std::path::Path| {
        std::fs::write(path, "print -r -- '[text]'\n").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(old)
            .unwrap();
    };
    // Control: nothing compiled, so the text is what runs.
    let control = aterm_tempfile::tempdir().unwrap();
    ensure_scripts(control.path()).unwrap();
    let control_body = control.path().join(format!("{BODY_FILE_STEM}.zsh"));
    old_text(&control_body);
    assert_eq!(probe(&control_body), "[text]\n[defined=0]\n");

    compile_zsh_scripts(dir.path(), std::path::Path::new(zsh)).unwrap();
    let loader_zwc = dir.path().join("aterm_shell_integration.zsh.zwc");
    let made = std::fs::metadata(&loader_zwc).unwrap().modified().unwrap();
    old_text(&body);
    assert_eq!(
        probe(&body),
        "[defined=1]\n",
        "the compiled body runs, not its text"
    );
    compile_zsh_scripts(dir.path(), std::path::Path::new(zsh)).unwrap();
    assert_eq!(
        std::fs::metadata(&loader_zwc).unwrap().modified().unwrap(),
        made,
        "a set already compiled is kept"
    );
}

/// A real zsh sources the body alone without an error, from inside a function under
/// `emulate -L zsh` (as the check sources it) and at top level, and the body defines what
/// the loader's trampoline and wiring name.
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

/// Sourcing the live body (in zsh, bash, and fish, as `__aterm_body_check` does
/// on an adopted shell's next prompt) exports `AGY_CLI_FAST_PROMPT_EDITOR=true`
/// to child processes when unset, while preserving an explicit user override.
#[cfg(unix)]
#[test]
fn live_body_exports_agy_fast_prompt_editor_default_without_clobbering_override() {
    let dir = aterm_tempfile::tempdir().unwrap();
    ensure_scripts(dir.path()).unwrap();
    let mut cases = vec![
        (
            "zsh",
            need_zsh(),
            "-f",
            ". \"$1\" && /usr/bin/printenv AGY_CLI_FAST_PROMPT_EDITOR",
        ),
        (
            "bash",
            bash_shell(),
            "--norc",
            ". \"$1\" && /usr/bin/printenv AGY_CLI_FAST_PROMPT_EDITOR",
        ),
    ];
    if let Some(fish) = fish_shell() {
        cases.push((
            "fish",
            fish,
            "--no-config",
            "source \"$argv[2]\"; and /usr/bin/printenv AGY_CLI_FAST_PROMPT_EDITOR",
        ));
    }
    for (ext, shell, flag, snippet) in cases {
        let body = dir.path().join(format!("{BODY_FILE_STEM}.{ext}"));
        let default_out = shell_command(shell)
            .args([flag, "-c", snippet, ext])
            .arg(&body)
            .env("HOME", dir.path())
            .output()
            .unwrap();
        assert!(
            default_out.status.success(),
            "{ext} default failed: {}",
            String::from_utf8_lossy(&default_out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&default_out.stdout).trim(),
            "true",
            "{ext} body must export AGY_CLI_FAST_PROMPT_EDITOR=true when unset"
        );

        let override_out = shell_command(shell)
            .args([flag, "-c", snippet, ext])
            .arg(&body)
            .env("HOME", dir.path())
            .env("AGY_CLI_FAST_PROMPT_EDITOR", "false")
            .output()
            .unwrap();
        assert!(
            override_out.status.success(),
            "{ext} override failed: {}",
            String::from_utf8_lossy(&override_out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&override_out.stdout).trim(),
            "false",
            "{ext} body must preserve explicit AGY_CLI_FAST_PROMPT_EDITOR=false"
        );
    }
}

/// The body of this build under a cache root, plus a newer build's folder whose body is
/// this build's with one probe function spliced in: what a running shell finds under its
/// root after an update.
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
/// Its callers are the macOS-only zsh rows (macOS ships `/bin/zsh`).
#[cfg(target_os = "macos")]
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
    LiveShell::spawn("zsh", &mut cmd).holding(cmd.into_home())
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
        .env("ATERM_CHILD", "1");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("bash", &mut cmd).holding(cmd.into_home())
}

/// The probe function the newer body carries (POSIX function syntax, so the same text
/// serves zsh and bash).
#[cfg(unix)]
const BODY_PROBE_FN: &str = "__aterm_body_probe() { printf '%s\\n' PROBE-NEWER; }";

/// [`BODY_PROBE_FN`] in fish. It reads `$__aterm_last_status` with `test`, as earlier bodies'
/// custom prompts do, so it fails if this body leaves that a list (which stops an older body
/// a shell is pointed back to with a `test` error).
#[cfg(unix)]
const BODY_PROBE_FN_FISH: &str = "function __aterm_body_probe; test $__aterm_last_status -ge 0; and printf '%s\\n' PROBE-NEWER; end";

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
        // fish's hooks are its event handlers: a re-sourced body must not register one
        // twice. Sorted, since fish lists an event's handlers in definition order.
        return (
            format!(
                "printf 'REV=%s\\n' \"$__aterm_body_rev\"; functions -q __aterm_body_probe; \
                 and __aterm_body_probe; or printf '%s\\n' PROBE-NONE; \
                 printf 'HOOKS=%s\\n' (functions --handlers-type generic | sort | string join ,); \
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

/// The prompt strings in `text` a shell signed with `key`, each from a signed
/// 133;A to the signed 133;B after it — or None when its signed marks do not
/// alternate A, B (a mark written twice, or one missing).
#[cfg(unix)]
fn signed_prompts(text: &str, key: &str) -> Option<Vec<String>> {
    let (a, b) = (format!("133;A;id={key}"), format!("133;B;id={key}"));
    let mut marks: Vec<(usize, bool)> = text
        .match_indices(&a)
        .map(|(i, _)| (i, true))
        .chain(text.match_indices(&b).map(|(i, _)| (i, false)))
        .collect();
    marks.sort_unstable();
    let mut out = Vec::new();
    for pair in marks.chunks(2) {
        match pair {
            [(i, true), (j, false)] => out.push(text[i + a.len()..*j].to_string()),
            _ => return None,
        }
    }
    Some(out)
}

/// Drive one live shell through the body pointer: spawned from this build's folder, it
/// signs this build's revision; a malformed pointer and one naming a missing folder are
/// consumed and change nothing; a pointer naming the newer folder, written while the shell
/// idles, is taken at its next prompt (the newer body's probe exists, it signs the newer
/// revision, the command that ran across the switch still gets its 133;D, and the marks
/// keep flowing with the same key), while PATH and the prompt hooks stay as they were.
/// `hooked` = spawned with the pointer path; the negative control, without it, never reads
/// the file.
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
        deliver(&bf.pointer, bad);
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

    // The newer build, pointed at while the shell idles: only once the last probe's prompt
    // has emitted its 133;A (after that prompt's own body check), so that prompt cannot
    // read the pointer before the command below runs.
    let at = sh.wait_for_after(&format!("133;A;id={REKEY_OLD}"), at);
    deliver(&bf.pointer, NEWER_REV);
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
    // The prompt that took the body: between the 133;C of the command that ran across the
    // switch (from the old body) and the first revision the new body signs comes that
    // command's 133;D (the body was re-sourced before the prompt handler ran, and the
    // command's state survived it); then the prompt start.
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
    // The hooks are the ones it had, none twice. Read a prompt later: fish's body registers
    // a one-shot fish_prompt handler (the first prompt's managed-PATH assert) that a
    // re-sourced body registers again and the next prompt runs and erases.
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

/// The pre-loader scripts (fixtures): what a shell spawned before loaders existed runs,
/// which no body pointer reaches.
#[cfg(unix)]
const PRELOADER_ZSH: &str = include_str!("fixtures/preloader-087a969af.zsh");
#[cfg(unix)]
const PRELOADER_BASH: &str = include_str!("fixtures/preloader-087a969af.bash");
#[cfg(unix)]
const PRELOADER_FISH: &str = include_str!("fixtures/preloader-087a969af.fish");
#[cfg(any(unix, windows))]
const PRELOADER_PWSH: &str = include_str!("fixtures/preloader-087a969af.ps1");

/// The scripts of 613305828c's parent (57daa3707e), the last that put
/// `~/.aterm/bin` FIRST on PATH: LOADERS, as every build from 2026-09-26 to then
/// wrote them, which take a newer body at a prompt and keep their own folder as
/// `__aterm_si_dir`.
#[cfg(unix)]
const OLDER_LOADER_ZSH: &str = include_str!("fixtures/loader-57daa3707e.zsh");
#[cfg(unix)]
const OLDER_LOADER_BASH: &str = include_str!("fixtures/loader-57daa3707e.bash");
#[cfg(unix)]
const OLDER_LOADER_FISH: &str = include_str!("fixtures/loader-57daa3707e.fish");

/// The block those loaders put `~/.aterm/bin` first with, as zsh and bash spell
/// it and as fish does. 613305828c's loaders are theirs without it, byte for
/// byte in code: the first of the builds that put none there and set no mark
/// (v0.106.0 among them).
#[cfg(unix)]
const OLDER_PREPEND_POSIX: &str = "if (( __aterm_fresh_load )) && [ -d \"$HOME/.aterm/bin\" ]; then
    export PATH=\"$HOME/.aterm/bin:$PATH\"
fi
";
#[cfg(unix)]
const OLDER_PREPEND_FISH: &str =
    "if test \"$__aterm_fresh_load\" = 1; and test -d \"$HOME/.aterm/bin\"
    set -gx PATH \"$HOME/.aterm/bin\" $PATH
end
";

/// The folder addresses the older loaders are laid at, beside this build's
/// under one cache root: 57daa3707e's, which prepends, and 613305828c's, which
/// does not.
#[cfg(unix)]
const PREPENDING_REV: &str = "57daa3707e000000";
#[cfg(unix)]
const UNPREPENDING_REV: &str = "613305828c000000";

/// An older LOADER's folder under `fx`'s root, as its build laid it out: the
/// zsh and bash loaders without their comment lines, fish's whole, fish's again
/// in its vendor conf.d, and the wrappers (this build's, at the paths this build
/// starts the shells from, as [`preloader_folder`] lays them). `prepends`: the
/// 57daa3707e loaders; otherwise the same minus their prepend block
/// (613305828c's).
#[cfg(unix)]
fn older_loader_folder(fx: &LiveFixture, prepends: bool) -> PathBuf {
    let dir = fx.base.join(if prepends {
        PREPENDING_REV
    } else {
        UNPREPENDING_REV
    });
    let conf_d = dir.join("fish-xdg/fish/vendor_conf.d");
    for sub in [dir.join("zdotdir"), dir.join("bash"), conf_d.clone()] {
        std::fs::create_dir_all(sub).unwrap();
    }
    for (ext, script, block) in [
        ("zsh", OLDER_LOADER_ZSH, OLDER_PREPEND_POSIX),
        ("bash", OLDER_LOADER_BASH, OLDER_PREPEND_POSIX),
        ("fish", OLDER_LOADER_FISH, OLDER_PREPEND_FISH),
    ] {
        assert_eq!(script.matches(block).count(), 1, "{ext}: the prepend block");
        let script = if prepends {
            script.to_owned()
        } else {
            script.replacen(block, "", 1)
        };
        let installed = if ext == "fish" {
            script
        } else {
            strip_comment_lines(&script)
        };
        std::fs::write(
            dir.join(format!("aterm_shell_integration.{ext}")),
            &installed,
        )
        .unwrap();
        if ext == "fish" {
            std::fs::write(conf_d.join("aterm_shell_integration.fish"), &installed).unwrap();
        }
    }
    std::fs::write(dir.join("zdotdir/.zshenv"), ZSH_WRAPPER).unwrap();
    for loader in ["rcfile", BASH_LOADER_FILE] {
        std::fs::write(dir.join("bash").join(loader), BASH_WRAPPER).unwrap();
    }
    dir
}

/// A folder holding the pre-loader scripts as a build of that time laid them out: the
/// scripts and their wrappers, and no body.
#[cfg(unix)]
fn preloader_folder(fx: &LiveFixture) -> PathBuf {
    let old = fx.base.join("preloader");
    std::fs::create_dir_all(old.join("zdotdir")).unwrap();
    std::fs::create_dir_all(old.join("bash")).unwrap();
    std::fs::write(old.join("aterm_shell_integration.zsh"), PRELOADER_ZSH).unwrap();
    std::fs::write(old.join("aterm_shell_integration.bash"), PRELOADER_BASH).unwrap();
    std::fs::write(old.join("zdotdir/.zshenv"), ZSH_WRAPPER).unwrap();
    // The wrapper a build of that day started bash on, at the path this build
    // starts bash from (`bash/bashrc` off macOS).
    for loader in ["rcfile", BASH_LOADER_FILE] {
        std::fs::write(old.join("bash").join(loader), BASH_WRAPPER).unwrap();
    }
    let conf_d = old.join("fish-xdg/fish/vendor_conf.d");
    std::fs::create_dir_all(&conf_d).unwrap();
    std::fs::write(old.join("aterm_shell_integration.fish"), PRELOADER_FISH).unwrap();
    std::fs::write(conf_d.join("aterm_shell_integration.fish"), PRELOADER_FISH).unwrap();
    old
}

/// The start the body's AN OLDER SCRIPT'S `~/.aterm/bin` block takes a shell's
/// against: 2026-10-16 00:00 UTC, before which every script prepended. The
/// shells below start ten seconds either side of it, so a cut moved further
/// either way fails a case.
#[cfg(unix)]
const CUT: u64 = 1_792_108_800;

/// The clock the stand-ins below read: 2026-10-20 00:00 UTC, four days after
/// [`CUT`].
#[cfg(unix)]
const STAND_IN_NOW: u64 = CUT + 4 * 86_400;

/// A shell's age as the stand-in `ps -o etime=` reports it: four days less ten
/// seconds, so started ten seconds after [`CUT`] ...
#[cfg(unix)]
const STARTED_SINCE: &str = "3-23:59:50";

/// ... or four days and ten seconds, so started ten seconds before it.
#[cfg(unix)]
const STARTED_BEFORE: &str = "4-00:00:10";

/// Lay the fixture's bin before the shell starts. The actual loader's external
/// readers receive captured timestamps: the clock [`STAND_IN_NOW`], a shell
/// started since the cut ([`STARTED_SINCE`]), the folder born and changed 100 s
/// before [`CUT`], so before every shell here. No filesystem creation-time
/// convention or aging wait is used.
#[cfg(unix)]
fn lay_aterm_bin(fx: &LiveFixture) -> String {
    lay_aterm_bin_with_dates(fx, CUT - 100, CUT - 100, STARTED_SINCE)
}

/// Owned stand-ins for the timestamp readings the real cleanup `sh -c` asks:
/// the folder's `birth` and `modified` (`stat`), the clock (`date`,
/// [`STAND_IN_NOW`]) and the shell's `age` (`ps -o etime=`). Other invocations
/// still use the system tools. The old loader puts this bin first, so the same
/// actual PATH cleanup consumes these captured readings.
#[cfg(unix)]
fn lay_aterm_bin_with_dates(fx: &LiveFixture, birth: u64, modified: u64, age: &str) -> String {
    let bin = fx.home.join(".aterm").join("bin");
    std::fs::create_dir_all(&bin).expect("mk ~/.aterm/bin");
    write_executable(
        &bin.join("stat"),
        &format!(
            "#!/bin/sh\nif [ \"$#\" -eq 4 ] && [ \"$3\" = -- ] && [ \"$4\" = \"$HOME/.aterm/bin\" ]; then\n\
             case \"$1:$2\" in '-f:%B %m'|'-c:%W %Y') printf '%s\\n' '{birth} {modified}'; exit 0;; esac\n\
             fi\nexec /usr/bin/stat \"$@\"\n"
        ),
    );
    write_executable(
        &bin.join("date"),
        &format!(
            "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = +%s ]; then printf '%s\\n' {STAND_IN_NOW}; else exec /bin/date \"$@\"; fi\n"
        ),
    );
    write_executable(
        &bin.join("ps"),
        &format!(
            "#!/bin/sh\nif [ \"$#\" -eq 4 ] && [ \"$1:$2:$3\" = '-o:etime=:-p' ]; then printf '%s\\n' {age}; else exec /bin/ps \"$@\"; fi\n"
        ),
    );
    bin.to_str().expect("UTF-8 home").to_owned()
}

/// `path` with its first entry equal to `entry` taken off and nothing else
/// moved — what an older script's `~/.aterm/bin` leaves when it goes; `path`
/// itself when no entry is. Entries are split on the platform's PATH separator.
#[cfg(any(unix, windows))]
fn without_first(path: &str, entry: &str) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut entries: Vec<&str> = path.split(sep).collect();
    if let Some(at) = entries.iter().position(|e| *e == entry) {
        entries.remove(at);
    }
    entries.join(sep)
}

/// A LIVE SHELL FROM BEFORE LOADERS — spawned from main's pre-loader script,
/// with its nonce — takes the typed upgrade: before the line it signs no
/// revision (the negative control); the window's one-use file holds a FRESH
/// key (the heal of a DEGRADED tab, whose key the engine lost; a healthy tab's
/// file hands back the key it already signs with), this build's folder and its
/// body pointer; after the line (whose command still runs) the file is gone,
/// the shell signs THIS build's revision with the FILE'S key, its prompt hooks
/// are the ones it had, and — the point of it — a body pointer written later is
/// taken at its next prompt like any loader shell's. A second line naming a
/// file that is gone changes nothing, prints nothing, and the command after it
/// runs.
///
/// And THE OLD `~/.aterm/bin` goes: the pre-loader script put that folder (the
/// caller lays it, [`lay_aterm_bin`]) FIRST on PATH, and the user has added a
/// copy of their own at the end; the upgrade takes off exactly the first and
/// moves nothing else (before it, both are there: the negative control), and the
/// newer body taken later takes off nothing more.
#[cfg(unix)]
fn live_typed_upgrade(label: &str, shell: ShellType, sh: &mut LiveShell, bf: &BodyFixture) {
    let aterm_bin = bf.fx.home.join(".aterm").join("bin");
    let aterm_bin = aterm_bin.to_str().expect("UTF-8 home");
    sh.send(if label == "fish" {
        "set -gx PATH $PATH $HOME/.aterm/bin"
    } else {
        "PATH=\"$PATH:$HOME/.aterm/bin\""
    });
    let (line, marker) = body_probe(label, 1);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, 0);
    assert_eq!(
        sh.value_before("REV=", at),
        "",
        "{label}: no revision before"
    );
    let hooks = sh.value_before("HOOKS=", at);
    let path = sh.value_before("PATHV=", at);
    assert!(
        path.starts_with(&format!("{aterm_bin}:"))
            && path.split(':').filter(|entry| *entry == aterm_bin).count() == 2,
        "{label}: the old script put ~/.aterm/bin first, the user a copy last: {path:?}"
    );
    assert!(
        !stdout_between(sh, 0, at).contains(INTEGRATION_REV_KEY),
        "{label}: a pre-loader shell signs no revision (the negative control)"
    );

    let file = bf.fx.home.join("re key's");
    std::fs::write(
        &file,
        format!(
            "{REKEY_NEW}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey_with_loader(shell, &posix_quoted(&file)).expect("scripted");
    let sent = at;
    // Under the user's `set -x` the trace names the path, never the key. (fish traces
    // through `fish_trace`; not asserted.)
    let (trace, untrace) = if matches!(shell, ShellType::Zsh | ShellType::Bash) {
        ("set -x; ", "set +x; ")
    } else {
        ("", "")
    };
    sh.send(&format!("{trace}{typed} {untrace}{}", ran(label, 201)));
    let at = sh.wait_for_after("RAN201", at);
    if !trace.is_empty() {
        // The trace's own last line, `set +x`, ends it on stderr.
        let pipe = std::sync::Arc::clone(&sh.stderr);
        sh.wait_on(&pipe, "the trace's `set +x`", |err| {
            find_bytes(err, b"set +x").map(|_| ())
        });
        // bash writes its prompt, and the signed 133;B in it, on stderr too.
        assert!(
            !cut_osc(&sh.stderr_text()).contains(REKEY_NEW),
            "{label}: the key in the trace\n{}",
            sh.stderr_text()
        );
    }
    // Signed at the upgrade, before the command the line carries runs: that command is the
    // relaunched agent, which can hold the terminal for hours, and until the next prompt the
    // window would read `integration_rev=frozen`. And signed with the file's key: the key
    // the shell held is the one a degraded tab's engine lost, so a mark signed with it is
    // dropped.
    let signed = format!(
        "633;P;{INTEGRATION_REV_KEY}={};id={REKEY_NEW}",
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
    let upgraded = without_first(&path, aterm_bin);
    assert_eq!(
        sh.value_before("PATHV=", at),
        upgraded,
        "{label}: the old script's ~/.aterm/bin off, the user's copy and the rest where they were"
    );
    // The hooks it had, and in fish the loader's prompt-event check and the
    // body's prompt give-back (defined after the command) beside them.
    assert_eq!(
        sh.value_before("HOOKS=", at)
            .replace(",fish_prompt __aterm_fish_prompt_event", "")
            .replace(",fish_preexec __aterm_prompt_give_back", ""),
        hooks,
        "{label}: the same hooks"
    );
    let (line, marker) = body_probe(label, 3);
    sh.send(&line);
    let end = sh.wait_for_after(&marker, at);
    let after = stdout_between(sh, at, end);
    for mark in [
        format!(
            "633;P;{INTEGRATION_REV_KEY}={};id={REKEY_NEW}",
            bf.ours_rev()
        ),
        format!("133;A;id={REKEY_NEW}"),
        format!("133;C;id={REKEY_NEW}"),
    ] {
        assert!(after.contains(&mark), "{label}: {mark} after:\n{after:?}");
    }

    // The pointer it was handed works like any loader shell's.
    deliver(&bf.pointer, NEWER_REV);
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
    assert_eq!(
        sh.value_before("PATHV=", at),
        upgraded,
        "{label}: once per shell: the newer body keeps the user's copy"
    );

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
    lay_aterm_bin(&bf.fx);
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
    lay_aterm_bin(&bf.fx);
    std::fs::write(bf.fx.home.join(".bashrc"), "set -u\n").unwrap();
    let mut sh = spawn_live_bash_from(&bf.fx, &old, &[("ATERM_SHELL_NONCE", REKEY_OLD)]);
    live_typed_upgrade("bash", ShellType::Bash, &mut sh, &bf);
    sh.finish();
}

/// The key-only text an older sweep types reads the first line of the three-line file and
/// nothing else (the window writes a healthy shell's current key there, so that text never
/// takes an empty line for a key).
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

/// A nested shell is never integrated by the typed line, at its first line or any later
/// one. A shell started from an integrated tab (a `bash` typed there, a `nix develop`)
/// inherits the exported guard, so its own load stopped at the guard; when an agent it
/// launched is relaunched, the harness types the upgrade line into it with the file issued
/// for the tab's own shell. Upgrading it would wire hooks, re-front its PATH and sign
/// `integration_rev=current` for a tab whose own shell is still frozen. The loader's proof
/// that a shell is its own is a hook function its integration defined, which no child
/// shell inherits and no typed line defines.
#[cfg(unix)]
fn nested_shell_is_left_alone(label: &str, shell: ShellType, sh: &mut LiveShell, bf: &BodyFixture) {
    let (line, marker) = body_probe(label, 1);
    sh.send(&line);
    let mut at = sh.wait_for_after(&marker, 0);
    let (hooks, path) = (sh.value_before("HOOKS=", at), sh.value_before("PATHV=", at));
    assert_eq!(sh.value_before("REV=", at), "", "{label}: nested, no body");

    for round in 0..2 {
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
        sh.send(&format!("{typed} {}", ran(label, 201 + round)));
        at = sh.wait_for_after(&format!("RAN{}", 201 + round), at);
        let (line, marker) = body_probe(label, 2 + round);
        sh.send(&line);
        at = sh.wait_for_after(&marker, at);
        assert!(
            !file.exists(),
            "{label}: the one-use file is still consumed (line {round})"
        );
        assert_eq!(
            sh.value_before("REV=", at),
            "",
            "{label}: no body sourced into a nested shell (line {round})\n{}",
            sh.transcript()
        );
        assert_eq!(
            sh.value_before("HOOKS=", at),
            hooks,
            "{label}: no hook wired (line {round})"
        );
        assert_eq!(
            sh.value_before("PATHV=", at),
            path,
            "{label}: PATH as the nested environment set it (line {round})"
        );
        assert!(
            !stdout_between(sh, 0, at).contains(INTEGRATION_REV_KEY),
            "{label}: no revision signed (line {round})\n{}",
            sh.transcript()
        );
    }
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
        .env("ATERM_SHELL_INTEGRATION_INSTALLED", "1");
    let mut sh = LiveShell::spawn("bash", &mut cmd);
    nested_shell_is_left_alone("bash", ShellType::Bash, &mut sh, &bf);
    sh.finish();
}

/// THE OLD `~/.aterm/bin` IN POWERSHELL. Its integration has no loader and no
/// body: the guard ends a second load, and nothing reaches a running pwsh but
/// the file dot-sourced into it again (a profile re-run after an update), which
/// takes the entry off before the guard where the pwsh proves an older script
/// put it there. `run(snippet, nested)` runs `snippet` in one pwsh, with
/// `$env:ATERM_TEST_OLD` naming the pre-loader script and
/// `$env:ATERM_TEST_NEW` this one, and returns its stdout; `nested` = the guard
/// and a PATH led by a copy of the folder inherited. The folder is made inside
/// the pwsh. Captured metadata places it before or after that process without aging waits.
/// Four pwsh:
///
/// - THIS build's first load, the folder there: PATH as it was; a copy the
///   user then puts first stays through the file sourced again (no older
///   script ran here: the negative control of the proof).
/// - The pre-loader script, the folder there before: it put the folder first;
///   with a copy of the user's own at the end, the file sourced again takes
///   exactly the first off, and a third load nothing more.
/// - The pre-loader script with no folder at its load (it put nothing there),
///   the folder made after the process started and the user's copy at the end:
///   the file sourced again keeps it.
/// - A NESTED pwsh — the guard and PATH inherited, no variable of an older
///   script's — keeps PATH as it found it.
///
/// The five processes share nothing (each its own home), so they start side by
/// side and are read in this order: one after another they were five pwsh
/// startups end to end, the longest wait in the crate's binary.
#[cfg(any(unix, windows))]
fn pwsh_takes_an_older_scripts_aterm_bin_off(
    label: &str,
    run: impl Fn(&str, bool) -> String + Sync,
) {
    let sep = if cfg!(windows) { ";" } else { ":" };
    // Captured metadata in the existing owned child, not filesystem birthtime
    // setters: the actual loader still reads its normal Get-Item boundary.
    let dates = "$__t_start = [System.Diagnostics.Process]::GetCurrentProcess().StartTime.ToUniversalTime(); \
                 function Get-Item { param([string]$LiteralPath, [switch]$Force, [string]$ErrorAction) \
                 if ($LiteralPath -eq $__t_bin) { \
                 [PSCustomObject]@{ CreationTimeUtc = $__t_born; LastWriteTimeUtc = $__t_mtime } \
                 } else { Microsoft.PowerShell.Management\\Get-Item -LiteralPath $LiteralPath -Force:$Force -ErrorAction $ErrorAction } }; ";
    // The `B=` and `P<n>=` lines a run printed, in order.
    let lines = |stdout: &str| -> (String, Vec<String>) {
        let mut bin = String::new();
        let mut paths = Vec::new();
        for line in stdout.lines() {
            let Some((key, value)) = line.trim_end_matches('\r').split_once('=') else {
                continue;
            };
            if key == "B" {
                bin = value.to_owned();
            } else if key
                .strip_prefix('P')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            {
                paths.push(value.to_owned());
            }
        }
        (bin, paths)
    };
    let older = "$__t_bin = Join-Path $HOME '.aterm/bin'; \
                 $__t_dir = New-Item -ItemType Directory -Force -Path $__t_bin; \
                 $__t_born = $__t_start.AddHours(-1); $__t_mtime = $__t_start.AddHours(-2); 'B=' + $__t_bin; ";
    let now = "$__t_bin = Join-Path $HOME '.aterm/bin'; \
               $__t_dir = New-Item -ItemType Directory -Force -Path $__t_bin; \
               $__t_born = $__t_start.AddHours(1); $__t_mtime = $__t_start.AddHours(-2); 'B=' + $__t_bin; ";
    let user_first = "$env:PATH = $__t_bin + [IO.Path]::PathSeparator + $env:PATH; ";
    let user_last = "$env:PATH = $env:PATH + [IO.Path]::PathSeparator + $__t_bin; ";
    let copies = |path: &str, bin: &str| path.split(sep).filter(|entry| *entry == bin).count();

    let fallbacks = ["", "$__t_born = [DateTime]::MinValue; "];
    let mut runs = vec![(
        format!(
            "{older}. $env:ATERM_TEST_NEW; 'P0=' + $env:PATH; {user_first}'P1=' + $env:PATH; \
             . $env:ATERM_TEST_NEW; 'P2=' + $env:PATH"
        ),
        false,
    )];
    runs.extend(fallbacks.map(|fallback| {
        (
            format!(
                "{older}{fallback}. $env:ATERM_TEST_OLD; {user_last}'P0=' + $env:PATH; \
                 . $env:ATERM_TEST_NEW; 'P1=' + $env:PATH; . $env:ATERM_TEST_NEW; 'P2=' + $env:PATH"
            ),
            false,
        )
    }));
    runs.push((
        format!(
            ". $env:ATERM_TEST_OLD; {now}{user_last}'P0=' + $env:PATH; \
             . $env:ATERM_TEST_NEW; 'P1=' + $env:PATH"
        ),
        false,
    ));
    runs.push((
        "'P0=' + $env:PATH; . $env:ATERM_TEST_NEW; 'P1=' + $env:PATH".to_owned(),
        true,
    ));
    let run = &run;
    let mut outs = std::thread::scope(|scope| {
        let started: Vec<_> = runs
            .iter()
            .map(|(snippet, nested)| {
                scope.spawn(move || run(&format!("{dates}{snippet}"), *nested))
            })
            .collect();
        started
            .into_iter()
            .map(|one| {
                one.join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect::<Vec<_>>()
            .into_iter()
    });
    let mut next = || lines(&outs.next().expect("one output per run"));

    let (bin, p) = next();
    assert_eq!(p.len(), 3, "{label}: {p:?}");
    assert_eq!(
        copies(&p[0], &bin),
        0,
        "{label}: this build's first load: {p:?}"
    );
    assert_eq!(p[1], format!("{bin}{sep}{}", p[0]), "{label}");
    assert_eq!(p[2], p[1], "{label}: the user's own ~/.aterm/bin stays");

    for _fallback in fallbacks {
        let (bin, p) = next();
        assert_eq!(p.len(), 3, "{label}: {p:?}");
        assert!(
            p[0].starts_with(&format!("{bin}{sep}")) && copies(&p[0], &bin) == 2,
            "{label}: the old script put ~/.aterm/bin first, the user a copy last: {p:?}"
        );
        assert_eq!(
            p[1],
            without_first(&p[0], &bin),
            "{label}: sourced again: the old script's entry off, nothing else moved"
        );
        assert_eq!(p[2], p[1], "{label}: once");
    }

    let (bin, p) = next();
    assert_eq!(p.len(), 2, "{label}: {p:?}");
    assert_eq!(
        copies(&p[0], &bin),
        1,
        "{label}: no folder at the old script's load, so only the user's copy: {p:?}"
    );
    assert_eq!(
        p[1], p[0],
        "{label}: a folder made after the process started was never the old script's"
    );

    let (_, p) = next();
    assert_eq!(p.len(), 2, "{label}: {p:?}");
    assert!(p[0].contains(".aterm"), "{label}: inherited: {p:?}");
    assert_eq!(
        p[1], p[0],
        "{label}: a nested pwsh keeps PATH as it found it"
    );
}

#[cfg(unix)]
#[test]
fn a_pwsh_takes_an_older_scripts_aterm_bin_off_when_sourced_again() {
    let pwsh = need_pwsh();
    let new = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    pwsh_takes_an_older_scripts_aterm_bin_off("pwsh", |snippet, nested| {
        let mut cmd = shell_command(pwsh);
        let old = cmd.home().join("old.ps1");
        std::fs::write(&old, PRELOADER_PWSH).expect("write the pre-loader script");
        let inherited = format!("{}/.aterm/bin:/usr/bin:/bin", cmd.home().display());
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", snippet])
            .env("ATERM_TEST_OLD", &old)
            .env("ATERM_TEST_NEW", &new)
            .env("PATH", "/usr/bin:/bin")
            .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
            .env("POWERSHELL_UPDATECHECK", "Off");
        if nested {
            cmd.env("ATERM_SHELL_INTEGRATION_INSTALLED", "1")
                .env("PATH", inherited);
        }
        let out = cmd
            .output()
            .unwrap_or_else(|error| panic!("spawn {pwsh}: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "pwsh: {snippet}\nstdout={stdout:?} stderr={:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    });
}

/// [`pwsh_takes_an_older_scripts_aterm_bin_off`] where the script runs most:
/// Windows PowerShell 5.1 and pwsh 7, `;` between PATH entries, each in a
/// private home.
#[cfg(windows)]
#[test]
fn a_powershell_takes_an_older_scripts_aterm_bin_off_when_sourced_again() {
    on_each_powershell("an older script's ~/.aterm/bin", |exe| {
        pwsh_takes_an_older_scripts_aterm_bin_off(&format!("{exe:?}"), |snippet, nested| {
            let world = PowerShellWorld::new();
            let old = world.home().join("old.ps1");
            std::fs::write(&old, PRELOADER_PWSH).expect("write the pre-loader script");
            let new = format!(
                "{}/src/scripts/aterm_shell_integration.ps1",
                env!("CARGO_MANIFEST_DIR")
            );
            let old = old.to_str().expect("UTF-8 fixture path").to_owned();
            let inherited = format!(
                "{};{}",
                world.home().join(".aterm").join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let mut env = vec![
                ("ATERM_TEST_OLD", old.as_str()),
                ("ATERM_TEST_NEW", new.as_str()),
            ];
            if nested {
                env.extend([
                    ("ATERM_SHELL_INTEGRATION_INSTALLED", "1"),
                    ("PATH", inherited.as_str()),
                ]);
            }
            let out = powershell_output_in(&world, exe, "", snippet, None, Pwsh::Bare, &env);
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            assert!(
                out.status.success(),
                "{exe:?}: {snippet}\nstdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            );
            stdout
        });
    });
}

/// The line `label`'s user types to put `~/.aterm/bin` on PATH: `first`, ahead
/// of everything, or last.
#[cfg(unix)]
fn users_aterm_bin(label: &str, first: bool) -> &'static str {
    match (label == "fish", first) {
        (true, true) => "set -gx PATH $HOME/.aterm/bin $PATH",
        (true, false) => "set -gx PATH $PATH $HOME/.aterm/bin",
        (false, true) => "PATH=\"$HOME/.aterm/bin:$PATH\"",
        (false, false) => "PATH=\"$PATH:$HOME/.aterm/bin\"",
    }
}

/// Probe `sh` ([`body_probe`] number `n`, after offset `at`): the offset past
/// it, and the revision and PATH it printed.
#[cfg(unix)]
fn probe_rev_path(label: &str, sh: &mut LiveShell, n: u32, at: usize) -> (usize, String, String) {
    let (line, marker) = body_probe(label, n);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, at);
    (
        at,
        sh.value_before("REV=", at),
        sh.value_before("PATHV=", at),
    )
}

/// Run `line` to its end, point `sh` at `rev`, and probe the body it took (a
/// line sent after the pointer is written, so the prompt before the probe runs
/// the body check): the offset past the probe and the PATH it printed.
#[cfg(unix)]
fn line_then_body(
    label: &str,
    sh: &mut LiveShell,
    bf: &BodyFixture,
    (line, rev, n): (&str, &str, u32),
    at: usize,
) -> (usize, String) {
    sh.send(&format!("{line}; {}", ran(label, 200 + n)));
    let at = sh.wait_for_after(&format!("RAN{}", 200 + n), at);
    deliver(&bf.pointer, rev);
    sh.send("true");
    let (at, taken, path) = probe_rev_path(label, sh, n, at);
    assert_eq!(taken, rev, "{label}: body {n} taken");
    (at, path)
}

/// AN OLDER LOADER'S `~/.aterm/bin`, in a live `label` shell, through both ways
/// this build reaches a running loader shell, from the real loaders that put
/// it there (57daa3707e) and from the same minus that block (613305828c, the
/// first build that put none there and set no mark, as every build since up
/// to this one). `spawn(bf, folder, spawned_by)` starts the shell on
/// `folder`'s scripts with the session's body pointer and `spawned_by` in its
/// environment. The shells, each in its own fixture, side by side, each started
/// since the cut the scripts take a shell's start against unless it says
/// otherwise:
///
/// 1. The prepending loader, the folder there before the shell started: it put
///    the folder first, and the user adds a copy at the end. This build's
///    body, taken at a prompt, takes exactly the first off, and a newer body
///    after it nothing more.
/// 2. The same loader with no folder at its load (it put nothing there), the
///    folder made after the shell started and the user's copy first: the body
///    keeps it — in a shell started before the cut too, whose start proves
///    the loader prepended but not that the folder was there.
/// 3. 613305828c's loader, the folder there before, the shell spawned by a
///    terminal naming itself aterm 0.105.0 (a dev build of the hour after
///    613305828c): a copy the user puts first stays through the body, since
///    the loader is there and answers (the review's v0.106.0 shell).
/// 4. The prepending loader, then this build's LOADER sourced over it in place
///    (a re-source; the typed upgrade's way): it notes the older loader before
///    it writes over `__aterm_si_dir`, and its body takes exactly the first
///    off.
/// 5. The prepending loader whose folder is then collected, and 6. the
///    prepending loader with 613305828c's sourced over it in place: neither
///    shell names a loader that holds the prepend any more
///    ([`UNNAMED_LOADER_CASES`]). Started before the cut, the start alone
///    proves an older script loaded it; started since, the version aterm
///    named to the shell does when it is v0.105.0 or older. Then the body
///    takes exactly the first off; otherwise it keeps the entry (the limit
///    `aterm help update` states).
#[cfg(unix)]
fn live_older_loaders_aterm_bin(
    label: &str,
    spawn: impl Fn(&BodyFixture, &Path, &[(&str, &str)]) -> LiveShell + Sync,
) {
    let ours_rev = |bf: &BodyFixture| bf.ours_rev();
    let spawn = &spawn;
    let source = if label == "fish" { "source" } else { "." };

    let born = |birth: u64| {
        let bf = BodyFixture::new();
        let bin = lay_aterm_bin_with_dates(&bf.fx, birth, CUT - 100, STARTED_SINCE);
        let mut sh = spawn(&bf, &older_loader_folder(&bf.fx, true), &[]);
        let (at, rev, before) = probe_rev_path(label, &mut sh, 1, 0);
        assert_eq!(rev, PREPENDING_REV, "{label}: the older loader runs");
        assert!(
            before.starts_with(&format!("{bin}:")),
            "{label}: the older loader put ~/.aterm/bin first: {before:?}"
        );
        let with_users = format!("{before}:{bin}");
        let user = users_aterm_bin(label, false);
        let (at, path) = line_then_body(label, &mut sh, &bf, (user, &ours_rev(&bf), 2), at);
        assert_eq!(
            path,
            without_first(&with_users, &bin),
            "{label}: the older loader's ~/.aterm/bin off, the user's copy and the rest where they were"
        );
        let (_, again) = line_then_body(label, &mut sh, &bf, ("true", NEWER_REV, 3), at);
        assert_eq!(again, path, "{label}: once per shell");
        sh.finish();
    };

    let made_after = |age: &str| {
        let bf = BodyFixture::new();
        let mut sh = spawn(&bf, &older_loader_folder(&bf.fx, true), &[]);
        let (at, _, before) = probe_rev_path(label, &mut sh, 1, 0);
        assert!(
            !before.contains(".aterm/bin"),
            "{label}: no folder, no entry: {before:?}"
        );
        // The directory is created after the old loader ran. Its positive birth
        // is newer than the shell, although its preserved mtime is much older.
        let bin = lay_aterm_bin_with_dates(&bf.fx, STAND_IN_NOW - 5, CUT - 100, age);
        let user = users_aterm_bin(label, true);
        let (_, path) = line_then_body(label, &mut sh, &bf, (user, &ours_rev(&bf), 2), at);
        assert_eq!(
            path,
            format!("{bin}:{before}"),
            "{label} ({age}): the user's own ~/.aterm/bin stays where the older loader found no folder"
        );
        sh.finish();
    };

    let unprepending = || {
        let bf = BodyFixture::new();
        let bin = lay_aterm_bin(&bf.fx);
        // Spawned by a dev build of the hour after 613305828c that still read
        // 0.105.0: its loader is there, and it answers.
        let mut sh = spawn(&bf, &older_loader_folder(&bf.fx, false), SPAWNED_BY_0_105);
        let (at, rev, before) = probe_rev_path(label, &mut sh, 1, 0);
        assert_eq!(rev, UNPREPENDING_REV, "{label}: 613305828c's loader runs");
        assert!(
            !before.contains(".aterm/bin"),
            "{label}: it puts none there: {before:?}"
        );
        let user = users_aterm_bin(label, true);
        let (_, path) = line_then_body(label, &mut sh, &bf, (user, &ours_rev(&bf), 2), at);
        assert_eq!(
            path,
            format!("{bin}:{before}"),
            "{label}: the user's own ~/.aterm/bin stays in a shell no older script prepended, \
             its loader there to say so whatever version spawned it"
        );
        sh.finish();
    };

    let in_place = || {
        let bf = BodyFixture::new();
        let bin = lay_aterm_bin(&bf.fx);
        let mut sh = spawn(&bf, &older_loader_folder(&bf.fx, true), &[]);
        let (at, _, before) = probe_rev_path(label, &mut sh, 1, 0);
        assert!(
            before.starts_with(&format!("{bin}:")),
            "{label}: {before:?}"
        );
        let with_users = format!("{before}:{bin}");
        let loader = bf.ours.join(format!("aterm_shell_integration.{label}"));
        sh.send(&format!(
            "{}; {source} {}; {}",
            users_aterm_bin(label, false),
            posix_quoted(&loader),
            ran(label, 202)
        ));
        let at = sh.wait_for_after("RAN202", at);
        let (_, rev, path) = probe_rev_path(label, &mut sh, 2, at);
        assert_eq!(
            rev,
            ours_rev(&bf),
            "{label}: this build's loader came in place"
        );
        assert_eq!(
            path,
            without_first(&with_users, &bin),
            "{label}: in place: the older loader's ~/.aterm/bin off, the user's copy kept"
        );
        sh.finish();
    };

    let unnamed = |(age, spawned_by, collected, off): (&str, SpawnedBy, bool, bool)| {
        let bf = BodyFixture::new();
        let bin = lay_aterm_bin_with_dates(
            &bf.fx,
            STAND_IN_NOW - 6 * 86_400,
            STAND_IN_NOW - 6 * 86_400,
            age,
        );
        let older = older_loader_folder(&bf.fx, true);
        let mut sh = spawn(&bf, &older, spawned_by);
        let (at, _, before) = probe_rev_path(label, &mut sh, 1, 0);
        assert!(
            before.starts_with(&format!("{bin}:")),
            "{label}: the older loader put ~/.aterm/bin first: {before:?}"
        );
        let with_users = format!("{before}:{bin}");
        let user = users_aterm_bin(label, false);
        let line = if collected {
            std::fs::remove_dir_all(&older).expect("collect the older loader's folder");
            user.to_owned()
        } else {
            let between =
                older_loader_folder(&bf.fx, false).join(format!("aterm_shell_integration.{label}"));
            format!("{user}; {source} {}", posix_quoted(&between))
        };
        let (_, path) = line_then_body(label, &mut sh, &bf, (&line, &ours_rev(&bf), 2), at);
        let how = format!(
            "{}, started {}, spawned by {spawned_by:?}",
            if collected {
                "its loader's folder collected"
            } else {
                "613305828c's loader sourced in place"
            },
            if age == STARTED_BEFORE {
                "before the cut"
            } else {
                "since"
            },
        );
        if off {
            assert_eq!(
                path,
                without_first(&with_users, &bin),
                "{label} ({how}): the older loader's ~/.aterm/bin off, the user's copy and \
                 the rest where they were"
            );
        } else {
            assert_eq!(
                path, with_users,
                "{label} ({how}): nothing proves whose the entry is, so it stays"
            );
        }
        sh.finish();
    };

    let mut cases: Vec<(String, Box<dyn FnOnce() + Send + '_>)> = vec![
        (format!("{label}-born-before"), Box::new(|| born(CUT - 100))),
        (format!("{label}-no-birth"), Box::new(|| born(0))),
        (
            format!("{label}-made-after-since"),
            Box::new(|| made_after(STARTED_SINCE)),
        ),
        (
            format!("{label}-made-after-before"),
            Box::new(|| made_after(STARTED_BEFORE)),
        ),
        (format!("{label}-unprepending"), Box::new(unprepending)),
        (format!("{label}-in-place"), Box::new(in_place)),
    ];
    let unnamed = &unnamed;
    for (n, case) in UNNAMED_LOADER_CASES.into_iter().enumerate() {
        cases.push((
            format!("{label}-unnamed-{n}"),
            Box::new(move || unnamed(case)),
        ));
    }
    side_by_side(cases);
}

/// Run `cases`, each on a thread named for it, and wait for every one: shells
/// that share nothing start side by side instead of end to end. The first
/// case to panic (each prints its own message as it fails, under its thread's
/// name) is raised again here once all have ended.
#[cfg(unix)]
fn side_by_side<'a>(cases: Vec<(String, Box<dyn FnOnce() + Send + 'a>)>) {
    std::thread::scope(|scope| {
        let running: Vec<_> = cases
            .into_iter()
            .map(|(name, case)| {
                std::thread::Builder::new()
                    .name(name)
                    .spawn_scoped(scope, case)
                    .expect("start a case's thread")
            })
            .collect();
        let mut first = None;
        for case in running {
            if let Err(panic) = case.join() {
                first.get_or_insert(panic);
            }
        }
        if let Some(panic) = first {
            std::panic::resume_unwind(panic);
        }
    });
}

/// The environment a terminal names itself in to the shells it spawns.
#[cfg(unix)]
type SpawnedBy = &'static [(&'static str, &'static str)];

/// Spawned by aterm v0.105.0, the last release whose every script prepended
/// ...
#[cfg(unix)]
const SPAWNED_BY_0_105: SpawnedBy = &[
    ("TERM_PROGRAM", "aterm"),
    ("TERM_PROGRAM_VERSION", "0.105.0"),
];

/// ... v0.106.0, the first whose script did not ...
#[cfg(unix)]
const SPAWNED_BY_0_106: SpawnedBy = &[
    ("TERM_PROGRAM", "aterm"),
    ("TERM_PROGRAM_VERSION", "0.106.0"),
];

/// ... another terminal at a version that would count were it aterm's ...
#[cfg(unix)]
const SPAWNED_BY_ANOTHER: SpawnedBy = &[
    ("TERM_PROGRAM", "vscode"),
    ("TERM_PROGRAM_VERSION", "0.105.0"),
];

/// The shells whose loader is gone or unnamed ([`live_older_loaders_aterm_bin`]
/// 5 and 6): `(age, what spawned it, collected, the entry goes)`. Before the
/// cut the start proves the prepend whatever named it; since, only aterm at
/// v0.105.0 or older does.
#[cfg(unix)]
const UNNAMED_LOADER_CASES: [(&str, SpawnedBy, bool, bool); 8] = [
    (STARTED_BEFORE, &[], true, true),
    (STARTED_BEFORE, &[], false, true),
    (STARTED_SINCE, &[], true, false),
    (STARTED_SINCE, &[], false, false),
    (STARTED_SINCE, SPAWNED_BY_0_105, true, true),
    (STARTED_SINCE, SPAWNED_BY_0_105, false, true),
    (STARTED_SINCE, SPAWNED_BY_0_106, true, false),
    (STARTED_SINCE, SPAWNED_BY_ANOTHER, false, false),
];

#[cfg(target_os = "macos")]
#[test]
fn a_live_zsh_takes_an_older_loaders_aterm_bin_off_and_only_that() {
    let zsh = zsh_shell().expect("macOS ships /bin/zsh");
    live_older_loaders_aterm_bin("zsh", |bf, dir, spawned_by| {
        let pointer = bf.pointer.to_str().unwrap().to_string();
        let mut env = vec![
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            ("ATERM_CHILD", "1"),
            (BODY_POINTER_VAR, pointer.as_str()),
        ];
        env.extend_from_slice(spawned_by);
        spawn_live_zsh_from(zsh, &bf.fx, dir, &env)
    });
}

#[cfg(unix)]
#[test]
fn a_live_bash_takes_an_older_loaders_aterm_bin_off_and_only_that() {
    live_older_loaders_aterm_bin("bash", |bf, dir, spawned_by| {
        let pointer = bf.pointer.to_str().unwrap().to_string();
        let mut env = vec![
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            (BODY_POINTER_VAR, pointer.as_str()),
        ];
        env.extend_from_slice(spawned_by);
        spawn_live_bash_from(&bf.fx, dir, &env)
    });
}

// ─── fish: the lanes above, in fish ───

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
    let mut cmd = pty_script(&[fish, "-i"]);
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
    let data = hermetic_data(hermetic.home());
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
    LiveShell::spawn("fish", &mut cmd).holding(hermetic.into_home())
}

/// The fish lanes need fish and a pty. Like every other live fish test in this
/// crate, a host without them runs none of them and says so: COULD NOT RUN.
#[cfg(unix)]
fn live_fish() -> &'static str {
    let fish = need_fish();
    need_pty_script();
    fish
}

#[cfg(unix)]
#[test]
fn a_live_fish_takes_a_newer_body_at_its_next_prompt() {
    let fish = live_fish();
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
    let fish = live_fish();
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    lay_aterm_bin(&bf.fx);
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
    let fish = live_fish();
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

#[cfg(unix)]
#[test]
fn a_live_fish_takes_an_older_loaders_aterm_bin_off_and_only_that() {
    let fish = live_fish();
    live_older_loaders_aterm_bin("fish", |bf, dir, spawned_by| {
        let pointer = bf.pointer.to_str().unwrap().to_string();
        let mut env = vec![
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            (BODY_POINTER_VAR, pointer.as_str()),
        ];
        env.extend_from_slice(spawned_by);
        spawn_live_fish_from(fish, &bf.fx, Some(dir), &env)
    });
}

// ─── A body re-sourced keeps the user's key bindings ───

/// The line that binds Ctrl+Right to a widget of the user's choosing, and the probe that
/// prints what it is bound to now, in `label`'s own syntax.
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

/// A newer body taken at a prompt leaves the user's key bindings alone. The integration
/// binds a handful of keys (Alt/Ctrl+arrows, Home/End, Delete, Shift+Up/Down) so their
/// sequences never leak as text, once, at the shell's first load, which zsh (`.zshenv`) and
/// fish (vendor conf.d) run before the user's own rc, so a binding of the user's wins. The
/// binding is the loader's, on a fresh load only: a re-sourced body would take those keys
/// back from the user in every live tab.
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
    deliver(&bf.pointer, NEWER_REV);
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
    let fish = live_fish();
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

// ─── An upgrade in place leaves the zle-line-init chain alone ───

/// A live zsh with ZLE (on a pty, from the pre-loader script in `old`), whose `.zshrc` hooks
/// `zle-line-init` as modern plugins do (`add-zle-hook-widget`, which keeps aterm's widget
/// first among its hooks), takes the typed upgrade, and each next prompt emits one 133;B.
///
/// The upgrade must not re-wire `zle-line-init`: aliasing the plugin's dispatcher (which
/// calls aterm's widget) to `__aterm_orig_zle_line_init` and installing aterm's own widget
/// that calls the alias is a loop (recursion-limit errors and hundreds of marks per
/// prompt). A widget names its function, so the existing chain reaches the new body's
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
    let mut cmd = pty_script(&[zsh, "-i"]);
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

/// A live fish from before loaders whose config.fish defines its own prompt keeps it
/// through the typed upgrade. fish reads the vendor conf.d (aterm's wrapper) before
/// config.fish, so a prompt the user defines there replaces aterm's wrapper for the
/// shell's life; the upgrade replaces only a prompt that is aterm's own. This prompt keeps
/// a copy of the one it replaced, as conda's hook and a venv's activate.fish do (here the
/// pre-loader wrapper, which writes its own marks): the prompt drawn after the upgrade
/// carries one signed 133;A and one signed 133;B, and the copy reads the status the user's
/// wrapper put back before calling it (`$status` and a `$pipestatus` of its own), not the
/// command's.
#[cfg(unix)]
#[test]
fn a_live_fish_keeps_the_users_own_prompt_through_the_typed_upgrade() {
    let fish = live_fish();
    let bf = BodyFixture::new();
    let old = preloader_folder(&bf.fx);
    let config = bf.fx.home.join("config").join("fish");
    std::fs::create_dir_all(config.join("functions")).expect("config dir");
    std::fs::write(
        config.join("functions").join("fish_prompt.fish"),
        "function fish_prompt\n    printf 'IN[%s|%s]> ' $status \"$pipestatus\"\nend\n",
    )
    .expect("fish_prompt.fish");
    std::fs::write(
        config.join("config.fish"),
        "functions -c fish_prompt __my_prev_prompt\n\
         function __my_back\n    return $argv[1]\nend\n\
         function fish_prompt\n    set -l st $status\n    echo -n 'MYPROMPT> '\n\
         __my_back 0 | __my_back $st\n    __my_prev_prompt\nend\n",
    )
    .expect("config.fish");
    let mut sh = spawn_live_fish_from(
        fish,
        &bf.fx,
        Some(&old),
        &[("ATERM_SHELL_NONCE", REKEY_OLD)],
    );
    sh.send("echo MINE(math 500 + 1)");
    let at = sh.wait_for_after("MINE501", 0);
    assert!(
        stdout_between(&sh, 0, at).contains("MYPROMPT> "),
        "before: the user's own prompt\n{}",
        sh.transcript()
    );
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
    sh.send("false");
    sh.send("echo DONE(math 600 + 1)");
    let end = sh.wait_for_after("DONE601", at);
    // fish under TERM=dumb draws the prompt again, whole, as typed-ahead text
    // arrives: every draw is the user's, between one signed 133;A and one 133;B,
    // and the last, after `false`, reads what the wrapper put back.
    let drawn = signed_prompts(&stdout_between(&sh, at, end), REKEY_OLD);
    assert!(
        drawn.as_ref().is_some_and(|p| {
            !p.is_empty()
                && p.iter().all(|s| s.contains("MYPROMPT> "))
                && p.last().is_some_and(|s| s.contains("IN[1|0 1]> "))
        }),
        "the user's own prompt is the one drawn after the upgrade, between one signed \
         133;A and one signed 133;B, reading the status its wrapper put back: {drawn:?}\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// A live fish whose config.fish defines its own prompt, piped to `source` as
/// `starship init fish | source` does, takes a newer body at its next prompt and signs the
/// revision it then runs. Such a prompt replaces the loader's trampoline, and with it the
/// trampoline's body check, so the check rides the `fish_prompt` event, which fish emits
/// before every prompt whoever draws it; the prompt drawn is still the user's, reading the
/// previous command's status.
#[cfg(unix)]
#[test]
fn a_live_fish_with_its_own_prompt_takes_a_newer_body() {
    let fish = live_fish();
    let bf = BodyFixture::new();
    let config = bf.fx.home.join("config").join("fish");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(
        config.join("config.fish"),
        "printf '%s\\n' 'function fish_prompt' 'printf \"MYPROMPT[%s]> \" $status' end | source\n",
    )
    .expect("config.fish");
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
    let (line, marker) = body_probe("fish", 1);
    sh.send(&line);
    let at = sh.wait_for_after(&marker, 0);
    assert_eq!(sh.value_before("REV=", at), bf.ours_rev(), "spawned rev");

    deliver(&bf.pointer, NEWER_REV);
    sh.send("false");
    let (line, marker) = body_probe("fish", 2);
    sh.send(&line);
    let from = at;
    let at = sh.wait_for_after(&marker, at);
    assert_eq!(
        sh.value_before("REV=", at),
        NEWER_REV,
        "taken at the next prompt\n{}",
        sh.transcript()
    );
    assert_eq!(sh.value_before("PROBE-", at), "NEWER", "the newer body");
    assert!(!bf.pointer.exists(), "the pointer is removed once read");
    let switch = stdout_between(&sh, from, at);
    let signed = format!("633;P;{INTEGRATION_REV_KEY}={NEWER_REV};id={REKEY_OLD}");
    assert!(
        switch.contains(&signed),
        "the revision it runs is signed:\n{switch:?}"
    );
    assert!(
        switch.contains("MYPROMPT[1]> "),
        "the user's prompt, with the status of `false`:\n{switch:?}"
    );
    sh.finish();
}

// ─── The user's prompt: handed what fish hands it, and kept wrapped ───

/// A live fish's prompt that the integration wraps (one in functions/, wrapped at load, as
/// fish's own default prompt is) reads the previous command's `$status` and `$pipestatus`
/// exactly as fish hands them to an unwrapped prompt, after `not` too (which inverts the
/// status and leaves `$pipestatus`). And draws a Ctrl-C cancels, four in a row with no
/// command between them (this prompt cancels itself, as a job killed by SIGINT cancels
/// fish's whole evaluation), leave the next prompt the user's, and marked.
#[cfg(unix)]
#[test]
fn a_live_fish_wrapped_prompt_reads_the_commands_status() {
    let fish = live_fish();
    let fx = LiveFixture::new();
    let ours = install_script_set(&fx.base).expect("install");
    let functions = fx.home.join("config").join("fish").join("functions");
    std::fs::create_dir_all(&functions).expect("functions dir");
    std::fs::write(
        functions.join("fish_prompt.fish"),
        "function fish_prompt\n    printf 'ST[%s|%s]> ' \"$status\" \"$pipestatus\"\n\
         if set -q MYCANCEL[1]\n        set -e MYCANCEL[1]\n        sh -c 'kill -INT $$'\n    end\nend\n",
    )
    .expect("fish_prompt.fish");
    let mut sh = spawn_live_fish_from(fish, &fx, Some(&ours), &[("ATERM_SHELL_NONCE", REKEY_OLD)]);
    let mut at = 0;
    for (n, (cmd, want)) in [
        ("false", "ST[1|1]> "),
        ("sh -c 'exit 7'", "ST[7|7]> "),
        ("false | true", "ST[0|1 0]> "),
        ("not true", "ST[1|0]> "),
        ("not false", "ST[0|1]> "),
    ]
    .into_iter()
    .enumerate()
    {
        let from = at;
        sh.send(cmd);
        sh.send(&format!("echo PS(math {} + 1)", 600 + n));
        at = sh.wait_for_after(&format!("PS{}", 601 + n), at);
        assert!(
            stdout_between(&sh, from, at).contains(want),
            "the wrapped prompt after `{cmd}` reads {want:?}\n{}",
            sh.transcript()
        );
    }
    // The prompt after `set` is cancelled, and each Enter draws it again: three
    // more cancelled, then the one the `echo` is typed at.
    sh.send("set -g MYCANCEL 1 2 3 4");
    for _ in 0..4 {
        sh.send("");
    }
    sh.send("echo PS(math 700 + 1)");
    let end = sh.wait_for_after("PS701", at);
    let text = stdout_between(&sh, at, end);
    let redrawn = text
        .rsplit(&format!("133;A;id={REKEY_OLD}"))
        .next()
        .unwrap_or("");
    assert!(
        redrawn
            .find("ST[")
            .zip(redrawn.find(&format!("133;B;id={REKEY_OLD}")))
            .is_some_and(|(p, b)| p < b),
        "the prompt drawn after a cancelled one is marked: {redrawn:?}\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// A live fish that draws aterm's own prompt (`ATERM_PROMPT_STYLE`) still gives the user's
/// prompt back for a command, so `funcsave fish_prompt` writes the user's prompt, not the
/// loader's trampoline (which a fish without aterm would answer with
/// `Unknown command: __aterm_body_check` at every prompt).
#[cfg(unix)]
#[test]
fn a_live_fish_with_a_prompt_style_gives_the_users_prompt_back() {
    let fish = live_fish();
    let fx = LiveFixture::new();
    let ours = install_script_set(&fx.base).expect("install");
    let functions = fx.home.join("config").join("fish").join("functions");
    std::fs::create_dir_all(&functions).expect("functions dir");
    let saved = functions.join("fish_prompt.fish");
    std::fs::write(&saved, "function fish_prompt\n    printf 'MINE> '\nend\n")
        .expect("fish_prompt.fish");
    let mut sh = spawn_live_fish_from(
        fish,
        &fx,
        Some(&ours),
        &[
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            ("ATERM_PROMPT_STYLE", "minimal"),
        ],
    );
    sh.send("false");
    sh.send(
        "functions fish_prompt | string match -q '*MINE*'; and echo SEEN=own; or echo SEEN=other; \
         funcsave fish_prompt; echo PY(math 950 + 1)",
    );
    let at = sh.wait_for_after("PY951", 0);
    assert!(
        !stdout_between(&sh, 0, at).contains("MINE> "),
        "the style's prompt is the one drawn\n{}",
        sh.transcript()
    );
    assert_eq!(
        sh.value_before("SEEN=", at),
        "own",
        "a command sees the user's own prompt\n{}",
        sh.transcript()
    );
    let kept = std::fs::read_to_string(&saved).unwrap_or_default();
    assert!(
        kept.contains("MINE> ") && !kept.contains("__aterm"),
        "funcsave writes the user's prompt back: {kept:?}\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// A live fish whose config.fish defines its own prompt keeps the prompt marks: every
/// prompt, the first too, is a signed 133;A, the user's prompt (reading the previous
/// command's status), a signed 133;B. fish reads the vendor conf.d before config.fish, so
/// such a prompt (`starship init fish | source`) replaces the trampoline for the shell's
/// life. Also pinned:
/// - a prompt that keeps a copy of the trampoline and calls it only sometimes never reaches
///   itself again through that copy, and the copy draws its marks once (the signed marks of
///   every prompt string alternate A, B), also where fish words its stack trace (which
///   tells the copy from the wrapper) in German;
/// - a prompt written through `string` (fish-async-prompt's, tide's) is drawn on one line
///   with its marks;
/// - a command sees the user's own `fish_prompt`, the one config.fish defined, which
///   `funced` edits in place;
/// - the prompt is handed what fish hands it: fish 4.1's transient prompt draws it once
///   more as a command starts, with `--final-rendering`.
#[cfg(unix)]
#[test]
fn a_live_fish_with_its_own_prompt_keeps_its_prompt_marks() {
    let fish = live_fish();
    let fx = LiveFixture::new();
    let ours = install_script_set(&fx.base).expect("install");
    let config = fx.home.join("config").join("fish");
    std::fs::create_dir_all(config.join("functions")).expect("config dir");
    std::fs::write(
        config.join("functions").join("fish_prompt.fish"),
        "function fish_prompt\n    printf 'BASE> '\nend\n",
    )
    .expect("fish_prompt.fish");
    std::fs::write(
        config.join("config.fish"),
        "set -g MYMINI 1\nfunctions -c fish_prompt __my_prev_prompt\nfunction fish_prompt\n\
         set -l st $status\nif contains -- --final-rendering $argv\nprintf 'FINAL> '\n\
         else if set -q MYMINI\nstring join '' -- 'MINI[' $st ']> '\n\
         else\n__my_prev_prompt\nend\nend\n",
    )
    .expect("config.fish");
    // The editor `funced` runs below: it renames the prompt in the file it is given.
    let editor = fx.home.join("edit-prompt");
    aterm_tempfile::stand_in(
        &editor,
        "#!/bin/sh\nsed 's/MINI\\[/MINJ[/' \"$1\" >\"$1.new\" && cat \"$1.new\" >\"$1\" && rm -f \"$1.new\"\n",
    )
    .expect("editor");
    let editor = editor.to_str().expect("utf-8").to_string();
    let mut sh = spawn_live_fish_from(
        fish,
        &fx,
        Some(&ours),
        &[
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            ("LC_ALL", "de_DE.UTF-8"),
            ("LANGUAGE", "de"),
            ("EDITOR", &editor),
        ],
    );
    let a = format!("133;A;id={REKEY_OLD}");
    let mut at = 0;
    for (n, (cmd, want)) in [
        ("true", "MINI[0]> "),
        ("false", "MINI[1]> "),
        ("set -e MYMINI", "BASE> "),
        ("false", "BASE> "),
        ("set -g MYMINI 1", "MINI[0]> "),
        ("sh -c 'exit 7'", "MINI[7]> "),
    ]
    .into_iter()
    .enumerate()
    {
        let from = at;
        sh.send(cmd);
        sh.send(&format!("echo PM(math {} + 1)", 700 + n));
        at = sh.wait_for_after(&format!("PM{}", 701 + n), at);
        let text = stdout_between(&sh, from, at);
        let drawn = signed_prompts(&text, REKEY_OLD);
        // The last prompt string is the one drawn after `cmd`; the first row's
        // first is the shell's first prompt, drawn before any command started.
        let first_drawn = text.find(&a).is_some_and(|i| {
            text.find(&format!("133;C;id={REKEY_OLD}"))
                .is_some_and(|c| i < c)
        });
        let ok = drawn.as_ref().is_some_and(|p| {
            p.last()
                .is_some_and(|last| last.contains(want) && !last.contains('\n'))
                && (n > 0 || first_drawn && p[0].contains("MINI[0]> "))
        });
        assert!(
            ok && !text.contains("call stack"),
            "prompt after `{cmd}`: a signed 133;A, {want:?}, a signed 133;B: {drawn:?}\n{}",
            sh.transcript()
        );
    }
    // A command runs with the user's own prompt: what `funcsave fish_prompt`
    // writes is theirs, never a trampoline every later shell would draw.
    sh.send(
        "functions fish_prompt | string match -q '*MINI*'; and echo SEEN=own; or echo SEEN=other; \
         echo FV=$FISH_VERSION; echo PM(math 800 + 1)",
    );
    let at = sh.wait_for_after("PM801", at);
    assert_eq!(
        sh.value_before("SEEN=", at),
        "own",
        "a command sees the user's own prompt\n{}",
        sh.transcript()
    );
    let version = sh.value_before("FV=", at);
    let v: Vec<u32> = version.split('.').map(|n| n.parse().unwrap_or(0)).collect();
    // ... the one config.fish defined, so `funced` edits config.fish in place, as in a
    // fish without aterm (fish 4's funced writes functions/fish_prompt.fish for a prompt
    // whose copy names another file). fish 3 drops the file from every copy: fish 4 only.
    let mut at = at;
    if v.first().copied() >= Some(4) {
        sh.send("funced -s fish_prompt; echo PM(math 850 + 1)");
        at = sh.wait_for_after("PM851", at);
        let edited = std::fs::read_to_string(config.join("config.fish")).unwrap_or_default();
        assert!(
            edited.contains("'MINJ['"),
            "funced edits the user's config.fish in place: {edited:?}\n{}",
            sh.transcript()
        );
    } else {
        eprintln!(
            "fish {version} copies a function without its file (3.x): the funced row did not run"
        );
    }
    // The transient prompt (fish 4.1): the prompt drawn last as `true` starts is
    // the user's `--final-rendering` one.
    sh.send("set -g fish_transient_prompt 1; echo PM(math 900 + 1)");
    let at = sh.wait_for_after("PM901", at);
    if (v.first().copied(), v.get(1).copied()) >= (Some(4), Some(1)) {
        sh.send("true");
        sh.send("echo PM(math 900 + 2)");
        let end = sh.wait_for_after("PM902", at);
        let text = stdout_between(&sh, at, end);
        let started = text
            .find(&format!("133;C;id={REKEY_OLD}"))
            .unwrap_or(text.len());
        let drawn = signed_prompts(&text[..started], REKEY_OLD);
        assert!(
            drawn
                .as_ref()
                .and_then(|p| p.last())
                .is_some_and(|s| s.contains("FINAL> ")),
            "the transient prompt is the user's: {drawn:?}\n{}",
            sh.transcript()
        );
    } else {
        eprintln!("fish {version} has no transient prompt (4.1+): that row did not run");
    }
    sh.finish();
}
