// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
#[cfg(unix)]
use std::fmt::Write as _;
#[cfg(any(unix, windows))]
use std::process::Command;

const APP_ZSH_RESOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/aterm-mac/Sources/ATermMac/Resources/ShellIntegration/aterm_shell_integration.zsh"
));
const APP_BASH_RESOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/aterm-mac/Sources/ATermMac/Resources/ShellIntegration/aterm_shell_integration.bash"
));
const APP_FISH_RESOURCE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../apps/aterm-mac/Sources/ATermMac/Resources/ShellIntegration/aterm_shell_integration.fish"
));

#[cfg(unix)]
fn run_urlencode_via_shell(
    shell: &str,
    args: &[&str],
    cleanup: &str,
    script_name: &str,
    input: &str,
) -> String {
    let script = format!(
        "{}/src/scripts/{script_name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let command = format!(
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {cleanup}; printf '%s' \"$(__aterm_urlencode \"$ATERM_TEST_CWD\")\""
    );
    let output = shell_command(shell)
        .args(args)
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", script)
        .env("ATERM_TEST_CWD", input)
        .output()
        .unwrap_or_else(|error| panic!("spawn {shell} for shell integration test: {error}"));
    assert!(
        output.status.success(),
        "{shell} should encode {input:?}; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("shell urlencode output should be UTF-8")
}

#[cfg(unix)]
fn bash_shell() -> &'static str {
    if std::path::Path::new("/bin/bash").exists() {
        "/bin/bash"
    } else {
        "bash"
    }
}

#[cfg(unix)]
/// Absolute path to a `zsh` binary, or `None` to skip the spawning tests on a
/// host without zsh installed (mirrors [`fish_shell`]). Probing an absolute path
/// rather than bare `"zsh"` keeps the spawn hermetic and avoids a misleading
/// "No such file or directory" failure where zsh simply isn't present.
fn zsh_shell() -> Option<&'static str> {
    ["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh", "/opt/homebrew/bin/zsh"]
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).exists())
}

/// Spawn a shell with a hermetic environment for integration tests.
///
/// A developer running these tests inside aterm has the shipped integration
/// active in their own shell, which `export`s several vars the scripts read at
/// *load* time. Those leak through `cargo test` into the shells spawned below:
///   - `ATERM_SHELL_INTEGRATION_INSTALLED` trips the "already loaded" guard
///     (`[[ -n "$..." ]] && return`), so the script returns before defining any
///     function and every sourced-script test sees "command not found";
///   - `ATERM_BANNER_B64` is base64-decoded to stdout on load, which would
///     corrupt the exact-stdout assertions in the urlencode/report-cwd tests;
///   - `ATERM_SUITE_VERSION` / `ATERM_PROMPT_STYLE` / `ATERM_DISABLE_PROMPT_TITLES`
///     alter prompt/title behavior.
///
/// Strip them all so each test sources the script fresh regardless of the
/// developer's own active integration. Tests that need a specific var
/// (e.g. ATERM_SHELL_NONCE, or ATERM_PROMPT_STYLE for the prompt-override test)
/// set it AFTER calling this, which overrides the removal.
#[cfg(unix)]
fn shell_command(shell: &str) -> Command {
    let mut cmd = Command::new(shell);
    for var in [
        "ATERM_SHELL_INTEGRATION_INSTALLED",
        "ATERM_SUITE_VERSION",
        "ATERM_BANNER_B64",
        "ATERM_PROMPT_STYLE",
        "ATERM_DISABLE_PROMPT_TITLES",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

#[cfg(unix)]
fn assert_urlencode_cases(shell: &str, args: &[&str], cleanup: &str, script_name: &str) {
    let cases = [
        ("/tmp/foo@bar", "/tmp/foo%40bar"),
        ("/tmp/dir!", "/tmp/dir%21"),
        ("/tmp/[test]", "/tmp/%5Btest%5D"),
        ("/tmp/résumés", "/tmp/r%C3%A9sum%C3%A9s"),
    ];
    for (input, expected) in cases {
        let actual = run_urlencode_via_shell(shell, args, cleanup, script_name, input);
        assert_eq!(actual, expected, "{shell} should percent-encode {input:?}");
    }
}

#[cfg(unix)]
fn run_report_cwd_via_shell(
    shell: &str,
    args: &[&str],
    cleanup: &str,
    script_name: &str,
    cwd: &std::path::Path,
) -> String {
    let script = format!(
        "{}/src/scripts/{script_name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let command = format!(
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {cleanup}; builtin cd -- \"$ATERM_TEST_CWD\"; __aterm_report_cwd"
    );
    let output = shell_command(shell)
        .args(args)
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", script)
        .env("ATERM_TEST_CWD", cwd)
        .env("HOSTNAME", "aterm.test")
        .env("HOST", "aterm.test")
        .output()
        .unwrap_or_else(|error| panic!("spawn {shell} for shell integration test: {error}"));
    assert!(
        output.status.success(),
        "{shell} should report OSC 7 for {cwd:?}; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("shell OSC 7 output should be UTF-8")
}

#[cfg(unix)]
fn osc7_percent_encode(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.' | b'~' | b'/' | b'-' => {
                encoded.push(char::from(byte))
            }
            _ => write!(&mut encoded, "%{byte:02X}").expect("write to String"),
        }
    }
    encoded
}

#[cfg(unix)]
fn create_special_cwd() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm_osc7_")
        .tempdir()
        .expect("create tempdir for OSC 7 shell integration test");
    let cwd = dir.path().join("résumé @[test]!");
    std::fs::create_dir(&cwd).expect("create special-character cwd for OSC 7 test");
    (dir, cwd)
}

#[test]
fn test_zsh_prompt_hook_autoload_precedes_registration() {
    let autoload = scripts::ZSH
        .find("autoload -Uz add-zsh-hook")
        .expect("zsh script should autoload add-zsh-hook");
    let deferred_prompt = scripts::ZSH
        .find("add-zsh-hook precmd __aterm_first_precmd")
        .expect("zsh script should register deferred prompt hook");

    assert!(
        autoload < deferred_prompt,
        "autoload must run before prompt hook registration"
    );
}

#[test]
fn test_app_zsh_resource_matches_embedded_script() {
    assert_eq!(
        scripts::ZSH,
        APP_ZSH_RESOURCE,
        "app zsh resource must stay byte-identical to the embedded canonical script"
    );
}

/// Static guard (host-portable): the `shell.d` hook globs MUST carry zsh's `(N)`
/// NULL_GLOB qualifier. Without it, a `shell.d` holding only `*.zsh` drop-ins (the
/// exact layout `atpkg` installs: `00-atpkg.zsh`, no `*.sh`) makes the `*.sh` glob
/// raise NOMATCH, which ABORTS the entire sourced script — silently killing OSC 7
/// (cwd) + OSC 133 (command blocks) and printing an error on the first line of every
/// session (found by the v0.26 demo-day battery). Bash is exempt: an unmatched glob
/// stays literal there and the `[ -f ]` guard skips it.
#[test]
fn test_zsh_shell_d_globs_are_null_glob_guarded() {
    assert!(
        scripts::ZSH.contains(r#""$HOME/.aterm/shell.d"/*.zsh(N)"#)
            && scripts::ZSH.contains(r#""$HOME/.aterm/shell.d"/*.sh(N)"#),
        "the zsh shell.d source loop must use the (N) NULL_GLOB qualifier on BOTH globs, \
         or an atpkg-populated shell.d (only *.zsh) aborts the whole integration"
    );
}

/// Functional repro (zsh hosts only): source the real script under an interactive
/// zsh whose `shell.d` holds a `*.zsh` drop-in but NO `*.sh` — the atpkg layout that
/// triggered the abort. The integration must survive: its functions get defined
/// (`__aterm_urlencode` resolves) AND the drop-in is sourced (its sentinel export
/// reaches the environment) AND stderr carries no "no matches found".
#[cfg(unix)]
#[test]
fn test_zsh_survives_shell_d_with_only_zsh_dropins() {
    let Some(zsh) = zsh_shell() else {
        eprintln!("SKIP: no zsh on this host");
        return;
    };
    let home = std::env::temp_dir().join(format!("aterm-shd-{}", std::process::id()));
    let shd = home.join(".aterm/shell.d");
    std::fs::create_dir_all(&shd).expect("mk shell.d");
    std::fs::write(shd.join("00-atpkg.zsh"), "export ATERM_TEST_DROPIN=loaded\n")
        .expect("write dropin");
    let script = format!("{}/src/scripts/aterm_shell_integration.zsh", env!("CARGO_MANIFEST_DIR"));
    // Interactive (`-i`) so the `[[ -o interactive ]] || return` guard doesn't
    // short-circuit; hermetic HOME so ONLY our drop-in is present.
    let out = shell_command(zsh)
        .arg("-i")
        .arg("-c")
        .arg("source \"$ATERM_TEST_SCRIPT\"; \
              print -r -- \"URLENC=$(whence -w __aterm_urlencode)\"; \
              print -r -- \"DROPIN=${ATERM_TEST_DROPIN:-UNSET}\"")
        .env("HOME", &home)
        .env("ZDOTDIR", &home)
        .env("ATERM_TEST_SCRIPT", &script)
        .output()
        .expect("spawn zsh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&home);
    assert!(
        !stderr.contains("no matches found"),
        "shell.d glob aborted the script: stderr={stderr:?}"
    );
    assert!(
        stdout.contains("URLENC=__aterm_urlencode: function"),
        "integration functions must be defined (script ran to completion): stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("DROPIN=loaded"),
        "the *.zsh shell.d drop-in must be sourced: stdout={stdout:?}"
    );
}

#[test]
fn test_app_bash_resource_matches_embedded_script() {
    assert_eq!(
        scripts::BASH,
        APP_BASH_RESOURCE,
        "app bash resource must stay byte-identical to the embedded canonical script"
    );
}

#[test]
fn test_app_fish_resource_matches_embedded_script() {
    assert_eq!(
        scripts::FISH,
        APP_FISH_RESOURCE,
        "app fish resource must stay byte-identical to the embedded canonical script"
    );
}

#[cfg(unix)]
#[test]
fn test_bash_urlencode_handles_special_and_unicode_paths() {
    assert_urlencode_cases(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
        "aterm_shell_integration.bash",
    );
}

#[cfg(unix)]
#[test]
fn test_zsh_urlencode_handles_special_and_unicode_paths() {
    let Some(zsh) = zsh_shell() else {
        return;
    };
    assert_urlencode_cases(
        zsh,
        &["-f", "-i"],
        "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true",
        "aterm_shell_integration.zsh",
    );
}

#[cfg(unix)]
#[test]
fn test_bash_report_cwd_emits_percent_encoded_osc_7() {
    let (_dir, cwd) = create_special_cwd();
    let cwd_string = cwd.to_str().expect("cwd path should be UTF-8");
    let actual = run_report_cwd_via_shell(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
        "aterm_shell_integration.bash",
        &cwd,
    );
    let expected = format!(
        "\u{1b}]7;file://aterm.test{}\u{7}",
        osc7_percent_encode(cwd_string)
    );
    assert_eq!(
        actual, expected,
        "bash should emit a percent-encoded OSC 7 file URI for the live cwd"
    );
}

/// REGRESSION (stock Ubuntu bash): PROMPT_COMMAND as an ARRAY runs every element
/// as its own top-level command AFTER `__aterm_prompt_command` returned — the
/// in-prompt guard flag is already clear, and the old scalar compare saw only
/// element 0. A SIBLING integration's precmd (`__vte_prompt_command`, starship,
/// systemd's precmdline, ...) was then captured as the user's command: its 133;C
/// fired at the prompt (out of phase, dropped by the A→B→C→D machine) and
/// `__aterm_last_command` stayed occupied, so the REAL command never emitted
/// 633;E/133;C — no block ever reached Executing, and a driver's verified submit
/// (`turn`) could never attribute a press. The preexec must skip EVERY
/// PROMPT_COMMAND element and still capture the real command that follows.
#[cfg(unix)]
#[test]
fn test_bash_preexec_skips_prompt_command_array_siblings() {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    // One prompt cycle exactly as bash runs an array PROMPT_COMMAND: each element
    // is a top-level DEBUG-trapped command, ours first, the sibling after ours
    // returned. The marker rides an `__aterm_`-prefixed function so the guard
    // skips it identically before and after the fix.
    let command = "\
__fake_vte_precmd() { :; }\n\
PROMPT_COMMAND=(__fake_vte_precmd)\n\
source \"$ATERM_TEST_SCRIPT\"\n\
__aterm_prompt_command\n\
__fake_vte_precmd\n\
__aterm_test_marker() { printf '===CYCLE-DONE==='; }\n\
__aterm_test_marker\n\
echo real-command\n";
    let output = shell_command(bash_shell())
        .args(["--noprofile", "--norc", "-i", "-c", command])
        .env("ATERM_TEST_SCRIPT", &script)
        .output()
        .unwrap_or_else(|error| panic!("spawn bash for preexec array test: {error}"));
    assert!(
        output.status.success(),
        "bash preexec array cycle should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let (prompt_cycle, after) = stdout
        .split_once("===CYCLE-DONE===")
        .expect("marker should appear in stdout");
    assert!(
        !prompt_cycle.contains("633;E"),
        "a sibling PROMPT_COMMAND element must not be captured as a user command: {prompt_cycle:?}"
    );
    assert!(
        !prompt_cycle.contains("\u{1b}]133;C"),
        "no command-start mark may fire during the prompt cycle: {prompt_cycle:?}"
    );
    assert!(
        after.contains("\u{1b}]633;E;echo\\x20real-command\u{7}"),
        "the real command must still be captured (633;E): {after:?}"
    );
    assert!(
        after.contains("\u{1b}]133;C\u{7}"),
        "the real command must emit the 133;C command start: {after:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_zsh_report_cwd_emits_percent_encoded_osc_7() {
    let Some(zsh) = zsh_shell() else {
        return;
    };
    let (_dir, cwd) = create_special_cwd();
    let cwd_string = cwd.to_str().expect("cwd path should be UTF-8");
    let actual = run_report_cwd_via_shell(
        zsh,
        &["-f", "-i"],
        "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true",
        "aterm_shell_integration.zsh",
        &cwd,
    );
    let expected = format!(
        "\u{1b}]7;file://aterm.test{}\u{7}",
        osc7_percent_encode(cwd_string)
    );
    assert_eq!(
        actual, expected,
        "zsh should emit a percent-encoded OSC 7 file URI for the live cwd"
    );
}

#[cfg(unix)]
#[test]
fn test_prepare_zsh_prompt_override_starts_without_hook_error() {
    let Some(zsh) = zsh_shell() else {
        return;
    };
    let dir = aterm_tempfile::tempdir().expect("create tempdir for shell integration test");
    let base = dir.path().join("si");
    let InjectionEnv { env_add, .. } = prepare_into(ShellType::Zsh, &base)
        .expect("prepare shell integration")
        .expect("zsh shell integration should produce an injection environment");

    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("create temporary home directory");
    std::fs::write(home.join(".zshenv"), "").expect("write empty user .zshenv");

    // Hermetic spawn: strip the dev shell's exported integration vars (above all
    // ATERM_SHELL_INTEGRATION_INSTALLED) so the wrapper actually sources the
    // integration instead of short-circuiting on the "already loaded" guard.
    // Without this, the add-zsh-hook autoload-ordering check below is a false
    // green — the script returns before reaching add-zsh-hook at all. The probe
    // for $+functions[__aterm_precmd] asserts the hooks were really registered.
    let mut command = shell_command(zsh);
    command
        .arg("-i")
        .arg("-c")
        .arg(
            "print -r -- PROMPT_ENV_OK; \
             (( $+functions[__aterm_precmd] )) && print -r -- ATERM_INTEGRATION_LOADED",
        )
        .env("HOME", &home)
        .env("ATERM_PROMPT_STYLE", "minimal")
        .env_remove("ZDOTDIR");
    for (key, value) in env_add {
        command.env(key, value);
    }
    command
        .env_remove("ATERM_ORIGINAL_ZDOTDIR")
        .env("ATERM_UNSET_ZDOTDIR", "1");

    let output = command
        .output()
        .expect("spawn prompt-enabled zsh with embedded shell integration");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success(),
        "prompt-enabled zsh should exit cleanly; stdout: {stdout:?}; stderr: {stderr:?}"
    );
    assert!(
        combined.contains("PROMPT_ENV_OK"),
        "prompt-enabled zsh should run the test command; stdout: {stdout:?}; stderr: {stderr:?}"
    );
    assert!(
        !combined.contains("command not found: add-zsh-hook"),
        "embedded zsh script should autoload add-zsh-hook before registration; stdout: {stdout:?}; stderr: {stderr:?}"
    );
    // Positive guard: the integration must have actually loaded (functions
    // defined + hooks registered), not merely "not errored". This is what makes
    // the hook-ordering assertion above meaningful instead of a false green when
    // a stray ATERM_SHELL_INTEGRATION_INSTALLED would short-circuit the script.
    assert!(
        combined.contains("ATERM_INTEGRATION_LOADED"),
        "embedded zsh integration must define its precmd hook (functions registered); stdout: {stdout:?}; stderr: {stderr:?}"
    );
}

#[test]
fn test_detect_zsh() {
    assert_eq!(ShellType::detect("/bin/zsh"), ShellType::Zsh);
    assert_eq!(ShellType::detect("/usr/local/bin/zsh"), ShellType::Zsh);
}

#[test]
fn test_detect_bash() {
    assert_eq!(ShellType::detect("/bin/bash"), ShellType::Bash);
    assert_eq!(ShellType::detect("bash5"), ShellType::Bash);
}

#[test]
fn test_detect_fish() {
    assert_eq!(ShellType::detect("/usr/bin/fish"), ShellType::Fish);
}

#[test]
fn test_detect_unknown() {
    assert_eq!(ShellType::detect("/bin/sh"), ShellType::Unknown);
    assert_eq!(ShellType::detect(""), ShellType::Unknown);
}

#[test]
fn test_scripts_embedded() {
    assert!(scripts::ZSH.contains("ATERM_SHELL_INTEGRATION_INSTALLED"));
    assert!(scripts::BASH.contains("ATERM_SHELL_INTEGRATION_INSTALLED"));
    assert!(scripts::FISH.contains("ATERM_SHELL_INTEGRATION_INSTALLED"));
}

#[test]
fn test_scripts_contain_prompt_override() {
    assert!(scripts::ZSH.contains("ATERM_PROMPT_STYLE"));
    assert!(scripts::BASH.contains("ATERM_PROMPT_STYLE"));
    assert!(scripts::FISH.contains("ATERM_PROMPT_STYLE"));
}

#[test]
fn test_cwd_tab_title_strips_control_chars() {
    // A directory name may contain any byte except '/' and NUL, so a crafted
    // name could embed BEL/ESC and smuggle a nested OSC (e.g. a clipboard
    // write) out of the OSC 0 tab title. The CWD-title path must strip control
    // bytes exactly like the command-title path already does — these asserts
    // pin the guard so it cannot silently regress.
    assert!(
        scripts::ZSH.contains("\"0;${__aterm_tab_title//[[:cntrl:]]/}\""),
        "zsh CWD tab title must strip control characters"
    );
    assert!(
        scripts::BASH.contains("\"0;${__aterm_tab_title//[[:cntrl:]]/}\""),
        "bash CWD tab title must strip control characters"
    );
    // fish strips both the ~-abbreviated and the absolute CWD forms.
    assert!(
        scripts::FISH.contains(r#"string replace -ra '[\x00-\x1f\x7f]' '' -- "~$rel""#),
        "fish abbreviated CWD tab title must strip control characters"
    );
    assert!(
        scripts::FISH.contains(r#"string replace -ra '[\x00-\x1f\x7f]' '' -- "$PWD""#),
        "fish absolute CWD tab title must strip control characters"
    );
}

#[test]
fn test_fish_encode_cmd_escapes_all_control_bytes() {
    // The OSC 633;E command-line encoder (__aterm_encode_cmd) must backslash-
    // hex-escape EVERY control byte (0x00-0x1f, 0x7f) — not just the six
    // whitespace bytes it special-cases. A raw ESC (0x1b) or BEL (0x07) in the
    // command line (reachable via Ctrl-V verbatim insert, paste, or tab-
    // completing a filename that contains control bytes) terminates an OSC
    // string in the parser, so an unescaped one would prematurely close the
    // OSC 633;E sequence and let the following bytes be parsed as fresh control
    // sequences — a classic OSC break-out (e.g. smuggling an OSC 52 clipboard
    // write). zsh/bash escape the whole [[:cntrl:]] class; fish must match.
    // This pins fish's guard so the pre-fix verbatim `case '*'` passthrough
    // cannot silently regress even on hosts where fish is not installed.
    assert!(
        scripts::FISH.contains(r#"string match -qr '[\x00-\x1f\x7f]' -- "$i""#),
        "fish __aterm_encode_cmd must hex-escape all C0/DEL control bytes \
         rather than passing them through verbatim"
    );
    // The app-bundle copy is the fish script actually shipped to macOS users,
    // so it must carry the identical guard. (Byte-for-byte identity with the
    // canonical script is separately enforced by
    // test_app_fish_resource_matches_embedded_script.)
    assert!(
        APP_FISH_RESOURCE.contains(r#"string match -qr '[\x00-\x1f\x7f]' -- "$i""#),
        "shipped fish resource must also hex-escape all C0/DEL control bytes"
    );
}

#[cfg(unix)]
#[test]
fn test_fish_encode_cmd_emits_no_raw_control_bytes() {
    let Some(fish) = fish_shell() else {
        // fish is not universally installed; skip gracefully instead of
        // failing CI. The static guard in
        // test_fish_encode_cmd_escapes_all_control_bytes covers this host.
        eprintln!("fish not installed; skipping test_fish_encode_cmd_emits_no_raw_control_bytes");
        return;
    };
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.fish",
        env!("CARGO_MANIFEST_DIR")
    );
    // A command line carrying a raw ESC (0x1b) and BEL (0x07) around benign
    // text, plus a backslash and semicolon to confirm those keep their escapes.
    let mut input = String::new();
    input.push('a');
    input.push('\u{1b}'); // ESC — terminates an OSC string in the parser
    input.push('b');
    input.push('\u{07}'); // BEL — the ST for OSC strings; also breaks out
    input.push('\\');
    input.push(';');
    let command =
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; printf '%s' \"$(__aterm_encode_cmd \"$ATERM_TEST_INPUT\")\""
            .to_string();
    let output = shell_command(fish)
        .arg("-i")
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", script)
        .env("ATERM_TEST_INPUT", &input)
        .output()
        .unwrap_or_else(|error| panic!("spawn fish for encode-cmd test: {error}"));
    assert!(
        output.status.success(),
        "fish encode-cmd invocation should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = output.stdout;
    // The emitted OSC 633;E payload must contain NO raw ESC/BEL — the whole
    // point of the fix — otherwise the sequence breaks out in the parser.
    assert!(
        !stdout.contains(&0x1b) && !stdout.contains(&0x07),
        "fish encoder must not emit raw ESC/BEL bytes; got: {stdout:?}"
    );
    let text = String::from_utf8(stdout).expect("encoder output should be UTF-8");
    // ...they must instead appear as the decoder's \xNN escapes.
    assert!(
        text.contains(r"\x1b"),
        "ESC (0x1b) must be hex-escaped as \\x1b; got: {text:?}"
    );
    assert!(
        text.contains(r"\x07"),
        "BEL (0x07) must be hex-escaped as \\x07; got: {text:?}"
    );
    // Backslash and semicolon keep their existing OSC 633 escapes.
    assert!(
        text.contains(r"\\") && text.contains(r"\x3b"),
        "backslash and semicolon must stay escaped; got: {text:?}"
    );
}

// ─── The OSC frame writer must not re-interpret its payload ──────────────
//
// `__aterm_osc` wraps an already-built payload in `ESC ] … BEL`. It must copy
// that payload through byte-for-byte. zsh's `print` WITHOUT `-r` does not: it
// expands escape sequences in its argument, which silently undid every escape
// `__aterm_encode_cmd` had just produced and re-materialized the exact raw
// ESC/BEL bytes the encoder exists to remove. bash always spelled the writer
// `printf '\033]%s\a' "$1"` (a `%s` argument is never interpreted) and was
// never affected, so the two shells being byte-equal here is the real check.

/// Command line for the frame-writer probe. It carries two independent
/// break-out attempts:
///   * raw ESC (0x1b) and BEL (0x07) bytes, which `__aterm_encode_cmd` must
///     hex-escape and the frame writer must then leave as escapes; and
///   * the LITERAL text `\e]52;c;aGVsbG8=\a` — ordinary printable characters,
///     so the tab title's `${…//[[:cntrl:]]/}` guard has no control byte to
///     strip. Only a frame writer that expands escapes can turn that text into
///     a real nested OSC 52 clipboard write.
#[cfg(unix)]
const OSC_PROBE_CMDLINE: &str = "a\u{1b}b\u{7}c;d e\\f \\e]52;c;aGVsbG8=\\a";

/// OSC 633;E for [`OSC_PROBE_CMDLINE`]: every reserved byte still spelled as
/// the decoder's backslash escape, and exactly one BEL — the terminator.
#[cfg(unix)]
const OSC_PROBE_EXPECTED_633: &str = concat!(
    "\u{1b}]633;E;",
    r"a\x1bb\x07c\x3bd\x20e\\f\x20\\e]52\x3bc\x3baGVsbG8=\\a",
    "\u{7}"
);

/// OSC 0 title for [`OSC_PROBE_CMDLINE`]: the control bytes are stripped
/// (`a<ESC>b<BEL>c` → `abc`) and the literal `\e`/`\a` text stays literal.
#[cfg(unix)]
const OSC_PROBE_EXPECTED_TITLE: &str =
    concat!("\u{1b}]0;", r"abc;d e\f \e]52;c;aGVsbG8=\a", "\u{7}");

/// Drive the two emissions `preexec` performs, through the real
/// `__aterm_osc` frame writer, and return the raw stdout bytes.
///
/// The snippet is spelled identically for bash and zsh — both write command
/// substitution as `$(…)` and control-stripping as `${v//[[:cntrl:]]/}` — so
/// the same source line is exercised in both shells.
#[cfg(unix)]
fn run_osc_wire_probe(
    shell: &str,
    args: &[&str],
    cleanup: &str,
    script_name: &str,
) -> Vec<u8> {
    let script = format!(
        "{}/src/scripts/{script_name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let command = format!(
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {cleanup}; \
         __aterm_osc \"633;E;$(__aterm_encode_cmd \"$ATERM_TEST_INPUT\")\"; \
         __aterm_osc \"0;${{ATERM_TEST_INPUT//[[:cntrl:]]/}}\""
    );
    let output = shell_command(shell)
        .args(args)
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", script)
        .env("ATERM_TEST_INPUT", OSC_PROBE_CMDLINE)
        // No nonce: the probe pins the frame bytes, and a ";id=<hex>" tail
        // would just pad every expected string without exercising anything.
        .env_remove("ATERM_SHELL_NONCE")
        .output()
        .unwrap_or_else(|error| panic!("spawn {shell} for OSC frame-writer probe: {error}"));
    assert!(
        output.status.success(),
        "{shell} OSC frame-writer probe should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[cfg(unix)]
fn assert_osc_wire_is_escape_transparent(shell: &str, wire: &[u8]) {
    let expected = format!("{OSC_PROBE_EXPECTED_633}{OSC_PROBE_EXPECTED_TITLE}");
    // `ends_with`, not `==`: an interactive bash may prefix stdout with its
    // readline meta-mode probe on some hosts. Everything the writer emits is
    // pinned exactly; only a leading terminal-init blob is tolerated.
    assert!(
        wire.ends_with(expected.as_bytes()),
        "{shell} OSC frames must reproduce the payload byte-for-byte;\n  expected suffix: {:?}\n  got:             {:?}",
        expected,
        String::from_utf8_lossy(wire)
    );
    // The break-out itself, stated directly: no nested OSC introducer may
    // appear anywhere on the wire. Under the `print -n` writer this failed —
    // stdout carried a real `ESC ]52;c;aGVsbG8=` clipboard write.
    assert!(
        !wire.windows(5).any(|w| w == b"\x1b]52;"),
        "{shell} must not smuggle a nested OSC 52 out of the frame; got: {:?}",
        String::from_utf8_lossy(wire)
    );
    // ...and the 633;E payload must carry no raw ESC/BEL, so the sequence
    // cannot terminate before its own BEL. Everything between the introducer
    // and the first BEL is the payload.
    const INTRODUCER: &str = "\u{1b}]633;E;";
    let after_introducer = &wire[wire.len() - expected.len() + INTRODUCER.len()..];
    let payload_len = after_introducer
        .iter()
        .position(|&b| b == 0x07)
        .expect("OSC 633;E must be BEL-terminated");
    let payload = &after_introducer[..payload_len];
    assert!(
        !payload.contains(&0x1b) && !payload.contains(&0x07),
        "{shell} 633;E payload must carry no raw ESC/BEL; got: {:?}",
        String::from_utf8_lossy(payload)
    );
}

/// Static guard (host-portable, mirrors the fish one above): the frame writers
/// must stay on `printf`, whose `%s` argument is copied verbatim. Reverting zsh
/// to `print -n` — which expands escapes in its argument — silently re-opens the
/// break-out on hosts where the functional probe below cannot run.
#[test]
fn test_osc_frame_writers_use_printf() {
    for (name, script) in [("zsh", scripts::ZSH), ("bash", scripts::BASH)] {
        assert!(
            script.contains("__aterm_osc() {\n    printf '\\033]%s\\a' \"$1\"\n}"),
            "{name} __aterm_osc must frame with printf '%%s' so the payload is \
             copied byte-for-byte (zsh's `print` without -r expands escapes in \
             its argument and re-materializes raw ESC/BEL inside the OSC string)"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_bash_osc_frame_writer_preserves_payload_escapes() {
    let wire = run_osc_wire_probe(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
        "aterm_shell_integration.bash",
    );
    assert_osc_wire_is_escape_transparent("bash", &wire);
}

#[cfg(unix)]
#[test]
fn test_zsh_osc_frame_writer_preserves_payload_escapes() {
    let Some(zsh) = zsh_shell() else {
        eprintln!("SKIP: no zsh on this host");
        return;
    };
    let wire = run_osc_wire_probe(
        zsh,
        &["-f", "-i"],
        "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true",
        "aterm_shell_integration.zsh",
    );
    assert_osc_wire_is_escape_transparent("zsh", &wire);
}

#[test]
fn test_prepare_writes_scripts() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");

    let injection = prepare_into(ShellType::Zsh, &base).unwrap().unwrap();

    let keys: Vec<&str> = injection.env_add.iter().map(|(k, _)| k.as_str()).collect();
    assert!(keys.contains(&"ZDOTDIR"));
    assert!(keys.contains(&"ATERM_SHELL_INTEGRATION_DIR"));

    assert!(base.join("aterm_shell_integration.zsh").exists());
    assert!(base.join("aterm_shell_integration.bash").exists());
    assert!(base.join("aterm_shell_integration.fish").exists());
    assert!(base.join("aterm_shell_integration.ps1").exists());
    assert!(base.join("zdotdir").join(".zshenv").exists());
}

#[test]
fn test_prepare_bash_has_argv_override() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");

    let result = prepare_into(ShellType::Bash, &base).unwrap().unwrap();
    assert!(result.argv_override.is_some());
    let argv = result.argv_override.unwrap();
    assert_eq!(argv[0], "bash");
    assert_eq!(argv[1], "--rcfile");
}

#[test]
fn test_prepare_unknown_returns_none() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    assert!(prepare_into(ShellType::Unknown, &base).unwrap().is_none());
    // An unknown shell must not litter the cache dir with scripts — on
    // Windows the old fallback wrote them to C:\tmp on every spawn.
    assert!(
        !base.exists(),
        "prepare_into(Unknown) must not create the cache directory"
    );
}

#[test]
fn test_prepare_cached_skips_rewrite_and_self_heals() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let written = std::sync::Mutex::new(None);

    // First call writes everything.
    prepare_cached(ShellType::Zsh, base.clone(), &written)
        .unwrap()
        .unwrap();
    let zsh = base.join("aterm_shell_integration.zsh");
    assert_eq!(std::fs::read_to_string(&zsh).unwrap(), scripts::ZSH);

    // Second call with the same base must skip the writes: clobber the
    // script with a sentinel and verify it is NOT restored.
    std::fs::write(&zsh, "sentinel").unwrap();
    prepare_cached(ShellType::Zsh, base.clone(), &written)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&zsh).unwrap(),
        "sentinel",
        "cached prepare must not rewrite scripts for an already-written base"
    );

    // Self-healing: deleting the primary script (the common whole-dir
    // deletion collapses to this) forces a full rewrite on the next spawn.
    std::fs::remove_file(&zsh).unwrap();
    prepare_cached(ShellType::Zsh, base.clone(), &written)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&zsh).unwrap(),
        scripts::ZSH,
        "prepare must rewrite scripts when the cache dir was deleted mid-run"
    );
}

#[test]
fn test_prepare_cached_rewrites_on_base_change() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base_a = dir.path().join("a");
    let base_b = dir.path().join("b");
    let written = std::sync::Mutex::new(None);

    prepare_cached(ShellType::Bash, base_a.clone(), &written)
        .unwrap()
        .unwrap();
    // A different base (containment mode / XDG change) must write fresh.
    prepare_cached(ShellType::Bash, base_b.clone(), &written)
        .unwrap()
        .unwrap();
    assert!(base_b.join("aterm_shell_integration.bash").exists());

    // Returning to the first base must write again — the memo keys a
    // single base, never a set.
    let a_zsh = base_a.join("aterm_shell_integration.zsh");
    std::fs::write(&a_zsh, "sentinel").unwrap();
    prepare_cached(ShellType::Bash, base_a.clone(), &written)
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read_to_string(&a_zsh).unwrap(), scripts::ZSH);
}

#[test]
fn test_prepare_cached_retries_after_write_error() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    // A regular file at the base path makes create_dir_all fail.
    std::fs::write(&base, "").unwrap();
    let written = std::sync::Mutex::new(None);
    assert!(prepare_cached(ShellType::Zsh, base.clone(), &written).is_err());

    // The failed attempt must not have been recorded: once the obstruction
    // is gone the next spawn writes the scripts.
    std::fs::remove_file(&base).unwrap();
    prepare_cached(ShellType::Zsh, base.clone(), &written)
        .unwrap()
        .unwrap();
    assert!(base.join("aterm_shell_integration.zsh").exists());
}

#[test]
fn test_prepare_fish_xdg_data_dirs() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");

    let result = prepare_into(ShellType::Fish, &base).unwrap().unwrap();
    let xdg = result.env_add.iter().find(|(k, _)| k == "XDG_DATA_DIRS");
    assert!(xdg.is_some());
    assert!(xdg.unwrap().1.contains("fish-xdg"));
}

#[test]
fn test_zsh_wrapper_restores_zdotdir_and_sources_integration() {
    let wrapper = ZSH_WRAPPER;
    assert!(
        wrapper.contains("ATERM_ORIGINAL_ZDOTDIR"),
        "zsh wrapper must restore original ZDOTDIR"
    );
    assert!(
        wrapper.contains("ATERM_UNSET_ZDOTDIR"),
        "zsh wrapper must handle ZDOTDIR-was-unset case"
    );
    assert!(
        wrapper.contains("source \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.zsh\""),
        "zsh wrapper must source integration script"
    );
    assert!(
        wrapper.contains(".zshenv"),
        "zsh wrapper must source user's .zshenv"
    );
}

#[test]
fn test_bash_wrapper_sources_profile_chain_and_bashrc() {
    let wrapper = BASH_WRAPPER;
    assert!(
        wrapper.contains("/etc/profile"),
        "bash wrapper must source /etc/profile"
    );
    assert!(
        wrapper.contains(".bash_profile"),
        "bash wrapper must source .bash_profile"
    );
    assert!(
        wrapper.contains(".bashrc"),
        "bash wrapper must source .bashrc (--rcfile skips it)"
    );
    assert!(
        wrapper.contains("aterm_shell_integration.bash"),
        "bash wrapper must source integration script"
    );
}

// NOTE: the two tests that exercised prepare_zsh/prepare_fish with the
// relevant env var REMOVED (`ZDOTDIR`, `XDG_DATA_DIRS`) do not live here:
// this unit binary runs its tests on parallel threads, and mutating the
// process environment races every sibling test that calls `prepare_into`
// (which reads those vars). They are single-test-per-binary integration
// tests instead: tests/prepare_zsh_unset_zdotdir.rs and
// tests/prepare_fish_xdg_default.rs. Do NOT add env-mutating tests to
// this file — give each its own binary under tests/.

#[test]
fn test_bash_133b_embedded_in_custom_ps1() {
    let script = scripts::BASH;
    // #7987: the embedded 133;B now carries an optional ;id=<hex>
    // capability-nonce tail derived from ATERM_SHELL_NONCE. The nonce
    // interpolation must be inside the \[ \] group so bash does not
    // count it against visible prompt width.
    assert!(
        script.contains(r#"local mark_b="\[\033]133;B${mark_b_id}\a\]""#),
        "bash custom prompt must embed 133;B (with optional ;id=<hex>) in PS1"
    );
    assert!(
        script.contains("__aterm_prompt_has_mark_b=1"),
        "bash must flag that custom prompt has embedded 133;B"
    );
}

#[test]
fn test_bash_default_mode_embeds_133b_in_ps1() {
    let script = scripts::BASH;
    // #7987: the default-mode embed now uses a BASH_REMATCH-based strip
    // to tolerate nonce rotations (PS1 may already have a stale id=<hex>
    // tail). After stripping, the new 133;B (with the current nonce) is
    // appended back onto PS1.
    assert!(
        script.contains(r#"PS1="${PS1}${__aterm_b}""#),
        "bash default mode must append 133;B (with optional ;id=<hex>) to PS1"
    );
    assert!(
        script.contains(r#"local __aterm_b="\[\033]133;B${__aterm_b_suffix}\a\]""#),
        "bash default-mode 133;B builder must include the ATERM_SHELL_NONCE-derived suffix"
    );
}

#[test]
fn test_bash_prompt_command_guard_prevents_spurious_capture() {
    let script = scripts::BASH;
    assert!(
        script.contains("__aterm_in_prompt_cmd=1"),
        "bash must set guard flag at start of PROMPT_COMMAND"
    );
    assert!(
        script.contains("__aterm_in_prompt_cmd=0"),
        "bash must clear guard flag at end of PROMPT_COMMAND"
    );
    assert!(
        script.contains("(( __aterm_in_prompt_cmd )) && return"),
        "bash preexec must check guard flag to skip PROMPT_COMMAND commands"
    );
}

#[test]
fn test_fish_powerline_sep_uses_separator_color() {
    let script = scripts::FISH;
    assert!(
        script.contains(r#"set -l sep (set_color $sc)"""#),
        "fish powerline must color separator glyphs with sep_color"
    );
}

// ─── Capability-nonce emission tests (#7987) ────────────────────────────
//
// The shipped shell integration scripts MUST reference ATERM_SHELL_NONCE
// and emit ";id=<hex>" on every OSC 133/633 sub-op when the env var is
// set. Without this, the host-side nonce defense added in #7960 ships
// client-broken — hosts that flip
// `TerminalModes::require_shell_integration_nonce` to true silently drop
// every legitimate shell-integration emission.
//
// These regex-level tests guard against the scripts regressing to the
// un-nonced form. Full functional verification (spawn a real shell,
// check the wire emits id=<hex>) lives in the PTY integration tests
// where a host is available to authorize the nonce.

#[test]
fn test_bash_script_references_shell_nonce_env() {
    let script = scripts::BASH;
    assert!(
        script.contains("ATERM_SHELL_NONCE"),
        "bash script must reference ATERM_SHELL_NONCE to honor \
         the #7960/#7987 capability-nonce defense"
    );
}

#[test]
fn test_zsh_script_references_shell_nonce_env() {
    let script = scripts::ZSH;
    assert!(
        script.contains("ATERM_SHELL_NONCE"),
        "zsh script must reference ATERM_SHELL_NONCE to honor \
         the #7960/#7987 capability-nonce defense"
    );
}

#[test]
fn test_fish_script_references_shell_nonce_env() {
    let script = scripts::FISH;
    assert!(
        script.contains("ATERM_SHELL_NONCE"),
        "fish script must reference ATERM_SHELL_NONCE to honor \
         the #7960/#7987 capability-nonce defense"
    );
}

#[test]
fn test_bash_script_defines_id_suffix_helper() {
    let script = scripts::BASH;
    assert!(
        script.contains("__aterm_id_suffix"),
        "bash script must define __aterm_id_suffix helper"
    );
    assert!(
        script.contains("printf ';id=%s'"),
        "bash id suffix must emit ';id=<hex>' via printf so the \
         OSC parameter is well-formed for the host scanner"
    );
}

#[test]
fn test_zsh_script_defines_id_suffix_helper() {
    let script = scripts::ZSH;
    assert!(
        script.contains("__aterm_id_suffix"),
        "zsh script must define __aterm_id_suffix helper"
    );
    // #8015: after capture the env var is unset, so the emission reads
    // from the shell-local `$__aterm_shell_nonce` instead of the exported
    // env var — this prevents the nonce from leaking into subprocesses.
    assert!(
        script.contains(";id=${__aterm_shell_nonce}"),
        "zsh id suffix must emit ';id=<hex>' from the captured shell-local"
    );
}

#[test]
fn test_fish_script_defines_id_suffix_helper() {
    let script = scripts::FISH;
    assert!(
        script.contains("__aterm_id_suffix"),
        "fish script must define __aterm_id_suffix helper"
    );
    assert!(
        script.contains("printf ';id=%s'"),
        "fish id suffix must emit ';id=<hex>' via printf"
    );
}

#[test]
fn test_bash_mark_functions_invoke_id_suffix() {
    let script = scripts::BASH;
    // Every 133 A/B/C/D emission MUST include the id suffix. The suffix is
    // precomputed at source time into $__aterm_id_suffix_str (the nonce is
    // immutable after capture), so the emitters expand a parameter instead of
    // forking `$(__aterm_id_suffix)` five times per command cycle. The bytes
    // on the wire are unchanged; only the spelling of the substring moved.
    for expected in [
        r#""133;A${__aterm_id_suffix_str}""#,
        r#""133;B${__aterm_id_suffix_str}""#,
        r#""133;C${__aterm_id_suffix_str}""#,
        r#""133;D;${1}${__aterm_id_suffix_str}""#,
    ] {
        assert!(
            script.contains(expected),
            "bash script must emit OSC 133 with id suffix; \
             missing exact substring {expected:?}"
        );
    }
    // OSC 633;E must also carry the nonce.
    assert!(
        script.contains(r#""633;E;$(__aterm_encode_cmd "$BASH_COMMAND")${__aterm_id_suffix_str}""#),
        "bash script must emit OSC 633;E with id suffix"
    );
    // ...and the precomputed suffix must actually be derived from the captured
    // shell-local nonce, or the emissions above would carry an empty tail.
    assert!(
        script.contains(r#"__aterm_id_suffix_str=";id=${__aterm_shell_nonce}""#),
        "bash script must precompute the id suffix from the captured nonce"
    );
}

#[test]
fn test_zsh_mark_functions_invoke_id_suffix() {
    let script = scripts::ZSH;
    // As in bash: the suffix is precomputed once at source time into
    // $__aterm_id_suffix_str so the five per-command-cycle markers expand a
    // parameter rather than forking `$(__aterm_id_suffix)`. Same wire bytes.
    for expected in [
        r#""133;A${__aterm_id_suffix_str}""#,
        r#""133;B${__aterm_id_suffix_str}""#,
        r#""133;C${__aterm_id_suffix_str}""#,
        r#""133;D;$1${__aterm_id_suffix_str}""#,
    ] {
        assert!(
            script.contains(expected),
            "zsh script must emit OSC 133 with id suffix; \
             missing exact substring {expected:?}"
        );
    }
    assert!(
        script.contains(r#""633;E;$(__aterm_encode_cmd "$1")${__aterm_id_suffix_str}""#),
        "zsh script must emit OSC 633;E with id suffix"
    );
    assert!(
        script.contains(r#"__aterm_id_suffix_str=";id=${__aterm_shell_nonce}""#),
        "zsh script must precompute the id suffix from the captured nonce"
    );
}

#[test]
fn test_fish_mark_functions_invoke_id_suffix() {
    let script = scripts::FISH;
    // Fish has no $() — it uses outer parens for command substitution.
    for expected in [
        r#""133;A"(__aterm_id_suffix)"#,
        r#""133;B"(__aterm_id_suffix)"#,
        r#""133;C"(__aterm_id_suffix)"#,
        r#""133;D;$argv[1]"(__aterm_id_suffix)"#,
    ] {
        assert!(
            script.contains(expected),
            "fish script must emit OSC 133 with id suffix; \
             missing exact substring {expected:?}"
        );
    }
    assert!(
        script.contains(r#""633;E;"(__aterm_encode_cmd "$argv")(__aterm_id_suffix)"#),
        "fish script must emit OSC 633;E with id suffix"
    );
}

#[test]
fn test_bash_ps1_mark_b_includes_nonce_when_set() {
    let script = scripts::BASH;
    // #8015: both the default-mode embed and the custom __aterm_set_prompt
    // builder must gate the id=<hex> tail on the captured shell-local
    // `$__aterm_shell_nonce` (not the env var). The env var is unset at
    // source time to stop the 64-hex secret from leaking into subprocesses.
    assert!(
        script.contains(r#"[[ -n "$__aterm_shell_nonce" ]] && __aterm_b_suffix=";id=${__aterm_shell_nonce}""#),
        "bash default-mode PS1 must append ;id= from the captured shell-local"
    );
    assert!(
        script.contains(r#"[[ -n "$__aterm_shell_nonce" ]] && mark_b_id=";id=${__aterm_shell_nonce}""#),
        "bash custom-prompt builder must append ;id= from the captured shell-local"
    );
}

#[cfg(unix)]
fn run_id_suffix_via_shell(
    shell: &str,
    args: &[&str],
    cleanup: &str,
    script_name: &str,
    nonce: Option<&str>,
) -> String {
    let script = format!(
        "{}/src/scripts/{script_name}",
        env!("CARGO_MANIFEST_DIR")
    );
    // Guard definedness before reading the suffix: an UNDEFINED __aterm_id_suffix
    // also yields empty stdout inside $(...) with a zero outer exit, so the
    // nonce-unset case ("" expected) could pass even if the script never loaded.
    // `command -v` (POSIX; works in bash and zsh) forces a hard failure instead,
    // so empty-because-defined is distinguished from empty-because-undefined.
    let command = format!(
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {cleanup}; \
         command -v __aterm_id_suffix >/dev/null || exit 97; \
         printf '%s' \"$(__aterm_id_suffix)\""
    );
    let mut cmd = shell_command(shell);
    cmd.args(args)
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", script);
    match nonce {
        Some(n) => {
            cmd.env("ATERM_SHELL_NONCE", n);
        }
        None => {
            cmd.env_remove("ATERM_SHELL_NONCE");
        }
    }
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn {shell} for id-suffix test: {error}"));
    assert!(
        output.status.success(),
        "{shell} id-suffix invocation should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("id-suffix output should be UTF-8")
}

#[cfg(unix)]
fn fish_shell() -> Option<&'static str> {
    ["/opt/homebrew/bin/fish", "/usr/local/bin/fish", "/usr/bin/fish"]
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).exists())
}

#[cfg(unix)]
#[test]
fn test_bash_id_suffix_emits_hex_when_nonce_set() {
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let actual = run_id_suffix_via_shell(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
        "aterm_shell_integration.bash",
        Some(nonce),
    );
    assert_eq!(
        actual,
        format!(";id={nonce}"),
        "bash must emit ';id=<hex>' when ATERM_SHELL_NONCE is set"
    );
}

#[cfg(unix)]
#[test]
fn test_bash_id_suffix_empty_when_nonce_unset() {
    let actual = run_id_suffix_via_shell(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
        "aterm_shell_integration.bash",
        None,
    );
    assert_eq!(
        actual, "",
        "bash must emit empty string when ATERM_SHELL_NONCE is unset \
         (pre-nonce host compatibility)"
    );
}

#[cfg(unix)]
#[test]
fn test_zsh_id_suffix_emits_hex_when_nonce_set() {
    let Some(zsh) = zsh_shell() else {
        return;
    };
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let actual = run_id_suffix_via_shell(
        zsh,
        &["-f", "-i"],
        "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true",
        "aterm_shell_integration.zsh",
        Some(nonce),
    );
    assert_eq!(
        actual,
        format!(";id={nonce}"),
        "zsh must emit ';id=<hex>' when ATERM_SHELL_NONCE is set"
    );
}

#[cfg(unix)]
#[test]
fn test_zsh_id_suffix_empty_when_nonce_unset() {
    let Some(zsh) = zsh_shell() else {
        return;
    };
    let actual = run_id_suffix_via_shell(
        zsh,
        &["-f", "-i"],
        "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true",
        "aterm_shell_integration.zsh",
        None,
    );
    assert_eq!(
        actual, "",
        "zsh must emit empty string when ATERM_SHELL_NONCE is unset"
    );
}

#[cfg(unix)]
#[test]
fn test_fish_id_suffix_emits_hex_when_nonce_set() {
    let Some(fish) = fish_shell() else {
        // fish is not universally installed; skip gracefully instead of failing CI.
        eprintln!("fish not installed; skipping test_fish_id_suffix_emits_hex_when_nonce_set");
        return;
    };
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.fish",
        env!("CARGO_MANIFEST_DIR")
    );
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let command = "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; __aterm_id_suffix".to_string();
    let output = shell_command(fish)
        .arg("-i")
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", &script)
        .env("ATERM_SHELL_NONCE", nonce)
        .output()
        .unwrap_or_else(|error| panic!("spawn fish for id-suffix test: {error}"));
    assert!(
        output.status.success(),
        "fish id-suffix invocation should succeed; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(";id={nonce}"),
        "fish must emit ';id=<hex>' when ATERM_SHELL_NONCE is set"
    );
}

// ─── #8015: ATERM_SHELL_NONCE must NOT leak into child processes ─────────
//
// Round-3 adversarial audit finding P1-R3-04: the 64-hex capability nonce
// was being exported into the spawned shell's environment but the shell
// integration scripts never `unset`ed it. Every child process (env,
// printenv, ssh SendEnv, docker, tmux children, cron jobs, Python
// subprocess, ...) inherited the secret that would be used to bypass the
// #7960 nonce-enforcement defense. The fix: capture the env var into a
// shell-local at source time, then immediately unset it so subprocesses
// never see it.

#[test]
fn test_bash_script_unsets_shell_nonce_env_var() {
    let script = scripts::BASH;
    assert!(
        script.contains("unset ATERM_SHELL_NONCE"),
        "bash script must `unset ATERM_SHELL_NONCE` after capturing to a \
         shell-local (#8015) so the nonce is not inherited by subprocesses"
    );
    assert!(
        script.contains(r#"__aterm_shell_nonce="${ATERM_SHELL_NONCE:-}""#),
        "bash script must capture ATERM_SHELL_NONCE into __aterm_shell_nonce \
         at source time (#8015)"
    );
}

#[test]
fn test_zsh_script_unsets_shell_nonce_env_var() {
    let script = scripts::ZSH;
    assert!(
        script.contains("unset ATERM_SHELL_NONCE"),
        "zsh script must `unset ATERM_SHELL_NONCE` after capturing to a \
         shell-local (#8015) so the nonce is not inherited by subprocesses"
    );
    assert!(
        script.contains(r#"typeset -g __aterm_shell_nonce="${ATERM_SHELL_NONCE:-}""#),
        "zsh script must capture ATERM_SHELL_NONCE into __aterm_shell_nonce \
         at source time using `typeset -g` (#8015)"
    );
}

#[test]
fn test_fish_script_unsets_shell_nonce_env_var() {
    let script = scripts::FISH;
    assert!(
        script.contains("set -e ATERM_SHELL_NONCE"),
        "fish script must `set -e ATERM_SHELL_NONCE` after capturing to a \
         shell-global (#8015) so the nonce is not inherited by subprocesses"
    );
    assert!(
        script.contains(r#"set -g __aterm_shell_nonce "$ATERM_SHELL_NONCE""#),
        "fish script must capture ATERM_SHELL_NONCE into __aterm_shell_nonce \
         at source time using `set -g` (#8015)"
    );
}

#[cfg(unix)]
fn run_env_check_after_source(
    shell: &str,
    args: &[&str],
    cleanup: &str,
    script_name: &str,
    nonce: &str,
) -> (String, String) {
    let script = format!(
        "{}/src/scripts/{script_name}",
        env!("CARGO_MANIFEST_DIR")
    );
    // After sourcing, query whether ATERM_SHELL_NONCE still exists in the
    // environment (it MUST NOT — #8015) and whether __aterm_id_suffix still
    // produces the expected output (it MUST — the shell captured the env
    // var into a shell-local before unsetting it).
    let command = format!(
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {cleanup}; \
         printf 'env=%s|suffix=%s' \"${{ATERM_SHELL_NONCE-UNSET}}\" \"$(__aterm_id_suffix)\""
    );
    let output = shell_command(shell)
        .args(args)
        .arg("-c")
        .arg(&command)
        .env("ATERM_TEST_SCRIPT", script)
        .env("ATERM_SHELL_NONCE", nonce)
        .output()
        .unwrap_or_else(|error| panic!("spawn {shell} for env-leak test: {error}"));
    assert!(
        output.status.success(),
        "{shell} env-leak invocation should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("env-leak output should be UTF-8");
    let parts: Vec<&str> = stdout.splitn(2, '|').collect();
    assert_eq!(parts.len(), 2, "env-leak output must have env=/suffix= pair: {stdout:?}");
    let env = parts[0]
        .strip_prefix("env=")
        .expect("env-leak stdout must start with env=")
        .to_string();
    let suffix = parts[1]
        .strip_prefix("suffix=")
        .expect("env-leak stdout must contain suffix=")
        .to_string();
    (env, suffix)
}

#[cfg(unix)]
#[test]
fn test_bash_unsets_shell_nonce_env_after_source() {
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let (env, suffix) = run_env_check_after_source(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
        "aterm_shell_integration.bash",
        nonce,
    );
    assert_eq!(
        env, "UNSET",
        "bash must `unset ATERM_SHELL_NONCE` after sourcing (#8015 — \
         otherwise every subprocess inherits the 64-hex secret)"
    );
    assert_eq!(
        suffix,
        format!(";id={nonce}"),
        "bash must still emit ';id=<hex>' from the captured shell-local \
         after the env var is unset"
    );
}

#[cfg(unix)]
#[test]
fn test_zsh_unsets_shell_nonce_env_after_source() {
    let Some(zsh) = zsh_shell() else {
        return;
    };
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let (env, suffix) = run_env_check_after_source(
        zsh,
        &["-f", "-i"],
        "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true",
        "aterm_shell_integration.zsh",
        nonce,
    );
    assert_eq!(
        env, "UNSET",
        "zsh must `unset ATERM_SHELL_NONCE` after sourcing (#8015 — \
         otherwise every subprocess inherits the 64-hex secret)"
    );
    assert_eq!(
        suffix,
        format!(";id={nonce}"),
        "zsh must still emit ';id=<hex>' from the captured shell-local \
         after the env var is unset"
    );
}

#[cfg(unix)]
#[test]
fn test_fish_unsets_shell_nonce_env_after_source() {
    let Some(fish) = fish_shell() else {
        eprintln!("fish not installed; skipping test_fish_unsets_shell_nonce_env_after_source");
        return;
    };
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.fish",
        env!("CARGO_MANIFEST_DIR")
    );
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    // After sourcing, query (1) whether ATERM_SHELL_NONCE is still set in
    // the environment and (2) that __aterm_id_suffix still prints ';id=<hex>'
    // from the captured shell-global. `set -qx` checks the exported env;
    // `set -q` alone would match the shell-global too.
    let command = "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
                   if set -qx ATERM_SHELL_NONCE; \
                       printf 'env=SET|suffix=%s' (__aterm_id_suffix); \
                   else; \
                       printf 'env=UNSET|suffix=%s' (__aterm_id_suffix); \
                   end";
    let output = shell_command(fish)
        .arg("-i")
        .arg("-c")
        .arg(command)
        .env("ATERM_TEST_SCRIPT", &script)
        .env("ATERM_SHELL_NONCE", nonce)
        .output()
        .unwrap_or_else(|error| panic!("spawn fish for env-leak test: {error}"));
    assert!(
        output.status.success(),
        "fish env-leak invocation should succeed; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        stdout.starts_with("env=UNSET"),
        "fish must `set -e ATERM_SHELL_NONCE` after sourcing (#8015); got: {stdout:?}"
    );
    assert!(
        stdout.ends_with(&format!("suffix=;id={nonce}")),
        "fish must still emit ';id=<hex>' from the captured shell-global \
         after the env var is unset; got: {stdout:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_bash_fallback_unnonced_when_env_missing() {
    // #8015 fallback: if ATERM_SHELL_NONCE is unset at source time,
    // __aterm_id_suffix must emit the empty string (NOT a literal
    // ";id="). Hosts with require_shell_integration_nonce=true will drop
    // those unnonced emissions; hosts with the enforcement off (current
    // default per #7960) will accept them — correct pre-nonce behavior.
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    // Guard definedness: empty "suffix=[]" must come from a defined function
    // returning nothing, NOT from an undefined __aterm_id_suffix (which would
    // also print "suffix=[]"). `command -v` forces a hard exit if it never loaded.
    let command = "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
                   __aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=; \
                   command -v __aterm_id_suffix >/dev/null || exit 97; \
                   printf 'suffix=[%s]' \"$(__aterm_id_suffix)\"";
    let output = shell_command(bash_shell())
        .args(["--noprofile", "--norc", "-i"])
        .arg("-c")
        .arg(command)
        .env("ATERM_TEST_SCRIPT", &script)
        .env_remove("ATERM_SHELL_NONCE")
        .output()
        .expect("spawn bash for fallback test");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert_eq!(
        stdout, "suffix=[]",
        "bash must fall through to empty suffix when nonce is unset \
         — not emit a literal `id=` that would fail enforcement"
    );
}

// ─── PowerShell / pwsh integration (Windows' default shells) ─────────────
//
// Static shape tests run everywhere; functional tests spawn powershell.exe
// and are Windows-only (powershell.exe ships with every supported Windows).

#[test]
fn test_detect_powershell() {
    assert_eq!(ShellType::detect("pwsh"), ShellType::PowerShell);
    assert_eq!(ShellType::detect("pwsh.exe"), ShellType::PowerShell);
    assert_eq!(ShellType::detect("powershell"), ShellType::PowerShell);
    assert_eq!(ShellType::detect("powershell.exe"), ShellType::PowerShell);
    assert_eq!(ShellType::detect("PowerShell.EXE"), ShellType::PowerShell);
    assert_eq!(ShellType::detect("/usr/bin/pwsh"), ShellType::PowerShell);
    #[cfg(windows)]
    assert_eq!(
        ShellType::detect(r"C:\Program Files\PowerShell\7\pwsh.exe"),
        ShellType::PowerShell
    );
}

#[test]
fn test_prepare_powershell_dot_sources_script() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");

    let result = prepare_into(ShellType::PowerShell, &base).unwrap().unwrap();
    let keys: Vec<&str> = result.env_add.iter().map(|(k, _)| k.as_str()).collect();
    assert!(keys.contains(&"ATERM_SHELL_INTEGRATION_DIR"));

    let argv = result
        .argv_override
        .expect("powershell injection needs an argv override");
    // `-ExecutionPolicy Bypass` must precede the interactive flags so the dot-source
    // survives a stock `Restricted` machine (else it throws UnauthorizedAccess).
    assert_eq!(argv[1], "-ExecutionPolicy");
    assert_eq!(argv[2], "Bypass");
    assert_eq!(argv[3], "-NoExit");
    assert_eq!(argv[4], "-Command");
    assert!(
        argv[5].contains("aterm_shell_integration.ps1"),
        "-Command must dot-source the ps1: {argv:?}"
    );
    assert!(
        argv[5].contains("$env:ATERM_SHELL_INTEGRATION_DIR"),
        "the ps1 path must resolve from the env var (no command-line path quoting): {argv:?}"
    );
    assert!(base.join("aterm_shell_integration.ps1").exists());
}

#[test]
fn test_powershell_script_is_ascii() {
    // Windows PowerShell 5.1 decodes BOM-less .ps1 source as ANSI, and we
    // write the script without a BOM — so it must stay pure ASCII.
    assert!(
        scripts::POWERSHELL.is_ascii(),
        "aterm_shell_integration.ps1 must stay ASCII-only (written without a BOM)"
    );
}

#[test]
fn test_powershell_script_references_shell_nonce_env() {
    assert!(
        scripts::POWERSHELL.contains("ATERM_SHELL_NONCE"),
        "PowerShell script must reference ATERM_SHELL_NONCE to honor \
         the #7960/#7987 capability-nonce defense"
    );
}

#[test]
fn test_powershell_script_unsets_shell_nonce_env_var() {
    let script = scripts::POWERSHELL;
    assert!(
        script.contains("Remove-Item Env:ATERM_SHELL_NONCE"),
        "PowerShell script must remove ATERM_SHELL_NONCE from the \
         environment after capture (#8015)"
    );
    assert!(
        script.contains(r"$Global:__aterm_shell_nonce = if ($env:ATERM_SHELL_NONCE)"),
        "PowerShell script must capture ATERM_SHELL_NONCE into a \
         (non-inherited) PowerShell variable at source time (#8015)"
    );
}

#[test]
fn test_powershell_mark_emissions_include_id_suffix() {
    let script = scripts::POWERSHELL;
    for expected in [
        r"]133;A$__aterm_suffix",
        r"]133;B$__aterm_suffix",
        r"]133;C$__aterm_suffix",
        r"]133;D;$__aterm_code$__aterm_suffix",
        r"]633;E;$(__aterm_encode_cmd $__aterm_line)$__aterm_suffix",
    ] {
        assert!(
            script.contains(expected),
            "PowerShell script must emit OSC 133/633 with the id suffix; \
             missing exact substring {expected:?}"
        );
    }
}

#[test]
fn test_powershell_script_reports_cwd_and_preserves_prompt() {
    let script = scripts::POWERSHELL;
    assert!(
        script.contains("]7;file://"),
        "PowerShell script must report the cwd via OSC 7"
    );
    assert!(
        script.contains(r"$Global:__aterm_original_prompt = $function:Prompt"),
        "PowerShell script must capture and wrap the user's prompt, not replace it"
    );
    assert!(
        script.contains("ATERM_DISABLE_PROMPT_TITLES"),
        "PowerShell OSC 0 titles must honor ATERM_DISABLE_PROMPT_TITLES"
    );
}

#[cfg(windows)]
fn run_powershell_snippet(snippet: &str, nonce: Option<&str>) -> String {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    // The snippet must avoid double quotes: powershell.exe's command-line
    // quote handling would otherwise interact with Rust's arg quoting.
    let command = format!(". $env:ATERM_TEST_SCRIPT; {snippet}");
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
    ])
    .arg(&command)
    .env("ATERM_TEST_SCRIPT", &script);
    // Hermetic spawn: a developer running the tests inside aterm has these
    // exported, which would trip the already-loaded guard or alter titles.
    for var in [
        "ATERM_SHELL_INTEGRATION_INSTALLED",
        "ATERM_DISABLE_PROMPT_TITLES",
    ] {
        cmd.env_remove(var);
    }
    match nonce {
        Some(n) => {
            cmd.env("ATERM_SHELL_NONCE", n);
        }
        None => {
            cmd.env_remove("ATERM_SHELL_NONCE");
        }
    }
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn powershell.exe for integration test: {error}"));
    assert!(
        output.status.success(),
        "powershell snippet {snippet:?} should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(windows)]
#[test]
fn test_powershell_id_suffix_emits_hex_when_nonce_set() {
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let actual = run_powershell_snippet("[Console]::Write((__aterm_id_suffix))", Some(nonce));
    assert_eq!(
        actual,
        format!(";id={nonce}"),
        "PowerShell must emit ';id=<hex>' when ATERM_SHELL_NONCE is set"
    );
}

#[cfg(windows)]
#[test]
fn test_powershell_id_suffix_empty_when_nonce_unset() {
    let actual = run_powershell_snippet(
        "[Console]::Write('suffix=[' + (__aterm_id_suffix) + ']')",
        None,
    );
    assert_eq!(
        actual, "suffix=[]",
        "PowerShell must emit empty suffix when ATERM_SHELL_NONCE is unset \
         (pre-nonce host compatibility)"
    );
}

#[cfg(windows)]
#[test]
fn test_powershell_unsets_shell_nonce_env_after_source() {
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let actual = run_powershell_snippet(
        "$e = if ($env:ATERM_SHELL_NONCE) { 'SET' } else { 'UNSET' }; \
         [Console]::Write('env=' + $e + '|suffix=' + (__aterm_id_suffix))",
        Some(nonce),
    );
    assert_eq!(
        actual,
        format!("env=UNSET|suffix=;id={nonce}"),
        "PowerShell must drop ATERM_SHELL_NONCE from the env (#8015) while \
         still emitting ';id=<hex>' from the captured variable"
    );
}

#[cfg(windows)]
#[test]
fn test_powershell_prompt_emits_osc7_and_prompt_marks() {
    let out = run_powershell_snippet("[Console]::Write((prompt))", None);
    let osc7 = out
        .find("\u{1b}]7;file://")
        .expect("prompt must emit OSC 7 cwd report");
    let mark_a = out
        .find("\u{1b}]133;A\u{7}")
        .expect("prompt must emit OSC 133;A");
    let mark_b = out
        .find("\u{1b}]133;B\u{7}")
        .expect("prompt must emit OSC 133;B");
    assert!(
        osc7 < mark_a && mark_a < mark_b,
        "protocol order must be OSC 7, then 133;A, then prompt text, then 133;B; got: {out:?}"
    );
    assert!(
        !out.contains("]133;D"),
        "first prompt (no command ran) must not emit OSC 133;D; got: {out:?}"
    );
}

#[cfg(windows)]
#[test]
fn test_powershell_osc7_path_percent_encodes_windows_paths() {
    let actual = run_powershell_snippet(
        r"[Console]::Write((__aterm_osc7_path 'C:\tmp\r sum @[test]!'))",
        None,
    );
    assert_eq!(
        actual, "/C:/tmp/r%20sum%20%40%5Btest%5D%21",
        "OSC 7 path must normalize backslashes, gain a leading '/', and \
         percent-encode non-unreserved bytes"
    );
}

#[cfg(windows)]
#[test]
fn test_cache_dir_windows_uses_localappdata() {
    let dir = cache_dir();
    let text = dir.to_string_lossy().into_owned();
    assert!(
        !text.starts_with("/tmp") && !text.starts_with("\\tmp"),
        "Windows cache dir must never resolve to a drive-root /tmp; got: {text}"
    );
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        assert!(
            dir.starts_with(&local),
            "cache dir must live under %LOCALAPPDATA%; got: {text}"
        );
        assert!(
            text.ends_with("shell-integration"),
            "cache dir must end with aterm\\shell-integration; got: {text}"
        );
    }
}

#[test]
#[cfg(feature = "local-pty")]
fn test_containment_modes_require_tmp_cache() {
    use aterm_containment::{ContainmentMode, ContainmentPolicy, FsCapability};

    for mode in [ContainmentMode::Containment, ContainmentMode::Safety] {
        let caps = ContainmentPolicy::capabilities(mode);
        assert!(
            caps.fs <= FsCapability::ProjectReadWrite,
            "{mode:?} should require /tmp path for shell integration"
        );
    }

    for mode in [ContainmentMode::User, ContainmentMode::Master] {
        let caps = ContainmentPolicy::capabilities(mode);
        assert!(
            caps.fs > FsCapability::ProjectReadWrite,
            "{mode:?} should allow ~/.cache path for shell integration"
        );
    }
}

// ---------------------------------------------------------------------------
// WSL + cmd.exe: the two first-class Windows shells that used to detect as
// `Unknown` and therefore got no injection at all.
// ---------------------------------------------------------------------------

#[test]
fn test_detect_wsl_and_cmd() {
    // The bare aliases the PTY seam already resolves...
    assert_eq!(ShellType::detect("wsl"), ShellType::Wsl);
    assert_eq!(ShellType::detect("cmd"), ShellType::Cmd);
    // ...and the resolved program paths those aliases turn into.
    assert_eq!(
        ShellType::detect("C:\\Windows\\System32\\wsl.exe"),
        ShellType::Wsl
    );
    assert_eq!(
        ShellType::detect("C:\\Windows\\System32\\CMD.EXE"),
        ShellType::Cmd
    );
    // Neighbours must not be swept in: nu still has no injection.
    assert_eq!(ShellType::detect("nu.exe"), ShellType::Unknown);
}

/// `WSLENV` is the ONLY channel into the distro, so getting it wrong is silent:
/// without the `/p` flag the integration dir arrives as an untranslated `C:\…`
/// that no Linux shell can read.
#[test]
fn test_prepare_wsl_env_carries_path_translated_wslenv() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let injection = prepare_into(ShellType::Wsl, &base).unwrap().unwrap();

    let env: std::collections::HashMap<&str, &str> = injection
        .env_add
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        env.get("ATERM_SHELL_INTEGRATION_DIR").copied(),
        Some(base.to_string_lossy().as_ref())
    );
    let wslenv = env.get("WSLENV").expect("WSLENV must be injected");
    let entries: Vec<&str> = wslenv.split(':').collect();
    assert!(
        entries.contains(&"ATERM_SHELL_INTEGRATION_DIR/p"),
        "the /p flag is what path-translates the dir into the distro: {wslenv}"
    );
    assert!(
        entries.contains(&"ATERM_SHELL_NONCE"),
        "the capability nonce must cross too or every mark is dropped: {wslenv}"
    );
    assert!(
        entries.contains(&WSL_CWD_VAR),
        "the cwd hand-off must cross: {wslenv}"
    );
}

/// The launcher must run through `--exec` (argv verbatim) and must reach the
/// EXISTING bash wrapper rcfile rather than re-implementing the injection.
#[test]
fn test_prepare_wsl_argv_execs_the_bash_rcfile() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let injection = prepare_into(ShellType::Wsl, &base).unwrap().unwrap();

    let argv = injection.argv_override.expect("wsl needs an argv override");
    assert_eq!(argv[0], "wsl", "argv[0] is the display token");
    assert_eq!(
        &argv[1..4],
        ["--exec", "/bin/sh", "-c"],
        "`--exec` bypasses the distro login shell so argv arrives byte-for-byte; \
         plain `wsl -- …` re-quotes every argument into a `bash -c` string"
    );
    let script = &argv[4];
    assert!(
        script.contains("$ATERM_SHELL_INTEGRATION_DIR/bash/rcfile"),
        "must source the shipped bash wrapper rcfile: {script}"
    );
    assert!(
        script.contains("--rcfile"),
        "bash's injection mechanism is --rcfile: {script}"
    );
    // The user's own login shell wins unless it IS bash: forcing bash on a zsh
    // WSL user to gain marks would be a regression, not a feature.
    assert!(
        script.contains(r#"case "$__aterm_sh" in */bash|bash)"#),
        "must gate the bash hijack on the login shell actually being bash: {script}"
    );
    assert!(
        script.contains(r#"exec "$__aterm_sh" -l"#),
        "non-bash login shells must still start, uninstrumented: {script}"
    );
    assert!(
        script.contains("unset ATERM_WSL_CWD"),
        "the cwd hand-off must not leak into shells nested inside this one: {script}"
    );
    // The bash rcfile the launcher points at has to exist on disk.
    assert!(base.join("bash").join("rcfile").exists());
}

#[test]
fn test_merge_wslenv_is_append_safe_and_idempotent() {
    // A user's own WSLENV survives, ours goes first.
    let merged = merge_wslenv("MY_TOOL/p:OTHER");
    assert!(merged.starts_with("ATERM_SHELL_INTEGRATION_DIR/p:"));
    assert!(merged.split(':').any(|e| e == "MY_TOOL/p"));
    assert!(merged.split(':').any(|e| e == "OTHER"));

    // An empty inherited value must not produce an empty trailing entry.
    let empty = merge_wslenv("");
    assert!(
        !empty.ends_with(':') && !empty.contains("::"),
        "{empty}"
    );

    // Nesting (aterm inside aterm inside …) must not grow duplicates, and must
    // not keep a STALE flag spelling for one of our own variables.
    assert_eq!(merge_wslenv(&empty), empty, "merge must be idempotent");
    let stale = merge_wslenv("ATERM_SHELL_INTEGRATION_DIR:ATERM_SHELL_NONCE/p");
    assert_eq!(
        stale.matches("ATERM_SHELL_INTEGRATION_DIR").count(),
        1,
        "{stale}"
    );
    assert!(!stale.contains("ATERM_SHELL_NONCE/p"), "{stale}");
}

#[test]
fn test_wsl_cwd_env_only_crosses_a_posix_path() {
    // The case that matters: a WSL tab reports `/home/you/proj` over OSC 7,
    // Windows cannot use it as a working directory, so it rides the env.
    assert_eq!(
        wsl_cwd_env(ShellType::Wsl, Some("/home/you/proj")),
        Some((WSL_CWD_VAR.to_string(), "/home/you/proj".to_string()))
    );
    // A Windows path is wsl.exe's own business (it inherits + translates it).
    assert_eq!(wsl_cwd_env(ShellType::Wsl, Some("C:\\Users\\x")), None);
    // `//host/share` is the host-preserving UNC form, not a Linux path.
    assert_eq!(wsl_cwd_env(ShellType::Wsl, Some("//server/share")), None);
    assert_eq!(wsl_cwd_env(ShellType::Wsl, None), None);
    // Never for another shell — bash on Windows gets a Windows cwd.
    assert_eq!(wsl_cwd_env(ShellType::Bash, Some("/home/you/proj")), None);
    assert_eq!(wsl_cwd_env(ShellType::Cmd, Some("/home/you/proj")), None);
}

/// cmd renders `%PROMPT%` on every input line and understands `$E`/`$P`; that
/// is the whole integration surface it has.
#[test]
fn test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let mut injection = prepare_into(ShellType::Cmd, &base).unwrap().unwrap();
    assert!(
        injection.argv_override.is_none(),
        "cmd takes no argv override — the injection is entirely %PROMPT%"
    );

    let nonce = "ab".repeat(32);
    augment_with_nonce(&mut injection, &nonce);
    let prompt = injection
        .env_add
        .iter()
        .find(|(k, _)| k == "PROMPT")
        .map(|(_, v)| v.clone())
        .expect("cmd injection must set PROMPT");

    assert!(
        prompt.contains(&format!("$e]133;A;id={nonce}$e\\")),
        "prompt-start mark (what jump-to-prompt navigates by): {prompt}"
    );
    assert!(
        prompt.contains(&format!("$e]133;B;id={nonce}$e\\")),
        "prompt-end mark: {prompt}"
    );
    assert!(
        prompt.contains(&format!("$e]633;P;Cwd=$P;id={nonce}$e\\")),
        "cwd via $P — cmd cannot percent-encode a file:// URI: {prompt}"
    );
    assert!(
        prompt.contains("$P$G"),
        "the user-visible prompt must still be cmd's own: {prompt}"
    );
    assert!(
        !prompt.contains(NONCE_PLACEHOLDER),
        "every placeholder must be substituted or the marks are dropped: {prompt}"
    );
    // C/D are deliberately absent: cmd has no hook for "a command started
    // executing", and the engine's phase machine needs A→B→C→D in order.
    assert!(!prompt.contains("]133;C"), "{prompt}");
    assert!(!prompt.contains("]133;D"), "{prompt}");
}

#[test]
fn test_augment_with_nonce_substitutes_the_placeholder() {
    let mut injection = InjectionEnv {
        env_add: vec![
            ("PLAIN".to_string(), "untouched".to_string()),
            (
                "MARKED".to_string(),
                format!("a{NONCE_PLACEHOLDER}b{NONCE_PLACEHOLDER}"),
            ),
        ],
        argv_override: None,
    };
    let nonce = "cd".repeat(32);
    augment_with_nonce(&mut injection, &nonce);
    assert_eq!(injection.env_add[0].1, "untouched");
    assert_eq!(injection.env_add[1].1, format!("a{nonce}b{nonce}"));
    assert_eq!(
        injection.env_add[2],
        ("ATERM_SHELL_NONCE".to_string(), nonce)
    );
}

/// A POSIX shell reads a script line by line and keeps a trailing CR as part of
/// the last token, so one `\r` per line shreds the whole file
/// (`$'\r': command not found`). The scripts are `include_str!`d at build time,
/// so a Windows build machine with Git's default `core.autocrlf=true` used to
/// ship exactly that — measured: 446 CRs in the installed
/// `aterm_shell_integration.bash`, and sourcing it in WSL failed at line 16.
#[test]
fn test_posix_scripts_are_written_lf_only() {
    assert_eq!(lf_only("a\r\nb\r\n"), "a\nb\n");
    assert_eq!(lf_only("a\nb\n"), "a\nb\n");
    assert!(
        matches!(lf_only("no cr here"), std::borrow::Cow::Borrowed(_)),
        "the Unix path must not allocate"
    );

    // ...and the writer is actually wired to it.
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    prepare_into(ShellType::Bash, &base).unwrap().unwrap();
    for name in [
        "aterm_shell_integration.bash",
        "aterm_shell_integration.zsh",
        "aterm_shell_integration.fish",
    ] {
        let bytes = std::fs::read(base.join(name)).unwrap();
        assert!(
            !bytes.contains(&b'\r'),
            "{name} reached disk with CR line endings — every POSIX shell that \
             sources it will fail line by line"
        );
    }
}

/// Functional proof on the real thing: cmd.exe renders the injected `%PROMPT%`
/// as OSC 133 A/B around its prompt. cmd.exe ships with every Windows, so this
/// never skips on the target platform.
#[cfg(windows)]
#[test]
fn test_cmd_prompt_emits_real_osc_133_marks() {
    use std::io::Write as _;
    use std::process::Stdio;

    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let mut injection = prepare_into(ShellType::Cmd, &base).unwrap().unwrap();
    let nonce = "3f".repeat(32);
    augment_with_nonce(&mut injection, &nonce);

    let mut cmd = Command::new("cmd.exe");
    cmd.arg("/k");
    for (k, v) in &injection.env_add {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("cmd.exe must be spawnable on Windows");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"exit\r\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains(&format!("\x1b]133;A;id={nonce}\x1b\\")),
        "cmd must render a real ESC-framed prompt-start mark; got {text:?}"
    );
    assert!(
        text.contains(&format!("\x1b]133;B;id={nonce}\x1b\\")),
        "cmd must render a real ESC-framed prompt-end mark; got {text:?}"
    );
    assert!(
        text.contains("\x1b]633;P;Cwd="),
        "cmd must report its cwd; got {text:?}"
    );
    // `$P` must have expanded to a real directory, not stayed literal.
    assert!(!text.contains("Cwd=$P"), "got {text:?}");
}

/// Functional proof on the real thing: the WSL launcher, run through the
/// installed `wsl.exe`, lands OSC 133 marks with the session nonce. Skips
/// (loudly) when WSL is not installed — aterm must never DEPEND on it.
#[cfg(windows)]
#[test]
fn test_wsl_launcher_emits_real_osc_133_marks() {
    use std::io::Write as _;
    use std::process::Stdio;

    let usable = Command::new("wsl.exe")
        .args(["--exec", "/bin/true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !usable {
        eprintln!("SKIP: no working WSL distro on this host");
        return;
    }

    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    let mut injection = prepare_into(ShellType::Wsl, &base).unwrap().unwrap();
    let nonce = "5c".repeat(32);
    augment_with_nonce(&mut injection, &nonce);
    let argv = injection.argv_override.clone().unwrap();

    let mut cmd = Command::new("wsl.exe");
    cmd.args(&argv[1..]);
    for (k, v) in &injection.env_add {
        cmd.env(k, v);
    }
    // The cwd hand-off, exercised end to end.
    cmd.env(WSL_CWD_VAR, "/usr");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("wsl.exe must be spawnable once the probe above succeeded");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"pwd\nexit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);

    assert!(
        text.contains(&format!("\x1b]133;A;id={nonce}")),
        "the bash integration must load INSIDE the distro and mark the prompt; \
         got {text:?}"
    );
    assert!(
        text.contains("\x1b]7;file://"),
        "cwd tracking (OSC 7) must survive the WSL boundary; got {text:?}"
    );
    assert!(
        text.contains("/usr"),
        "ATERM_WSL_CWD must place the shell in the requested POSIX directory; \
         got {text:?}"
    );
}
