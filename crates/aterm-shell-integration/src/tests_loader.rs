// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// ─── The loader and the body ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests.rs, whose
// helpers (`LiveShell`, `LiveFixture`, `shell_command`, the re-key probes
// and `deliver`) it uses.

/// A folder address this suite's builds never write: the NEWER build the live
/// tests point a running shell at.
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
        .env("ATERM_CHILD", "1")
        .env_remove("BASH_ENV");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("bash", &mut cmd).holding(cmd.into_home())
}

/// The probe function the NEWER body carries (POSIX function syntax, so the
/// same text serves zsh and bash).
#[cfg(unix)]
const BODY_PROBE_FN: &str = "__aterm_body_probe() { printf '%s\\n' PROBE-NEWER; }";

/// [`BODY_PROBE_FN`] in fish. It reads the last command's status as every
/// earlier body's custom prompt does (`test $__aterm_last_status -ne 0`): a
/// list there stopped an older body a shell was pointed back to with a `test`
/// error at its first prompt.
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
        // fish's hooks are its event handlers: a body re-sourced must not
        // register one twice. Sorted: fish lists one event's handlers in the
        // order they were defined, and a body re-sourced defines its own again
        // after the one the body before it defined at a prompt.
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

    // The newer build, pointed at while the shell idles: at the prompt after the
    // last probe, once its 133;A — which follows that prompt's own body check —
    // is out. Written as soon as the probe's output appeared, the pointer was at
    // times read by that very prompt, before the command below ran.
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
            "{REKEY_NEW}\n{}\n{}\n",
            bf.ours.display(),
            bf.pointer.display()
        ),
    )
    .unwrap();
    let typed = typed_rekey_with_loader(shell, &posix_quoted(&file)).expect("scripted");
    let sent = at;
    // Under the user's `set -x`: the trace names the path, never the key — the
    // commands that expand it traced it in plain text, and so did bash's loader,
    // which the line sources. (fish traces through `fish_trace`; not asserted.)
    let (trace, untrace) = if matches!(shell, ShellType::Zsh | ShellType::Bash) {
        ("set -x; ", "set +x; ")
    } else {
        ("", "")
    };
    sh.send(&format!("{trace}{typed} {untrace}{}", ran(label, 201)));
    let at = sh.wait_for_after("RAN201", at);
    if !trace.is_empty() {
        // The trace's own last line, `set +x`, ends it on stderr.
        let started = std::time::Instant::now();
        while !sh.stderr_text().contains("set +x") {
            assert!(
                started.elapsed() < LiveShell::TIMEOUT,
                "{label}: no trace\n{}",
                sh.stderr_text()
            );
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        // bash writes its prompt, and the signed 133;B in it, on stderr too.
        assert!(
            !cut_osc(&sh.stderr_text()).contains(REKEY_NEW),
            "{label}: the key in the trace\n{}",
            sh.stderr_text()
        );
    }
    // SIGNED AT THE UPGRADE, before the command the line carries runs: that
    // command is the relaunched agent, which holds the terminal for hours, and
    // until the next prompt the window would read `integration_rev=frozen`. And
    // signed with the FILE'S key: signed with the one the shell held — the key a
    // degraded tab's engine lost — the mark is dropped and the shell stays
    // degraded.
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

/// A NESTED SHELL is never integrated by the typed line, at its first line or
/// any later one. A shell started
/// from an integrated tab — a `bash` or `zsh` typed there, a `nix develop` /
/// `pipenv shell` — inherits the EXPORTED guard, so its own load stopped at the
/// guard. When the agent it launched is relaunched, the harness types the
/// upgrade line into IT (it leads the tab's foreground group), with the file
/// the window issued for the tab's OWN shell — at every live agent upgrade of
/// that agent. The line assigns the key to `__aterm_shell_nonce`, and a loader
/// that took that variable as proof the shell was its own upgraded the nested
/// shell "in place" (at the first line while the key went first, at the second
/// once the loader went first): a zsh got every hook wired, a bash had the body
/// run at its top level (its PATH re-fronted to the reroute dir, ahead of what
/// the nested environment put first), and the first revision the nested shell
/// signed read `integration_rev=current` for a tab whose own shell was still
/// frozen — which also shut the window's upgrade off for it for good. The proof
/// is a hook function the shell's own integration defined, which no child shell
/// inherits and no typed line defines.
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
        .env("ATERM_SHELL_INTEGRATION_INSTALLED", "1")
        .env_remove("BASH_ENV");
    let mut sh = LiveShell::spawn("bash", &mut cmd);
    nested_shell_is_left_alone("bash", ShellType::Bash, &mut sh, &bf);
    sh.finish();
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
    let data = hermetic.home().join("data");
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

// ─── A body re-sourced keeps the user's key bindings ───

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
/// keeps it through the typed upgrade. fish reads
/// the vendor conf.d — aterm's wrapper — BEFORE config.fish, so a prompt the
/// user defines there (a framework's, `starship init fish | source`) replaced
/// aterm's wrapper for that shell's whole life. The upgrade in place used to
/// redefine `fish_prompt` as the loader's trampoline regardless, and the user's
/// prompt was gone: the trampoline draws the prompt aterm copied at its first
/// load, before config.fish ran. It replaces only a prompt that is aterm's own,
/// and the prompt drawn after the upgrade is the user's. This one keeps a copy of
/// the prompt it replaced, as conda's hook and a venv's activate.fish do — here
/// the pre-loader build's prompt wrapper, which writes its own marks: the prompt
/// drawn after the upgrade carries one signed 133;A and one signed 133;B (the
/// copy's marks inside the wrapped prompt doubled both at every prompt). And
/// the prompt that copy draws reads the status the wrapper put back before
/// calling it — here `$status` and a `$pipestatus` of its own, as a venv's
/// activate.fish puts them back — not the command's, which it read until that
/// copy, which captures nothing itself, had its first call keep them.
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

/// A LIVE fish whose config.fish defines its OWN prompt — piped to `source`,
/// as `starship init fish | source` defines it — takes a newer body at its next
/// prompt and signs the revision it then runs. fish
/// reads the vendor conf.d before config.fish, so that prompt replaces the
/// loader's trampoline for the shell's life, and with it the trampoline's body
/// check: such a shell left every update's pointer unread and never signed a
/// revision. The check rides the `fish_prompt` EVENT, which fish emits before
/// every prompt whoever draws it — and the prompt drawn is still the user's,
/// reading the status of the command before it.
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

/// A LIVE fish's prompt that the integration WRAPS (one in functions/, wrapped
/// at load — fish's own default prompt is wrapped the same way) reads the
/// `$status` and `$pipestatus` of the command before it exactly as fish hands
/// them to an unwrapped prompt — after `not` too, which inverts the status and
/// leaves `$pipestatus`. Every command the trampoline ran before the prompt used
/// to replace both: fish's default prompt never showed `[1]` after `false`. And
/// draws a Ctrl-C cancels — four in a row, with no command between them (this
/// prompt cancels itself, as a job killed by SIGINT cancels fish's whole
/// evaluation) — leave the next prompt the user's, and marked: a count of prompts
/// being drawn that a cancelled draw never counted back down left every prompt
/// unmarked until the next command, and four of them drew aterm's fallback
/// prompt in place of the user's.
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

/// A LIVE fish that draws aterm's own prompt (`ATERM_PROMPT_STYLE`) still gives
/// the user's prompt back for a command. The shim that makes the load's prompt
/// the one given back ran only on the way to the user's prompt, so under a
/// style `fish_prompt` stayed the loader's trampoline for the shell's life, and
/// `funcsave fish_prompt` wrote it over functions/fish_prompt.fish: a fish
/// without aterm then printed `Unknown command: __aterm_body_check` at every
/// prompt (measured, fish 4.9.3).
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

/// A LIVE fish whose config.fish defines its OWN prompt keeps the prompt marks:
/// every prompt, the first one too, is a 133;A signed with the shell's key, the
/// user's prompt (reading the status of the command before it), a signed 133;B.
/// fish reads the vendor conf.d before config.fish, so such a prompt — what
/// `starship init fish | source` defines — replaced the trampoline for the
/// shell's life, and no prompt carried either mark. This one also keeps a COPY
/// of the trampoline and calls it only some of the time: wrapped, it must never
/// reach itself again through that copy (which recursed to fish's call stack
/// limit, dozens of marks per prompt), and the prompt the copy draws carries
/// its marks once — the signed marks of every prompt string alternate A, B —
/// in German too: a fish built with its message catalogs (a distro's) words its
/// stack trace in German, and the copy, told from the wrapper by that trace,
/// drew its marks again. A prompt written through `string` (fish-async-prompt's,
/// tide's) is drawn on one line with its marks: fish draws each piece written
/// apart on a line of its own, and the command line went below the prompt. A
/// command still sees the user's own `fish_prompt`, the one config.fish defined,
/// which `funced` edits in place. And the prompt is handed what fish hands it:
/// fish 4.1's transient prompt draws it once more as a command starts, with
/// `--final-rendering`, which drew the full prompt again when the wrapper
/// dropped it.
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
    // ... the one config.fish defined, so `funced` edits config.fish in place, as
    // in a fish without aterm: the prompt given back once named the integration's
    // file as where it was copied, which fish 4's funced reads as a prompt with no
    // file of its own, and it wrote functions/fish_prompt.fish instead. (fish 3
    // drops the file from every copy, so no prompt given back keeps it there.)
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
