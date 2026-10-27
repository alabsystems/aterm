// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
#[cfg(unix)]
use std::fmt::Write as _;
#[cfg(any(unix, windows))]
use std::process::Command;

/// The live tests' shells, as [`aterm_spec::host::test_shell`] finds them.
#[cfg(unix)]
fn interned_test_shell(
    cell: &'static std::sync::OnceLock<Option<String>>,
    shell: &str,
) -> Option<&'static str> {
    cell.get_or_init(|| {
        aterm_spec::host::test_shell(shell).map(|path| path.to_string_lossy().into_owned())
    })
    .as_deref()
}

/// The pinned bash 5.3 (the `bash-5.3` row, which `verify provision` builds on a Mac), when
/// this host has one that answers `BASH_VERSINFO` 5.3 or later; else why not.
#[cfg(unix)]
fn bash_5_3() -> Result<&'static str, &'static str> {
    static BASH: std::sync::OnceLock<Result<String, String>> = std::sync::OnceLock::new();
    let found = BASH.get_or_init(|| {
        let row = aterm_spec::host::tool("bash-5.3");
        let bash = aterm_spec::host::test_shell(row.name)
            .map(|path| path.to_string_lossy().into_owned())
            .ok_or_else(|| row.refusal())?;
        let out = shell_command(&bash)
            .args(["-c", "echo $((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1]))"])
            .output()
            .map_err(|error| format!("{bash} -c: {error}"))?;
        let level = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        match level.parse::<u32>() {
            Ok(level) if level >= 503 => Ok(bash),
            _ => Err(format!(
                "{bash} is no bash 5.3 (BASH_VERSINFO {level:?}): {}",
                row.remedy()
            )),
        }
    });
    found.as_deref().map_err(String::as_str)
}

/// Absolute path to a `zsh` binary, or `None` where the host has none ([`interned_test_shell`]).
#[cfg(unix)]
fn zsh_shell() -> Option<&'static str> {
    static ZSH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    interned_test_shell(&ZSH, "zsh")
}

/// A shell [`Command`] in its hermetic environment, with the private home it points into.
#[cfg(unix)]
struct ShellCommand {
    cmd: Command,
    home: aterm_tempfile::TempDir,
}

#[cfg(unix)]
impl ShellCommand {
    /// The hermetic home every `HOME`/`XDG_*` of this command points into.
    fn home(&self) -> &std::path::Path {
        self.home.path()
    }

    /// The home itself, for a [`LiveShell`] that outlives this command to hold.
    fn into_home(self) -> aterm_tempfile::TempDir {
        self.home
    }
}

#[cfg(unix)]
impl std::ops::Deref for ShellCommand {
    type Target = Command;
    fn deref(&self) -> &Command {
        &self.cmd
    }
}

#[cfg(unix)]
impl std::ops::DerefMut for ShellCommand {
    fn deref_mut(&mut self) -> &mut Command {
        &mut self.cmd
    }
}

/// Mark fish's man-page completions made in the dirs `cmd` names: fish 4 reads
/// its mark under `XDG_CACHE_HOME`, fish 3 under `XDG_DATA_HOME`. A live fish
/// without its mark starts, at its first prompt, a detached python that parses
/// every man page and outlives the test
/// (`no_live_fish_starts_its_completion_generator`).
#[cfg(unix)]
fn mark_fish_completions_made(cmd: &Command) {
    for (key, dir) in cmd.get_envs() {
        if let Some(dir) = dir.filter(|_| key == "XDG_CACHE_HOME" || key == "XDG_DATA_HOME") {
            std::fs::create_dir_all(Path::new(dir).join("fish").join("generated_completions"))
                .expect("mark fish's completions made");
        }
    }
}

#[cfg(unix)]
fn detach_session(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: between fork and exec, `getsid`, `getpid`, and `setsid` alone,
    // all async-signal-safe: start a fresh session with no controlling
    // terminal unless already leading one, so an interactive shell run on pipes
    // (`bash 5.3 -i`, `zsh -i`) cannot open `/dev/tty` to write on the caller's
    // terminal or stop a background test process group with `SIGTTIN`/`SIGTTOU`.
    unsafe {
        cmd.pre_exec(|| {
            if libc::getsid(0) != libc::getpid() && libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// A shell command with a hermetic environment for integration tests.
#[cfg(unix)]
fn shell_command(shell: &str) -> ShellCommand {
    let mut cmd = Command::new(shell);
    detach_session(&mut cmd);
    for var in [
        "ATERM_SHELL_INTEGRATION_INSTALLED",
        "ATERM_PROMPT_STYLE",
        "ATERM_DISABLE_PROMPT_TITLES",
        "ATERM_PROMPT_HOST_COLOR",
        "ATERM_PROMPT_PATH_COLOR",
        "ATERM_PROMPT_GIT_COLOR",
        "ATERM_PROMPT_ERROR_COLOR",
        "ATERM_PROMPT_SEP_COLOR",
        "ATERM_SHELL_NONCE",
        "ATERM_REROUTE_DIR",
        "__ATERM_REROUTE_PASSTHROUGH",
        "ATPKG_AGENTS",
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        "ATERM_REKEY_PATH",
        "ATERM_INTEGRATION_POINTER",
        "VISUAL",
        "EDITOR",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "AGY_CLI_FAST_PROMPT_EDITOR",
    ] {
        cmd.env_remove(var);
    }
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-si-hermetic-home-")
        .tempdir()
        .expect("create hermetic HOME for shell integration tests");
    let home = dir.path();
    for sub in [".config/fish/conf.d", ".local/state"] {
        std::fs::create_dir_all(home.join(sub))
            .expect("create hermetic HOME for shell integration tests");
    }
    for (key, value) in aterm_spec::host::hermetic_env(home) {
        cmd.env(key, value);
    }
    for key in aterm_spec::host::SHELL_START_POINTERS {
        cmd.env_remove(key);
    }
    cmd.env("ZDOTDIR", home)
        .env("XDG_DATA_DIRS", hermetic_data(home));
    ShellCommand { cmd, home: dir }
}

#[cfg(unix)]
#[test]
fn a_lane_shell_inherits_no_start_pointer() {
    use std::ffi::OsStr;
    let cmd = shell_command("/bin/sh");
    let envs: Vec<(&OsStr, Option<&OsStr>)> = cmd.get_envs().collect();
    assert!(envs.contains(&(OsStr::new("BASH_ENV"), None)), "{envs:?}");
    assert!(
        envs.contains(&(OsStr::new("ZDOTDIR"), Some(cmd.home().as_os_str()))),
        "{envs:?}"
    );
}

/// The hermetic home's data dir (`XDG_DATA_HOME`), which `XDG_DATA_DIRS` is
/// narrowed to.
#[cfg(unix)]
fn hermetic_data(home: &std::path::Path) -> std::path::PathBuf {
    aterm_spec::host::hermetic_env(home)
        .into_iter()
        .find(|(key, _)| *key == "XDG_DATA_HOME")
        .map(|(_, dir)| dir)
        .expect("the hermetic env names XDG_DATA_HOME")
}

/// `(shell, args, script, quiet)`.
#[cfg(unix)]
type PosixLane = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
);

/// The POSIX lanes the script-level tests drive: every bash ([`ShellLanes::bashes`]) and zsh.
#[cfg(unix)]
fn posix_lanes(shells: &mut ShellLanes) -> Vec<PosixLane> {
    let mut lanes: Vec<PosixLane> = shells
        .bashes()
        .into_iter()
        .map(|bash| {
            (
                bash,
                &["--noprofile", "--norc", "-i"][..],
                "aterm_shell_integration.bash",
                "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
            )
        })
        .collect();
    if let Some(zsh) = shells.zsh() {
        lanes.push((
            zsh,
            &["-f", "-i"][..],
            "aterm_shell_integration.zsh",
            "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; \
             add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true; setopt ksh_arrays",
        ));
    }
    lanes
}

/// The shells a test drives: the bashes, and the optional zsh and fish. A shell this host
/// lacks is noted, and once the test has run every lane it has, it ends COULD NOT RUN
/// naming each missing one.
#[cfg(unix)]
#[derive(Default)]
struct ShellLanes {
    /// What each missing shell's refusal says.
    absent: Vec<String>,
}

#[cfg(unix)]
impl ShellLanes {
    /// Every bash the bash tests run on: the OS's (`$ATERM_TEST_BASH`, else the `bash` row:
    /// macOS's 3.2, a Linux host's own), then the pinned 5.3 ([`bash_5_3`]), for the
    /// `${ ...; }` reads and PS0 hook 3.2 lacks. A missing 5.3 is noted where the tree pins one
    /// for this host. A bash test gets its bash here and nowhere else, so each runs on both.
    fn bashes(&mut self) -> Vec<&'static str> {
        static OS: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
        let os = interned_test_shell(&OS, "bash").unwrap_or("bash");
        let mut bashes = vec![os];
        match bash_5_3() {
            Ok(pinned) if pinned != os => bashes.push(pinned),
            Ok(_) => {}
            Err(why) => {
                let host = aterm_uds::host::host_platform();
                let pinned_here = aterm_spec::host::tool("bash-5.3")
                    .pin
                    .is_some_and(|pin| pin.pinned_for(&host));
                if pinned_here {
                    self.missing(why.to_owned());
                }
            }
        }
        bashes
    }

    /// Each lane of `dialects` (`bash`, `zsh`, `fish`) on this host, as `(dialect, program)`:
    /// every bash ([`ShellLanes::bashes`]), zsh and fish where found.
    fn lanes(&mut self, dialects: &[&'static str]) -> Vec<(&'static str, &'static str)> {
        let mut lanes = Vec::new();
        for &dialect in dialects {
            match dialect {
                "bash" => lanes.extend(self.bashes().into_iter().map(|bash| ("bash", bash))),
                "zsh" => lanes.extend(self.zsh().map(|zsh| ("zsh", zsh))),
                "fish" => lanes.extend(self.fish().map(|fish| ("fish", fish))),
                other => panic!("no lane for {other}"),
            }
        }
        lanes
    }

    /// [`zsh_shell`], noting a missing one.
    fn zsh(&mut self) -> Option<&'static str> {
        self.note("zsh", zsh_shell())
    }

    /// [`fish_shell`], noting a missing one.
    fn fish(&mut self) -> Option<&'static str> {
        self.note("fish", fish_shell())
    }

    fn note(&mut self, name: &'static str, found: Option<&'static str>) -> Option<&'static str> {
        if found.is_none() {
            self.missing(aterm_spec::host::tool(name).refusal());
        }
        found
    }

    fn missing(&mut self, why: String) {
        if !self.absent.contains(&why) {
            self.absent.push(why);
        }
    }
}

#[cfg(unix)]
impl Drop for ShellLanes {
    fn drop(&mut self) {
        if !std::thread::panicking() && !self.absent.is_empty() {
            aterm_spec::host::could_not_run(format!(
                "that lane did not run: {}",
                self.absent.join("; ")
            ));
        }
    }
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

#[cfg(unix)]
#[test]
fn test_reroute_and_agents_dirs_lead_path_in_each_shell() {
    let base = aterm_tempfile::tempdir().expect("tempdir");
    let reroute = base.path().join("reroute");
    let agents = base.path().join("Application Support/pkg/agents");
    std::fs::create_dir_all(&reroute).expect("mk reroute dir");
    std::fs::create_dir_all(&agents).expect("mk agents dir");
    let reroute = reroute.to_str().expect("UTF-8 temp dir").to_owned();
    let agents = agents.to_str().expect("UTF-8 temp dir").to_owned();
    let (stale_reroute, stale_agents) = ("/nonexistent/aterm-reroute", "/nonexistent/aterm-agents");
    let mut shells = ShellLanes::default();
    let mut lanes: Vec<(&str, &[&str], &str, String, bool)> = posix_lanes(&mut shells)
        .into_iter()
        .map(|(shell, args, script, quiet)| {
            let print = format!("{quiet}; printf 'PATH=%s\\n' \"$PATH\"");
            (shell, args, script, print, true)
        })
        .collect();
    if let Some(fish) = shells.fish() {
        lanes.push((
            fish,
            &["-i"][..],
            "aterm_shell_integration.fish",
            "printf 'PATH=%s\\n' (string join : -- $PATH)".to_owned(),
            false,
        ));
    }
    for (shell, args, script_name, print, exact) in lanes {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let run = |rr: Option<&str>, ag: Option<&str>, path: &str, pre: &str| {
            let mut cmd = shell_command(shell);
            cmd.args(args)
                .arg("-c")
                .arg(format!(
                    "{pre}source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {print}"
                ))
                .env("ATERM_TEST_SCRIPT", &script)
                .env("PATH", path);
            for (key, value) in [("ATERM_REROUTE_DIR", rr), ("ATPKG_AGENTS", ag)] {
                if let Some(value) = value {
                    cmd.env(key, value);
                }
            }
            let out = cmd
                .output()
                .unwrap_or_else(|error| panic!("{shell}: spawn: {error}"));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("PATH="),
                "{shell}: no PATH line; stdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&out.stderr)
            );
            let value: String = stdout
                .rsplit("PATH=")
                .next()
                .unwrap_or("")
                .chars()
                .take_while(|c| !c.is_control())
                .collect();
            value
        };
        let both = (Some(reroute.as_str()), Some(agents.as_str()));
        let got = run(
            both.0,
            both.1,
            &format!("/usr/bin:{agents}:/bin:{reroute}:{agents}"),
            "",
        );
        assert!(
            got.starts_with(&format!("{reroute}:{agents}:")),
            "{shell}: reroute then agents must lead PATH: {got:?}"
        );
        for dir in [&reroute, &agents] {
            assert_eq!(
                got.matches(dir.as_str()).count(),
                1,
                "{shell}: once: {got:?}"
            );
        }
        if exact {
            assert_eq!(got, format!("{reroute}:{agents}:/usr/bin:/bin"), "{shell}");
        }
        assert_eq!(run(both.0, both.1, &got, ""), got, "{shell}: idempotent");
        let missing = run(Some(stale_reroute), Some(stale_agents), "/usr/bin:/bin", "");
        assert!(
            !missing.contains("/nonexistent/"),
            "{shell}: a missing dir is never prepended: {missing:?}"
        );
        if !exact {
            continue;
        }
        assert_eq!(missing, "/usr/bin:/bin", "{shell}: inert for missing dirs");
        assert_eq!(
            run(None, None, "/usr/bin:/bin", ""),
            "/usr/bin:/bin",
            "{shell}: inert when unset"
        );
        assert_eq!(
            run(
                both.0,
                both.1,
                "/usr/bin:/bin",
                &format!("PATH=':{agents}'; ")
            ),
            format!("{reroute}:{agents}:"),
            "{shell}: a sole empty entry survives"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_an_unmanaged_aterm_bin_is_never_put_on_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut shells = ShellLanes::default();
    let mut lanes: Vec<(&str, Vec<&str>, &str, &str, bool)> = shells
        .bashes()
        .into_iter()
        .map(|bash| {
            (
                bash,
                vec!["--noprofile", "--norc", "-i", "-c"],
                "aterm_shell_integration.bash",
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; printf 'PATH=%s\\n' \"$PATH\"",
                true,
            )
        })
        .collect();
    if let Some(zsh) = shells.zsh() {
        lanes.push((
            zsh,
            vec!["-f", "-i", "-c"],
            "aterm_shell_integration.zsh",
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; print -r -- \"PATH=$PATH\"",
            true,
        ));
    }
    if let Some(fish) = shells.fish() {
        lanes.push((
            fish,
            vec!["-i", "-c"],
            "aterm_shell_integration.fish",
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; printf 'PATH=%s\\n' (string join : -- $PATH)",
            false,
        ));
    }
    for (shell, args, script_name, command, exact) in lanes {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let mut cmd = shell_command(shell);
        let bin = cmd.home().join(".aterm").join("bin");
        std::fs::create_dir_all(&bin).expect("mk ~/.aterm/bin");
        let shadow = bin.join("ls");
        std::fs::write(&shadow, "#!/bin/sh\necho shadowed\n").expect("write the shadow");
        std::fs::set_permissions(&shadow, std::fs::Permissions::from_mode(0o755))
            .expect("make the shadow executable");
        let bin = bin.to_str().expect("UTF-8 home").to_owned();
        let out = cmd
            .args(&args)
            .arg(command)
            .env("ATERM_TEST_SCRIPT", &script)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap_or_else(|error| panic!("spawn {shell}: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("PATH="),
            "{shell}: no PATH line; stdout={stdout:?} stderr={:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let path: String = stdout
            .rsplit("PATH=")
            .next()
            .expect("rsplit yields at least one piece")
            .chars()
            .take_while(|c| !c.is_control())
            .collect();
        assert!(
            !path.split(':').any(|entry| entry == bin),
            "{shell}: ~/.aterm/bin must not be on PATH: {path:?}"
        );
        if exact {
            assert_eq!(path, "/usr/bin:/bin", "{shell}: PATH untouched");
        }
    }
    let named = scripts::POWERSHELL
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find(|line| line.contains(".aterm/bin") || line.contains(".aterm\\bin"));
    assert_eq!(
        named, None,
        "the PowerShell integration must not put ~/.aterm/bin on PATH either"
    );
}

/// The 633;E encoding of `line`, byte for byte.
#[cfg(unix)]
fn osc633_encode(line: &str) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(line.len());
    for &byte in line.as_bytes() {
        match byte {
            b'\\' => encoded.extend_from_slice(br"\\"),
            b';' => encoded.extend_from_slice(br"\x3b"),
            b' ' => encoded.extend_from_slice(br"\x20"),
            0x00..=0x1f | 0x7f => encoded.extend_from_slice(format!(r"\x{byte:02x}").as_bytes()),
            _ => encoded.push(byte),
        }
    }
    encoded
}

#[cfg(unix)]
#[test]
fn test_encoders_answer_in_reply_and_keep_the_users() {
    let corpus = [
        "/tmp/a b",
        "/tmp/100%",
        "/tmp/résumé 🦀",
        "/tmp/a\nb",
        "/tmp/tab\there",
        "/tmp/\u{1b}]52;c;x\u{7}",
        "/tmp/del\u{7f}",
        "/x/;\\y",
        "",
        "plain/path-1_2.~",
    ];
    let mut command = String::new();
    for i in 0..corpus.len() {
        write!(
            command,
            "__aterm_urlencode \"$ATERM_TEST_IN_{i}\"; printf '%s\\0' \"$REPLY\"; \
             __aterm_encode_cmd \"$ATERM_TEST_IN_{i}\"; printf '%s\\0' \"$REPLY\"; "
        )
        .expect("write to String");
    }
    let mut want = Vec::new();
    for line in corpus {
        want.extend_from_slice(osc7_percent_encode(line).as_bytes());
        want.push(0);
        want.extend(osc633_encode(line));
        want.push(0);
    }
    let zsh_quiet = "add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; \
                     add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true";
    let bash_quiet = "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=";
    let mut shells = ShellLanes::default();
    let mut runs: Vec<(&str, &[&str], String, &str, &str)> = shells
        .bashes()
        .into_iter()
        .map(|bash| {
            (
                bash,
                &["--noprofile", "--norc", "-i"][..],
                bash_quiet.to_owned(),
                "aterm_shell_integration.bash",
                "",
            )
        })
        .collect();
    if let Some(zsh) = shells.zsh() {
        for options in ["", "setopt ksh_arrays"] {
            runs.push((
                zsh,
                &["-f", "-i"],
                format!("{zsh_quiet}; {options}"),
                "aterm_shell_integration.zsh",
                "__aterm_preexec 'echo hi' >/dev/null; ",
            ));
        }
    }
    for (shell, args, quiet, script_name, preexec) in runs {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let line = format!(
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {quiet}; {command}\
             REPLY=mine; __aterm_report_cwd >/dev/null; {preexec}printf 'KEPT=%s' \"$REPLY\""
        );
        let mut cmd = shell_command(shell);
        cmd.args(args)
            .arg("-c")
            .arg(&line)
            .env("ATERM_TEST_SCRIPT", script);
        for (i, input) in corpus.iter().enumerate() {
            cmd.env(format!("ATERM_TEST_IN_{i}"), input);
        }
        let output = cmd
            .output()
            .unwrap_or_else(|error| panic!("spawn {shell} for the encoder corpus: {error}"));
        assert!(
            output.status.success(),
            "{shell} {quiet}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut got = output.stdout;
        if let Some(start) = got.len().checked_sub(want.len() + b"KEPT=mine".len()) {
            got.drain(..start);
        }
        let (encoded, kept) = got.split_at(got.len().saturating_sub(b"KEPT=mine".len()));
        assert_eq!(
            String::from_utf8_lossy(encoded),
            String::from_utf8_lossy(&want),
            "{shell} {quiet}: encodings"
        );
        assert_eq!(kept, b"KEPT=mine", "{shell} {quiet}: the user's REPLY");
    }
}

#[cfg(unix)]
#[test]
fn fish_encodes_a_command_line_as_the_633e_decoder_reads_it() {
    let fish = need_fish();
    let corpus = [
        "/tmp/a b",
        "/tmp/100%",
        "/tmp/résumé 🦀",
        "/tmp/a\nb",
        "/tmp/tab\there",
        "/tmp/\u{1b}]52;c;x\u{7}",
        "/tmp/del\u{7f}",
        "/x/;\\y",
        "",
        "plain/path-1_2.~",
        "a\tb\tc\u{1b}d\n\ne;f\\g \u{7}\n",
        "\u{7}\u{7}\r",
        "\\x3b\\\\ \\;",
        "for f in *\n    echo \"$f\"\nend",
    ];
    let mut body = String::new();
    let mut want = Vec::new();
    for (i, line) in corpus.iter().enumerate() {
        write!(
            body,
            "set -l e (__aterm_encode_cmd \"$ATERM_TEST_IN_{i}\"); printf '%s\\0' \"$e\"; "
        )
        .expect("write to String");
        want.extend(osc633_encode(line));
        want.push(0);
    }
    let names: Vec<String> = (0..corpus.len())
        .map(|i| format!("ATERM_TEST_IN_{i}"))
        .collect();
    let env: Vec<(&str, &str)> = names
        .iter()
        .map(String::as_str)
        .zip(corpus.iter().copied())
        .collect();
    assert_eq!(
        run_fish_integration(fish, &env, &body),
        String::from_utf8_lossy(&want),
        "fish: 633;E encodings"
    );
}

#[cfg(unix)]
#[test]
fn test_report_cwd_emits_percent_encoded_osc_7() {
    let (_dir, cwd) = create_special_cwd();
    let expected = format!(
        "\u{1b}]7;file://aterm.test{}\u{7}",
        osc7_percent_encode(cwd.to_str().expect("cwd path should be UTF-8"))
    );
    let mut shells = ShellLanes::default();
    for (shell, args, script_name, quiet) in posix_lanes(&mut shells) {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let output = shell_command(shell)
            .args(args)
            .arg("-c")
            .arg(format!(
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {quiet}; \
                 builtin cd -- \"$ATERM_TEST_CWD\"; __aterm_report_cwd"
            ))
            .env("ATERM_TEST_SCRIPT", script)
            .env("ATERM_TEST_CWD", &cwd)
            .env("HOSTNAME", "aterm.test")
            .env("HOST", "aterm.test")
            .output()
            .unwrap_or_else(|error| panic!("spawn {shell}: {error}"));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{shell}; stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// The nonce every lane's cwd report is signed with in the tests.
#[cfg(unix)]
const REPORT_NONCE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The report a shell with the nonce `REPORT_NONCE` sends for `uri`.
#[cfg(unix)]
fn signed_report(uri: &str) -> String {
    format!("\u{1b}]633;P;AtermCwd={uri};id={REPORT_NONCE}\u{7}\u{1b}]7;{uri}\u{7}")
}

#[cfg(unix)]
#[test]
fn test_report_cwd_is_signed_with_the_captured_nonce() {
    let (_dir, cwd) = create_special_cwd();
    let expected = signed_report(&format!(
        "file://aterm.test{}",
        osc7_percent_encode(cwd.to_str().expect("cwd path should be UTF-8"))
    ));
    let mut shells = ShellLanes::default();
    for (shell, args, script_name, quiet) in posix_lanes(&mut shells) {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let output = shell_command(shell)
            .args(args)
            .arg("-c")
            .arg(format!(
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {quiet}; \
                 builtin cd -- \"$ATERM_TEST_CWD\"; __aterm_report_cwd"
            ))
            .env("ATERM_TEST_SCRIPT", script)
            .env("ATERM_TEST_CWD", &cwd)
            .env("ATERM_SHELL_NONCE", REPORT_NONCE)
            .env("HOSTNAME", "aterm.test")
            .env("HOST", "aterm.test")
            .output()
            .unwrap_or_else(|error| panic!("spawn {shell}: {error}"));
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{shell}; stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn test_fish_report_cwd_is_signed_with_the_captured_nonce() {
    let fish = need_fish();
    let (_dir, cwd) = create_special_cwd();
    assert_eq!(
        run_fish_integration(
            fish,
            &[("HOST", "aterm.test"), ("ATERM_SHELL_NONCE", REPORT_NONCE)],
            &format!("cd -- '{}'", cwd.display()),
        ),
        signed_report(&format!(
            "file://aterm.test{}",
            osc7_percent_encode(&cwd.to_string_lossy())
        )),
    );
}

#[cfg(unix)]
#[test]
fn test_fish_reports_its_cwd_at_load_only_for_an_upgrade_in_place() {
    let fish = need_fish();
    let (_dir, cwd) = create_special_cwd();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.fish",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut cmd = shell_command(fish);
    cmd.args(["-i", "-c"])
        .arg(format!(
            "cd -- '{}'; source \"$ATERM_TEST_SCRIPT\"; echo FRESH-LOADED; \
             source \"$ATERM_TEST_SCRIPT\"; echo IN-PLACE-LOADED",
            cwd.display()
        ))
        .env("ATERM_TEST_SCRIPT", &script)
        .env("HOST", "aterm.test")
        .env("ATERM_SHELL_NONCE", REPORT_NONCE);
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn fish: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (fresh, rest) = stdout
        .split_once("FRESH-LOADED")
        .unwrap_or_else(|| panic!("the fresh load ran; stdout: {stdout:?}"));
    let (in_place, _) = rest
        .split_once("IN-PLACE-LOADED")
        .unwrap_or_else(|| panic!("the in-place load ran; stdout: {stdout:?}"));
    assert!(
        !fresh.contains("\u{1b}]7;") && !fresh.contains("AtermCwd="),
        "a fresh load leaves the cwd report to its first prompt: {fresh:?}"
    );
    assert!(
        in_place.contains(&signed_report(&format!(
            "file://aterm.test{}",
            osc7_percent_encode(&cwd.to_string_lossy())
        ))),
        "an upgrade in place reports the cwd at load: {in_place:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn test_pwsh_report_cwd_is_signed_with_the_captured_nonce() {
    let pwsh = need_pwsh();
    let (_dir, cwd) = create_special_cwd();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut cmd = shell_command(pwsh);
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        ". $env:ATERM_TEST_SCRIPT; Set-Location -LiteralPath $env:ATERM_TEST_CWD; \
         [Console]::Write((prompt))",
    ])
    .env("ATERM_TEST_SCRIPT", &script)
    .env("ATERM_TEST_CWD", &cwd)
    .env("ATERM_SHELL_NONCE", REPORT_NONCE)
    .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
    .env("POWERSHELL_UPDATECHECK", "Off");
    let out = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn {pwsh}: {error}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let uri = stdout
        .split_once("\u{1b}]7;")
        .and_then(|(_, rest)| rest.split_once('\u{7}'))
        .map(|(uri, _)| uri)
        .unwrap_or_else(|| {
            panic!(
                "the prompt reports its cwd: {stdout:?}; stderr: {:?}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
    let encoded = osc7_percent_encode(&cwd.to_string_lossy());
    assert!(
        uri.starts_with("file://") && uri.ends_with(&encoded),
        "the OSC 7 is the URI alone: {uri:?}"
    );
    assert!(stdout.contains(&signed_report(uri)), "{stdout:?}");
}

#[cfg(unix)]
#[test]
fn test_pwsh_prompt_installs_the_readline_shim_again_after_a_reimport() {
    let pwsh = need_pwsh();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    let run = |body: &str| -> String {
        let mut cmd = shell_command(pwsh);
        cmd.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("Import-Module PSReadLine; . $env:ATERM_TEST_SCRIPT; {body}"),
        ])
        .env("ATERM_TEST_SCRIPT", &script)
        .env("ATERM_SHELL_NONCE", REPORT_NONCE)
        .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
        .env("POWERSHELL_UPDATECHECK", "Off");
        let out = cmd
            .output()
            .unwrap_or_else(|error| panic!("spawn {pwsh}: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{stdout:?}; stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    };
    let report = "$f = $function:PSConsoleHostReadLine; \
         if ($f.Module) { [Console]::Write('in-place=' + $f.Module.Name + '|'); exit }; \
         [Console]::Write('wraps=' + $Global:__aterm_original_readline.Module.Name + '|'); \
         $Global:__aterm_original_readline = { 'Get-Location' }; \
         [Console]::Write('line=' + (PSConsoleHostReadLine) + '|')";

    let reimported = run(&format!(
        "Import-Module PSReadLine -Force; $null = prompt; $null = prompt; {report}"
    ));
    let marked = format!("\u{1b}]133;C;id={REPORT_NONCE}\u{7}");
    assert!(
        reimported.contains("wraps=PSReadLine|"),
        "the shim wraps the re-imported readline: {reimported:?}"
    );
    assert!(
        reimported.contains(&format!("{marked}line=Get-Location|")),
        "the shim marks the line it read: {reimported:?}"
    );

    let wrapped = run("$Global:__test_prev = $function:PSConsoleHostReadLine; \
         function Global:PSConsoleHostReadLine { 'user:' + (& $Global:__test_prev) }; \
         $null = prompt; \
         [Console]::Write('kept=' + ($function:PSConsoleHostReadLine.ToString() -match 'user:') + '|')");
    assert!(
        wrapped.contains("kept=True|"),
        "a wrapper over the shim stays: {wrapped:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_bash_on_msys_and_cygwin_reports_the_windows_drive_form() {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let calls = dir.path().join("cygpath-calls");
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let _ = std::fs::remove_file(&calls);
        let run = |ostype: &str, pwds: &[&str]| -> Vec<String> {
            let mut command = format!(
                "OSTYPE={ostype}; source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
                 __aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=; \
                 cygpath() {{ printf x >>\"$ATERM_TEST_CALLS\"; \
                   case \"$3\" in /cygdrive/c/*) printf 'C:/%s\\n' \"${{3#/cygdrive/c/}}\" ;; \
                   *) printf 'C:/Program Files/Git%s\\n' \"$3\" ;; esac; }}; "
            );
            for pwd in pwds {
                command.push_str(&format!("PWD='{pwd}'; __aterm_report_cwd; printf '|'; "));
            }
            let output = shell_command(bash)
                .args(["--noprofile", "--norc", "-i", "-c", &command])
                .env("ATERM_TEST_SCRIPT", &script)
                .env("ATERM_TEST_CALLS", &calls)
                .env("HOSTNAME", "aterm.test")
                .output()
                .unwrap_or_else(|error| panic!("spawn {bash} for MSYS cwd test: {error}"));
            assert!(
                output.status.success(),
                "{bash} should report OSC 7 as {ostype}; stderr: {:?}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout)
                .split('|')
                .filter(|s| !s.is_empty())
                .map(|osc| {
                    osc.strip_prefix("\u{1b}]7;file://aterm.test")
                        .and_then(|s| s.strip_suffix('\u{7}'))
                        .unwrap_or_else(|| panic!("not an OSC 7 report: {osc:?}"))
                        .to_owned()
                })
                .collect()
        };
        let calls_made = || std::fs::read(&calls).map_or(0, |b| b.len());

        let reported = run(
            "msys",
            &[
                "/c/Windows/Temp",
                "/d",
                "/C/Users//me/My Dir",
                "/usr/bin",
                "/usr/bin",
                "/cmd",
            ],
        );
        assert_eq!(
            reported,
            [
                "/C%3A/Windows/Temp",
                "/D%3A",
                "/C%3A/Users//me/My%20Dir",
                "/C%3A/Program%20Files/Git/usr/bin",
                "/C%3A/Program%20Files/Git/usr/bin",
                "/C%3A/Program%20Files/Git/cmd",
            ],
            "{bash}"
        );
        assert_eq!(
            calls_made(),
            2,
            "{bash}: cygpath runs once per non-drive directory"
        );

        let _ = std::fs::remove_file(&calls);
        let reported = run("cygwin", &["/cygdrive/c/Users", "/c"]);
        assert_eq!(
            reported,
            ["/C%3A/Users", "/C%3A/Program%20Files/Git/c"],
            "{bash}"
        );
        assert_eq!(calls_made(), 2, "{bash}");

        let _ = std::fs::remove_file(&calls);
        let reported = run("darwin24", &["/c/Windows/Temp"]);
        assert_eq!(reported, ["/c/Windows/Temp"], "{bash}");
        assert_eq!(calls_made(), 0, "{bash}");
    }
}

#[cfg(unix)]
#[test]
fn test_bash_preexec_skips_prompt_command_array_siblings() {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    let command = "\
__fake_vte_precmd() { :; }\n\
__fake_starship_precmd() { :; }\n\
PROMPT_COMMAND=(__fake_vte_precmd __fake_starship_precmd)\n\
source \"$ATERM_TEST_SCRIPT\"\n\
__aterm_prompt_command\n\
__fake_vte_precmd\n\
__fake_starship_precmd\n\
__aterm_prompt_end\n\
__aterm_test_marker() { printf '===CYCLE-DONE==='; }\n\
__aterm_test_marker\n\
echo real-command\n";
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let output = shell_command(bash)
            .args(["--noprofile", "--norc", "-i", "-c", command])
            .env("ATERM_TEST_SCRIPT", &script)
            .output()
            .unwrap_or_else(|error| panic!("spawn {bash} for preexec array test: {error}"));
        assert!(
            output.status.success(),
            "{bash}: the preexec array cycle should succeed; stdout: {:?}; stderr: {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let (prompt_cycle, after) = stdout
            .split_once("===CYCLE-DONE===")
            .expect("marker should appear in stdout");
        assert!(
            !prompt_cycle.contains("633;E"),
            "{bash}: a sibling PROMPT_COMMAND element must not be captured as a user command: \
             {prompt_cycle:?}"
        );
        assert!(
            !prompt_cycle.contains("\u{1b}]133;C"),
            "{bash}: no command-start mark may fire during the prompt cycle: {prompt_cycle:?}"
        );
        assert!(
            after.contains("\u{1b}]633;E;echo\\x20real-command\u{7}"),
            "{bash}: the real command must still be captured (633;E): {after:?}"
        );
        assert!(
            after.contains("\u{1b}]133;C\u{7}"),
            "{bash}: the real command must emit the 133;C command start, unsigned with no nonce: \
             {after:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_bash_prompt_command_array_of_one_runs_its_element_once_a_prompt() {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    let command = "\
__user_precmd() { printf '[user-precmd]'; }\n\
PROMPT_COMMAND=(__user_precmd)\n\
source \"$ATERM_TEST_SCRIPT\"\n\
printf '[n=%s first=%s]' \"${#PROMPT_COMMAND[@]}\" \"${PROMPT_COMMAND[0]}\"\n\
eval \"$PROMPT_COMMAND\"\n\
eval \"$PROMPT_COMMAND\"\n";
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let output = shell_command(bash)
            .args(["--noprofile", "--norc", "-i", "-c", command])
            .env("ATERM_TEST_SCRIPT", &script)
            .output()
            .unwrap_or_else(|error| panic!("spawn {bash} for the one-element array test: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{bash}: stdout: {stdout:?}; stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("[n=1 first=__aterm_prompt_command]"),
            "{bash}: ours takes element 0 and adds none: {stdout:?}"
        );
        assert_eq!(
            stdout.matches("[user-precmd]").count(),
            2,
            "{bash}: the user's element runs once a prompt, two prompts: {stdout:?}"
        );
    }
}

#[test]
fn test_detect_posix_shells() {
    for (path, shell) in [
        ("/bin/zsh", ShellType::Zsh),
        ("/bin/bash", ShellType::Bash),
        ("bash5", ShellType::Bash),
        ("/usr/bin/fish", ShellType::Fish),
        ("/bin/sh", ShellType::Unknown),
        ("", ShellType::Unknown),
        ("pwsh", ShellType::PowerShell),
        ("PowerShell.EXE", ShellType::PowerShell),
        (
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            ShellType::PowerShell,
        ),
        ("wsl", ShellType::Wsl),
        ("cmd", ShellType::Cmd),
        ("C:\\Windows\\System32\\wsl.exe", ShellType::Wsl),
        ("C:\\Windows\\System32\\CMD.EXE", ShellType::Cmd),
        ("nu.exe", ShellType::Unknown),
    ] {
        assert_eq!(ShellType::detect(path), shell, "{path:?}");
    }
}

#[cfg(unix)]
#[test]
fn test_cwd_tab_title_strips_control_chars() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().join("x\u{1b}]52;c;aGVsbG8=\u{7}y");
    std::fs::create_dir(&cwd).expect("create the crafted cwd");
    let assert_clean = |shell: &str, wire: &[u8]| {
        assert!(
            wire.windows(3).any(|w| w == b"\x1b]0"),
            "{shell}: the prompt hook titles the tab: {:?}",
            String::from_utf8_lossy(wire)
        );
        assert!(
            !wire.windows(5).any(|w| w == b"\x1b]52;"),
            "{shell}: a crafted cwd smuggled a nested OSC 52 out of the title: {:?}",
            String::from_utf8_lossy(wire)
        );
    };
    let mut shells = ShellLanes::default();
    if let Some(fish) = shells.fish() {
        let stdout = run_fish_integration(
            fish,
            &[("ATERM_TEST_CWD", cwd.to_str().expect("a UTF-8 crafted cwd"))],
            "cd -- \"$ATERM_TEST_CWD\"; function __aterm_original_fish_prompt; end; fish_prompt",
        );
        assert_clean(fish, stdout.as_bytes());
    }
    let mut lanes: Vec<_> = shells
        .bashes()
        .into_iter()
        .map(|bash| {
            (
                bash,
                vec!["--noprofile", "--norc", "-i"],
                "aterm_shell_integration.bash",
                "__aterm_prompt_command",
            )
        })
        .collect();
    if let Some(zsh) = shells.zsh() {
        lanes.push((
            zsh,
            vec!["-f", "-i"],
            "aterm_shell_integration.zsh",
            "__aterm_precmd",
        ));
    }
    for (shell, args, script_name, hook) in lanes {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let output = shell_command(shell)
            .args(&args)
            .arg("-c")
            .arg(format!(
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; trap - DEBUG 2>/dev/null; \
                 builtin cd -- \"$ATERM_TEST_CWD\"; {hook}"
            ))
            .env("ATERM_TEST_SCRIPT", &script)
            .env("ATERM_TEST_CWD", &cwd)
            .output()
            .unwrap_or_else(|error| panic!("spawn {shell}: {error}"));
        assert_clean(shell, &output.stdout);
    }
}

#[cfg(unix)]
#[test]
fn test_osc_frame_writer_preserves_payload_escapes() {
    const CMDLINE: &str = "a\u{1b}b\u{7}c;d e\\f \\e]52;c;aGVsbG8=\\a";
    const PAYLOAD: &str = r"a\x1bb\x07c\x3bd\x20e\\f\x20\\e]52\x3bc\x3baGVsbG8=\\a";
    let expected = format!(
        "\u{1b}]633;E;{PAYLOAD}\u{7}\u{1b}]0;{}\u{7}",
        r"abc;d e\f \e]52;c;aGVsbG8=\a"
    );
    let mut shells = ShellLanes::default();
    if let Some(fish) = shells.fish() {
        let encoded = run_fish_integration(
            fish,
            &[("ATERM_TEST_INPUT", CMDLINE)],
            "printf '%s' \"$(__aterm_encode_cmd \"$ATERM_TEST_INPUT\")\"",
        );
        assert_eq!(encoded, PAYLOAD, "fish");
    }
    for (shell, args, script_name, quiet) in posix_lanes(&mut shells) {
        let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
        let output = shell_command(shell)
            .args(args)
            .arg("-c")
            .arg(format!(
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {quiet}; \
                 __aterm_encode_cmd \"$ATERM_TEST_INPUT\"; __aterm_osc \"633;E;$REPLY\"; \
                 __aterm_osc \"0;${{ATERM_TEST_INPUT//[[:cntrl:]]/}}\""
            ))
            .env("ATERM_TEST_SCRIPT", script)
            .env("ATERM_TEST_INPUT", CMDLINE)
            .output()
            .unwrap_or_else(|error| panic!("spawn {shell}: {error}"));
        assert!(
            output.stdout.ends_with(expected.as_bytes()),
            "{shell}: OSC frames must carry the payload byte-for-byte;\n  expected suffix: \
             {expected:?}\n  got: {:?}\n  stderr: {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn test_prepare_unknown_returns_none() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    assert!(prepare_into(ShellType::Unknown, &base).unwrap().is_none());
    assert!(
        !base.exists(),
        "prepare_into(Unknown) must not create the cache directory"
    );
}

/// Source the fish integration in an interactive fish, run `body`, and return its stdout.
#[cfg(unix)]
fn run_fish_integration(fish: &str, extra_env: &[(&str, &str)], body: &str) -> String {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.fish",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut cmd = shell_command(fish);
    cmd.arg("-i")
        .arg("-c")
        .arg(format!(
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {body}"
        ))
        .env("ATERM_TEST_SCRIPT", &script);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn fish for integration test: {error}"));
    assert!(
        output.status.success(),
        "fish integration body should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn test_fish_integration_loads_under_an_empty_guard_override() {
    let fish = need_fish();
    let stdout = run_fish_integration(
        fish,
        &[
            ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
            (
                "ATERM_SHELL_NONCE",
                "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899",
            ),
        ],
        "functions -q __aterm_mark_prompt_start; echo LOADED=$status; \
         echo NONCE_SCRUBBED=(set -q ATERM_SHELL_NONCE; echo $status)",
    );
    assert!(
        stdout.contains("LOADED=0"),
        "an empty ATERM_SHELL_INTEGRATION_INSTALLED must NOT be read as \
         'already loaded' — aterm sets it empty on purpose so a nested shell \
         re-loads the integration. stdout: {stdout:?}"
    );
    assert!(
        stdout.contains("NONCE_SCRUBBED=1"),
        "the #8015 nonce scrub lives past the loader guard, so a guard that \
         fires too early also leaks ATERM_SHELL_NONCE to every child process. \
         stdout: {stdout:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_fish_emits_all_133_marks_with_and_without_a_nonce() {
    let fish = need_fish();
    let stdout = run_fish_integration(
        fish,
        &[("ATERM_DISABLE_PROMPT_TITLES", "1")],
        "__aterm_mark_prompt_start; __aterm_mark_command_start; \
         __aterm_mark_exec_start; __aterm_mark_exec_finish 7; \
         __aterm_fish_preexec 'echo hi'",
    );
    let expected = concat!(
        "\u{1b}]133;A\u{7}",
        "\u{1b}]133;B\u{7}",
        "\u{1b}]133;C\u{7}",
        "\u{1b}]133;D;7\u{7}",
        "\u{1b}]633;E;echo\\x20hi\u{7}",
        "\u{1b}]133;C\u{7}",
    );
    assert_eq!(stdout, expected, "un-nonced");
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let stdout = run_fish_integration(
        fish,
        &[
            ("ATERM_SHELL_NONCE", nonce),
            ("ATERM_DISABLE_PROMPT_TITLES", "1"),
        ],
        "__aterm_mark_prompt_start; __aterm_mark_exec_finish 0",
    );
    assert_eq!(
        stdout,
        format!("\u{1b}]133;A;id={nonce}\u{7}\u{1b}]133;D;0;id={nonce}\u{7}"),
        "nonced"
    );
}

#[cfg(unix)]
#[test]
fn test_fish_empty_prompt_style_falls_through_to_the_user_prompt() {
    let fish = need_fish();
    let body = "function __aterm_custom_prompt; printf 'CUSTOM'; end; \
                function __aterm_original_fish_prompt; printf 'ORIGINAL'; end; \
                function __aterm_report_cwd; end; \
                fish_prompt";
    for (style, expected) in [
        ("", "ORIGINAL"),
        ("none", "ORIGINAL"),
        ("minimal", "CUSTOM"),
    ] {
        let stdout = run_fish_integration(
            fish,
            &[
                ("ATERM_PROMPT_STYLE", style),
                ("ATERM_DISABLE_PROMPT_TITLES", "1"),
            ],
            body,
        );
        assert_eq!(
            stdout,
            format!("\u{1b}]133;A\u{7}{expected}\u{1b}]133;B\u{7}"),
            "ATERM_PROMPT_STYLE={style:?} must select the {expected} prompt branch"
        );
    }
}

#[cfg(unix)]
#[test]
fn test_fish_empty_disable_prompt_titles_still_emits_titles() {
    let fish = need_fish();
    let with_title = concat!(
        "\u{1b}]633;E;echo\\x20hi\u{7}",
        "\u{1b}]0;echo hi\u{7}",
        "\u{1b}]133;C\u{7}",
    );
    let without_title = concat!("\u{1b}]633;E;echo\\x20hi\u{7}", "\u{1b}]133;C\u{7}");
    let body = "__aterm_fish_preexec 'echo hi'";
    assert_eq!(
        run_fish_integration(fish, &[("ATERM_DISABLE_PROMPT_TITLES", "")], body),
        with_title,
        "an EMPTY ATERM_DISABLE_PROMPT_TITLES means 'not disabled' in bash and \
         zsh; fish must agree and still emit the OSC 0 title"
    );
    assert_eq!(
        run_fish_integration(fish, &[("ATERM_DISABLE_PROMPT_TITLES", "1")], body),
        without_title,
        "a non-empty ATERM_DISABLE_PROMPT_TITLES must still suppress the title"
    );
}

#[cfg(unix)]
#[test]
fn test_a_long_command_title_ends_with_its_cut_mark() {
    let whole = format!("echo {}", "w".repeat(200));
    let long = format!("echo {}", "l".repeat(300));
    let cut = format!("{}…", long.chars().take(255).collect::<String>());
    let title = |wire: &str| {
        wire.split_once("\u{1b}]0;")
            .and_then(|(_, rest)| rest.split_once('\u{7}'))
            .map(|(title, _)| title.to_owned())
            .unwrap_or_default()
    };
    let mut shells = ShellLanes::default();
    for (line, expected) in [(&whole, &whole), (&long, &cut)] {
        if let Some(fish) = shells.fish() {
            let wire = run_fish_integration(
                fish,
                &[
                    ("ATERM_TEST_INPUT", line.as_str()),
                    ("ATERM_DISABLE_PROMPT_TITLES", ""),
                ],
                "__aterm_fish_preexec \"$ATERM_TEST_INPUT\"",
            );
            assert_eq!(&title(&wire), expected, "fish: {wire:?}");
        }
        for (shell, args, script_name, quiet) in posix_lanes(&mut shells) {
            let hook = if script_name.ends_with(".bash") {
                "__aterm_line_start"
            } else {
                "__aterm_preexec"
            };
            let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
            let output = shell_command(shell)
                .args(args)
                .arg("-c")
                .arg(format!(
                    "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; {quiet}; \
                     {hook} \"$ATERM_TEST_INPUT\""
                ))
                .env("ATERM_TEST_SCRIPT", script)
                .env("ATERM_TEST_INPUT", line)
                .env("ATERM_DISABLE_PROMPT_TITLES", "")
                .output()
                .unwrap_or_else(|error| panic!("spawn {shell}: {error}"));
            let wire = String::from_utf8_lossy(&output.stdout);
            assert_eq!(&title(&wire), expected, "{shell}: {wire:?}");
        }
    }
}

#[cfg(unix)]
#[test]
fn test_fish_empty_prompt_colors_fall_back_to_defaults() {
    let fish = need_fish();
    let body = "function prompt_pwd; printf '~/x'; end; __aterm_custom_prompt";
    let empty_colors = [
        ("ATERM_PROMPT_STYLE", "minimal"),
        ("ATERM_PROMPT_HOST_COLOR", ""),
        ("ATERM_PROMPT_PATH_COLOR", ""),
        ("ATERM_PROMPT_GIT_COLOR", ""),
        ("ATERM_PROMPT_ERROR_COLOR", ""),
        ("ATERM_PROMPT_SEP_COLOR", ""),
    ];
    assert_eq!(
        run_fish_integration(fish, &empty_colors, body),
        "\u{1b}[38;5;4m~/x\u{1b}[0m \u{1b}[38;5;8m$\u{1b}[0m ",
        "empty ATERM_PROMPT_*_COLOR values must fall back to the same defaults \
         bash and zsh get from `${{ATERM_PROMPT_*_COLOR:-N}}`"
    );
    assert_eq!(
        run_fish_integration(
            fish,
            &[
                ("ATERM_PROMPT_STYLE", "minimal"),
                ("ATERM_PROMPT_PATH_COLOR", "5"),
            ],
            body,
        ),
        "\u{1b}[38;5;5m~/x\u{1b}[0m \u{1b}[38;5;8m$\u{1b}[0m ",
        "a non-empty ATERM_PROMPT_PATH_COLOR must still reach the prompt"
    );
}

/// Absolute path to a `fish` binary, or `None` where the host has none ([`interned_test_shell`]).
#[cfg(unix)]
fn fish_shell() -> Option<&'static str> {
    static FISH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    interned_test_shell(&FISH, "fish")
}

#[cfg(unix)]
#[test]
fn test_bash_children_never_inherit_the_nonce_through_ps1() {
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    let command = "\
source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1\n\
__aterm_prompt_command >/dev/null 2>&1\n\
[[ -n \"$__aterm_shell_nonce\" && \"$PS1\" == *\";id=$__aterm_shell_nonce\"* ]] || exit 96\n\
env >\"$HOME/child-env-1\"\n\
export PS1\n\
__aterm_prompt_command >/dev/null 2>&1\n\
env >\"$HOME/child-env-2\"\n";
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let mut cmd = shell_command(bash);
        cmd.args(["--noprofile", "--norc", "-i", "-c", command])
            .env("ATERM_TEST_SCRIPT", &script)
            .env("ATERM_SHELL_NONCE", nonce)
            .env("PS1", "inherited$ ");
        let output = cmd
            .output()
            .unwrap_or_else(|error| panic!("spawn {bash} for PS1 nonce test: {error}"));
        assert!(
            output.status.success(),
            "{bash}: the PS1 nonce probe should succeed (96: PS1 never carried the nonce); \
             status: {:?}; stderr: {:?}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        for probe in ["child-env-1", "child-env-2"] {
            let env = std::fs::read_to_string(cmd.home().join(probe))
                .unwrap_or_else(|error| panic!("read {probe}: {error}"));
            assert!(
                env.contains("ATERM_TEST_SCRIPT="),
                "{bash}: {probe} must hold the child's environment: {env:?}"
            );
            assert!(
                !env.contains(nonce),
                "{bash}: a child of an integrated bash must not inherit the nonce ({probe}): {env:?}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn test_zsh_unsets_shell_nonce_env_after_source() {
    let zsh = need_zsh();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.zsh",
        env!("CARGO_MANIFEST_DIR")
    );
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    for (given, suffix) in [(Some(nonce), format!(";id={nonce}")), (None, String::new())] {
        let mut cmd = shell_command(zsh);
        cmd.args(["-f", "-i", "-c"])
            .arg(
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
                 printf 'env=%s|suffix=%s' \"${ATERM_SHELL_NONCE-UNSET}\" \"$(__aterm_id_suffix)\"",
            )
            .env("ATERM_TEST_SCRIPT", &script);
        if let Some(nonce) = given {
            cmd.env("ATERM_SHELL_NONCE", nonce);
        }
        let out = cmd.output().expect("spawn zsh");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("env=UNSET|suffix={suffix}"),
            "nonce {given:?}; stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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
    assert!(
        scripts::POWERSHELL.is_ascii(),
        "aterm_shell_integration.ps1 must stay ASCII-only (written without a BOM)"
    );
}

#[cfg(windows)]
#[test]
fn test_powershell_speaks_utf8_only_inside_an_aterm_tab() {
    let probe = "[Console]::Write([string]$OutputEncoding.CodePage + '|' + [string][Console]::OutputEncoding.CodePage)";
    let fresh = run_powershell(probe, None, Pwsh::Bare);
    assert_ne!(
        fresh, "65001|65001",
        "a fresh console is not UTF-8 both ways"
    );
    assert_eq!(run_powershell(probe, None, Pwsh::InTab), "65001|65001");
    assert_eq!(
        run_powershell(probe, None, Pwsh::Outside),
        fresh,
        "a console outside aterm is left alone"
    );
}

/// How a test pwsh runs: without the integration script, with it outside an aterm session.
#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pwsh {
    Bare,
    Outside,
    InTab,
}

#[cfg(windows)]
fn run_powershell_snippet(snippet: &str, nonce: Option<&str>) -> String {
    run_powershell(snippet, nonce, Pwsh::Outside)
}

#[cfg(windows)]
fn run_powershell(snippet: &str, nonce: Option<&str>, how: Pwsh) -> String {
    let output = powershell_output(Path::new("powershell.exe"), "", snippet, nonce, how, &[]);
    assert!(
        output.status.success(),
        "powershell snippet {snippet:?} should succeed; stdout: {:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// One run of `exe` (`powershell.exe`, or a pwsh 7 path) in a hidden console of its own.
#[cfg(windows)]
fn powershell_output(
    exe: &Path,
    prelude: &str,
    snippet: &str,
    nonce: Option<&str>,
    how: Pwsh,
    env: &[(&str, &str)],
) -> std::process::Output {
    let world = PowerShellWorld::new();
    powershell_output_in(&world, exe, prelude, snippet, nonce, how, env)
}

#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "every test child gets a windowless console of its own, never the runner's: \
              the UTF-8 test switches its console's code page, which aterm_uds::spawn's \
              Quiet rule would share with a runner that has a console"
)]
fn powershell_output_in(
    world: &PowerShellWorld,
    exe: &Path,
    prelude: &str,
    snippet: &str,
    nonce: Option<&str>,
    how: Pwsh,
    env: &[(&str, &str)],
) -> std::process::Output {
    use std::os::windows::process::CommandExt;
    /// `CREATE_NO_WINDOW`: a console of the child's own, never shown.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut command = PowerShellWorld::home_guard().to_owned();
    command.push_str(prelude);
    if how != Pwsh::Bare {
        command.push_str(". $env:ATERM_TEST_SCRIPT; ");
    }
    command.push_str(snippet);
    let mut cmd = Command::new(exe);
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
    ])
    .arg(&command)
    .env("ATERM_TEST_SCRIPT", &script)
    .creation_flags(CREATE_NO_WINDOW);
    for var in [
        "ATERM_SHELL_INTEGRATION_INSTALLED",
        "ATERM_DISABLE_PROMPT_TITLES",
        "ATERM_DISABLE_READLINE_KEYS",
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        "PSModulePath",
    ] {
        cmd.env_remove(var);
    }
    if how == Pwsh::InTab {
        cmd.env("ATERM_CHILD", "1");
    }
    match nonce {
        Some(n) => {
            cmd.env("ATERM_SHELL_NONCE", n);
        }
        None => {
            cmd.env_remove("ATERM_SHELL_NONCE");
        }
    }
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.envs(world.env()).current_dir(world.root.path());
    cmd.output()
        .unwrap_or_else(|error| panic!("spawn {exe:?} for integration test: {error}"))
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
    let modes = out
        .find("\u{1b}[?2004l\u{1b}[?2048l")
        .expect("prompt must turn off 2004 and 2048");
    assert!(modes < mark_a, "{out:?}");
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

#[cfg(unix)]
#[test]
fn test_cache_dir_is_the_per_user_cache_outside_the_containment_write_roots() {
    let (base, tail) = match (std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME")) {
        (Some(cache), _) => (PathBuf::from(cache), "aterm/shell-integration"),
        (None, Some(home)) => (PathBuf::from(home), ".cache/aterm/shell-integration"),
        (None, None) => (std::env::temp_dir(), "aterm-shell-integration"),
    };
    assert_eq!(
        per_user_cache_dir(),
        base.join(tail),
        "the per-user cache, whatever the mode"
    );
    if std::env::var_os("XDG_CACHE_HOME").is_none() {
        let root = aterm_uds::test_binary::scratch_root().expect("this test binary's root");
        assert_eq!(cache_dir(), root.join("cache/aterm/shell-integration"));
    }
    let Ok(base) = std::fs::canonicalize(&base) else {
        aterm_spec::host::could_not_run(format!(
            "the cache base {} does not resolve here",
            base.display()
        ));
    };
    let dir = std::fs::canonicalize(base.join(tail)).unwrap_or_else(|_| base.join(tail));
    let roots = aterm_containment::sbpl::WRITABLE_ROOTS;
    if roots.iter().any(|root| base.starts_with(root)) {
        aterm_spec::host::could_not_run(format!(
            "this run's cache base is itself under a temp root: {}",
            base.display()
        ));
    }
    for root in roots {
        assert!(
            !dir.starts_with(root),
            "{} lies under the Containment write root {root}",
            dir.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn test_wsl_launcher_starts_bash_on_the_rcfile_and_any_other_shell_plainly() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let injection = prepare_into(ShellType::Wsl, &dir.path().join("si"))
        .unwrap()
        .unwrap();
    let argv = injection.argv_override.as_deref().expect("wsl argv");
    assert_eq!(argv[..4], ["wsl", "--exec", "/bin/sh", "-c"]);
    let env: std::collections::HashMap<&str, &str> = injection
        .env_add
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert!(env["WSLENV"].starts_with(&merge_wslenv("")), "{env:?}");
    let si_dir = env["ATERM_SHELL_INTEGRATION_DIR"];
    let cwd = dir.path().join("proj");
    std::fs::create_dir(&cwd).unwrap();
    let cwd = std::fs::canonicalize(&cwd).unwrap();
    for (login, expected) in [
        ("bash", format!("--rcfile|{si_dir}/bash/rcfile|-i|")),
        ("zsh", "-l|".to_owned()),
    ] {
        let stand_in = dir.path().join("login").join(login);
        std::fs::create_dir_all(stand_in.parent().unwrap()).unwrap();
        write_executable(
            &stand_in,
            "#!/bin/sh\nprintf '%s|' \"$(pwd -P)\" \"${ATERM_WSL_CWD-unset}\" \"$@\"\n",
        );
        let out = shell_command(&argv[2])
            .args(&argv[3..])
            .envs(injection.env_add.iter().map(|(k, v)| (k, v)))
            .env("SHELL", &stand_in)
            .env(WSL_CWD_VAR, &cwd)
            .output()
            .expect("spawn /bin/sh");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{}|unset|{expected}", cwd.display()),
            "login shell {login}; stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn test_merge_wslenv_is_append_safe_and_idempotent() {
    let ours = "ATERM_SHELL_INTEGRATION_DIR/p:ATERM_SHELL_NONCE:ATERM_WSL_CWD";
    assert_eq!(merge_wslenv(""), ours);
    assert_eq!(
        merge_wslenv("MY_TOOL/p:OTHER"),
        format!("{ours}:MY_TOOL/p:OTHER")
    );
    assert_eq!(merge_wslenv(ours), ours, "idempotent");
    assert_eq!(
        merge_wslenv("ATERM_SHELL_INTEGRATION_DIR:ATERM_SHELL_NONCE/p"),
        ours,
        "stale flags"
    );
}

#[test]
fn test_wsl_cwd_env_only_crosses_a_posix_path() {
    assert_eq!(
        wsl_cwd_env(ShellType::Wsl, Some("/home/you/proj")),
        Some((WSL_CWD_VAR.to_string(), "/home/you/proj".to_string()))
    );
    assert_eq!(wsl_cwd_env(ShellType::Wsl, Some("C:\\Users\\x")), None);
    assert_eq!(wsl_cwd_env(ShellType::Wsl, Some("//server/share")), None);
    assert_eq!(wsl_cwd_env(ShellType::Wsl, None), None);
    assert_eq!(wsl_cwd_env(ShellType::Bash, Some("/home/you/proj")), None);
    assert_eq!(wsl_cwd_env(ShellType::Cmd, Some("/home/you/proj")), None);
}

#[test]
fn test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let mut injection = prepare_into(ShellType::Cmd, &dir.path().join("si"))
        .unwrap()
        .unwrap();
    assert!(
        injection.argv_override.is_none(),
        "the injection is %PROMPT%"
    );
    let nonce = "ab".repeat(32);
    augment_with_nonce(&mut injection, &nonce);
    let env: std::collections::HashMap<&str, &str> = injection
        .env_add
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(env.get("ATERM_SHELL_NONCE"), Some(&nonce.as_str()));
    let prompt = env.get("PROMPT").expect("cmd injection must set PROMPT");
    let at = |needle: &str| {
        prompt
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from {prompt:?}"))
    };
    let order = [
        at("$e]0;$P$e\\"),
        at("$e[?2004l$e[?2048l"),
        at(&format!("$e]133;A;id={nonce}$e\\")),
        at(&format!("$e]633;P;Cwd=$P;id={nonce}$e\\")),
        at("$P$G"),
        at(&format!("$e]133;B;id={nonce}$e\\")),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{prompt}");
    assert!(
        !prompt.contains("]133;C") && !prompt.contains("]133;D"),
        "{prompt}"
    );
}

#[test]
fn test_cmd_prompt_wraps_an_inherited_prompt_once() {
    let own = "$e]0;build box$e\\$P$G";
    let prompt = cmd_prompt(Some(own));
    let ours = prompt.find("$e]0;$P$e\\").expect("our title");
    assert!(
        prompt.find(own).is_some_and(|theirs| ours < theirs),
        "{prompt}"
    );
    assert_eq!(cmd_prompt(Some("")), cmd_prompt(None));
    let outer = cmd_prompt(None);
    assert_eq!(cmd_prompt(Some(&outer)), outer);
}

#[test]
fn test_posix_scripts_are_written_lf_only() {
    assert_eq!(lf_only("a\r\nb\r\n"), "a\nb\n");
    let dir = aterm_tempfile::tempdir().unwrap();
    let base = dir.path().join("si");
    prepare_into(ShellType::Bash, &base).unwrap().unwrap();
    for name in [
        "aterm_shell_integration.bash",
        "aterm_shell_integration.zsh",
        "aterm_shell_integration.fish",
    ] {
        let bytes = std::fs::read(base.join(name)).unwrap();
        assert!(!bytes.contains(&b'\r'), "{name} reached disk with CRs");
    }
}

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
    assert!(!text.contains("Cwd=$P"), "got {text:?}");
    let cwd = text
        .split_once("\x1b]633;P;Cwd=")
        .and_then(|(_, rest)| rest.split_once(";id="))
        .map(|(cwd, _)| cwd)
        .expect("the cwd mark carries the nonce after the path");
    assert!(
        text.contains(&format!("\x1b]0;{cwd}\x1b\\")),
        "cmd must title the tab with its directory {cwd:?}; got {text:?}"
    );
}

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
        aterm_spec::host::could_not_run("no working WSL distro on this host");
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

#[cfg(unix)]
#[test]
fn test_fish_osc7_is_well_formed_with_an_empty_host() {
    let fish = need_fish();
    let (_dir, cwd) = create_special_cwd();
    assert_eq!(
        run_fish_integration(
            fish,
            &[("HOST", "aterm.test")],
            &format!("set -g __aterm_report_host \"\"; cd -- '{}'", cwd.display()),
        ),
        format!(
            "\u{1b}]7;file://{}\u{7}",
            osc7_percent_encode(&cwd.to_string_lossy())
        ),
    );
}

#[cfg(unix)]
#[test]
fn test_fish_prompt_reasserts_its_own_cwd_before_the_prompt_start() {
    let fish = need_fish();
    let (_dir, cwd) = create_special_cwd();
    let own = format!(
        "\u{1b}]7;file://aterm.test{}\u{7}",
        osc7_percent_encode(&cwd.to_string_lossy())
    );
    let forged = "\u{1b}]7;file://h/elsewhere\u{7}";
    let stdout = run_fish_integration(
        fish,
        &[("HOST", "aterm.test"), ("ATERM_DISABLE_PROMPT_TITLES", "1")],
        &format!(
            "cd -- '{}'; function __aterm_original_fish_prompt; printf 'P'; end; \
             printf '{}'; fish_prompt",
            cwd.display(),
            forged.replace('\u{1b}', "\\e").replace('\u{7}', "\\a")
        ),
    );
    let (_, prompt) = stdout
        .split_once(forged)
        .unwrap_or_else(|| panic!("the forged report reached stdout: {stdout:?}"));
    let (before_start, _) = prompt
        .split_once("\u{1b}]133;A")
        .unwrap_or_else(|| panic!("the prompt starts a block: {stdout:?}"));
    let held = before_start
        .rfind("\u{1b}]7;")
        .map(|at| &before_start[at..]);
    assert!(
        held.is_some_and(|report| report.starts_with(&own)),
        "the cwd the host holds at 133;A must be the shell's own {own:?}: {stdout:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_fish_fallback_prompt_survives_a_missing_hostname() {
    let fish = need_fish();
    let body = "functions -e __aterm_original_fish_prompt; \
                function prompt_pwd; printf '~/x'; end; \
                function __aterm_report_cwd; end; \
                set -g __aterm_report_host localhost; \
                set -g USER tester; \
                fish_prompt";
    let out = run_fish_integration(fish, &[("ATERM_DISABLE_PROMPT_TITLES", "1")], body);
    assert_eq!(
        out, "\u{1b}]133;A\u{7}tester@localhost ~/x $ \u{1b}]133;B\u{7}",
        "the fallback prompt must render every segment it can, between the marks"
    );
}

/// Run `report` under `shell` with `$ATERM_TEST_SCRIPT` naming the script `script_name`, in a
/// synthesized environment (no multiplexer's variables but those `env` sets), and return its
/// stdout and stderr.
#[cfg(unix)]
fn run_mux_probe(
    shell: &str,
    args: &[&str],
    script_name: &str,
    runtime_dir: &std::path::Path,
    env: &[(&str, &str)],
    report: &str,
) -> (String, String) {
    let script = format!("{}/src/scripts/{script_name}", env!("CARGO_MANIFEST_DIR"));
    let mut cmd = shell_command(shell);
    cmd.args(args)
        .arg("-c")
        .arg(report)
        .env("ATERM_TEST_SCRIPT", &script)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env_remove("TMUX")
        .env_remove("STY")
        .env_remove("ATERM_MUX")
        .env_remove("ATERM_MUX_BASE")
        .env_remove("ATERM_MUX_OUTER_SESSION_ID")
        .env_remove("ATERM_PARENT_SESSION_ID");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("spawn {shell} for the mux probe: {e}"));
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(unix)]
#[test]
fn test_each_shell_marks_a_multiplexer_pane_and_clears_a_stale_marker() {
    const PANE: (&str, &str) = ("ATERM_SHELL_INTEGRATION_INSTALLED", "1");
    const SESSION: (&str, &str) = ("ATERM_SHELL_INTEGRATION_INSTALLED", "");
    const TMUX: (&str, &str) = ("TMUX", "/tmp/tmux-1000/default,9,0");
    const SCREEN: (&str, &str) = ("STY", "4242.pts-3.host");
    const OUTER: (&str, &str) = ("ATERM_PARENT_SESSION_ID", "s-outer");
    type Case = (
        &'static [(&'static str, &'static str)],
        &'static str,
        Option<&'static str>,
        bool,
    );
    let cases: &[Case] = &[
        (
            &[PANE, OUTER, ("TERM", "screen.xterm-256color"), SCREEN],
            "mux=screen outer=s-outer base=UNSET",
            Some("screen"),
            true,
        ),
        (
            &[PANE, OUTER, ("TERM", "screen.xterm-256color"), SCREEN],
            "mux=screen outer=s-outer base=UNSET",
            None,
            false,
        ),
        (
            &[PANE, OUTER, ("TERM", "screen-256color"), TMUX],
            "mux=tmux outer=s-outer base=UNSET",
            Some("tmux"),
            true,
        ),
        (
            &[
                PANE,
                ("ATERM_PARENT_SESSION_ID", "s-quiet"),
                ("TERM", "screen"),
                ("STY", "55.pts-9.h"),
                ("ATERM_MUX_NOTICE", "0"),
            ],
            "mux=screen outer=s-quiet base=UNSET",
            None,
            false,
        ),
        (
            &[
                PANE,
                OUTER,
                ("TERM", "screen"),
                ("STY", "66.pts-2.h"),
                ("ATERM_MUX", "0"),
            ],
            "mux=0 outer=UNSET base=UNSET",
            None,
            false,
        ),
        (
            &[PANE, ("TERM", "screen"), ("STY", "77.pts-1.h")],
            "mux=screen outer=UNSET base=UNSET",
            None,
            false,
        ),
        (
            &[PANE, OUTER, ("TERM", "xterm-256color")],
            "mux=UNSET outer=UNSET base=UNSET",
            None,
            false,
        ),
        (
            &[
                SESSION,
                ("ATERM_PARENT_SESSION_ID", "s-fresh"),
                ("TERM", "xterm-256color"),
            ],
            "mux=UNSET outer=UNSET base=|",
            None,
            false,
        ),
        (
            &[
                SESSION,
                ("ATERM_PARENT_SESSION_ID", "s-fresh"),
                ("TERM", "xterm-256color"),
                TMUX,
                ("ATERM_MUX", "tmux"),
                ("ATERM_MUX_OUTER_SESSION_ID", "s-stale"),
            ],
            "mux=UNSET outer=UNSET base=/tmp/tmux-1000/default,9,0|",
            None,
            true,
        ),
    ];
    let mut shells = ShellLanes::default();
    let mut lanes: Vec<_> = shells
        .bashes()
        .into_iter()
        .map(|bash| {
            (
                bash,
                &["--noprofile", "--norc", "-i"][..],
                "aterm_shell_integration.bash",
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null; \
                 __aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=; \
                 printf 'mux=%s outer=%s base=%s' \"${ATERM_MUX-UNSET}\" \
                 \"${ATERM_MUX_OUTER_SESSION_ID-UNSET}\" \"${ATERM_MUX_BASE-UNSET}\"",
            )
        })
        .collect();
    if let Some(zsh) = shells.zsh() {
        lanes.push((
            zsh,
            &["-f", "-i"][..],
            "aterm_shell_integration.zsh",
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null; \
             add-zsh-hook -d precmd __aterm_precmd 2>/dev/null || true; \
             add-zsh-hook -d preexec __aterm_preexec 2>/dev/null || true; \
             printf 'mux=%s outer=%s base=%s' \"${ATERM_MUX-UNSET}\" \
             \"${ATERM_MUX_OUTER_SESSION_ID-UNSET}\" \"${ATERM_MUX_BASE-UNSET}\"",
        ));
    }
    if let Some(fish) = shells.fish() {
        lanes.push((
            fish,
            &["-i"][..],
            "aterm_shell_integration.fish",
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null; \
             set -l m UNSET; test -n \"$ATERM_MUX\"; and set m \"$ATERM_MUX\"; \
             set -l o UNSET; test -n \"$ATERM_MUX_OUTER_SESSION_ID\"; and set o \"$ATERM_MUX_OUTER_SESSION_ID\"; \
             printf 'mux=%s outer=%s' \"$m\" \"$o\"",
        ));
    }
    for (shell, args, script_name, report) in lanes {
        let fish = script_name.ends_with(".fish");
        let rt = aterm_tempfile::tempdir().expect("scratch runtime dir");
        for (n, (env, expected, announced, in_fish)) in cases.iter().enumerate() {
            if fish && !in_fish {
                continue;
            }
            let (out, err) = run_mux_probe(shell, args, script_name, rt.path(), env, report);
            let expected = if fish {
                expected.split(" base=").next().unwrap_or(expected)
            } else {
                expected
            };
            assert_eq!(out, expected, "{shell} case {n}; stderr: {err:?}");
            match announced {
                Some(mux) => assert!(
                    err.contains(&format!("inside {mux}")),
                    "{shell} case {n}: announced once: {err:?}"
                ),
                None => assert!(
                    !err.contains("aterm: inside"),
                    "{shell} case {n}: nothing announced: {err:?}"
                ),
            }
        }
    }
}

/// The atpkg hook in `crates/atpkg/src/hooks.rs`'s exact format.
#[cfg(unix)]
const ATPKG_HOOK_POSIX_GOLDEN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/fixtures/atpkg-hook-posix.golden"
));
#[cfg(unix)]
const ATPKG_HOOK_FISH_GOLDEN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/fixtures/atpkg-hook-fish.golden"
));
#[cfg(unix)]
const ATPKG_HOOK_FIXTURE_AGENTS: &str = "/opt/aterm-si fixture/pkg/agents";
#[cfg(unix)]
const ATPKG_HOOK_FIXTURE_BIN: &str = "/opt/aterm-si fixture/pkg/bin";
#[cfg(unix)]
const ATPKG_HOOK_FIXTURE_REROUTE: &str = "/opt/aterm-si fixture/pkg/reroute";

/// The name the reroute stubs' directory carries (`atpkg::reroute::DIR_MARKER_FILE`).
#[cfg(unix)]
const REROUTE_DIR_MARKER: &str = ".atpkg-reroute-dir";

/// One live, interactive shell, driven over pipes or a pty.
#[cfg(unix)]
struct LiveShell {
    label: String,
    child: std::process::Child,
    /// Its stdin pipe, or its pty's master.
    stdin: std::fs::File,
    stdout: std::sync::Arc<Pumped>,
    stderr: std::sync::Arc<Pumped>,
    /// The hermetic home the shell's environment points into, when the
    /// [`ShellCommand`] that made it is gone ([`Self::holding`]).
    home: Option<aterm_tempfile::TempDir>,
    /// An rc's background job, armed before any wait/assertion can unwind.
    job: Option<ShellJob>,
    job_cleanup_started: bool,
}

#[cfg(unix)]
struct ShellJob {
    pid_file: PathBuf,
    root: std::sync::Arc<std::sync::Mutex<Option<aterm_tempfile::TempDir>>>,
    script: Option<OwnedShellPid>,
    /// The same open FIFO the fixture holds, never a reopened pathname.
    release: std::fs::File,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OwnedShellPid {
    pid: i32,
    start: aterm_uds::process::ProcessStart,
}

#[cfg(unix)]
impl OwnedShellPid {
    fn read(pid: u32) -> Option<Self> {
        let pid = i32::try_from(pid).ok().filter(|pid| *pid > 1)?;
        Some(Self {
            pid,
            start: aterm_uds::process::read_process_start(pid)?,
        })
    }

    fn current(self) -> bool {
        Self::read(self.pid as u32) == Some(self)
    }

    fn ended(self) -> bool {
        !aterm_uds::process::pid_alive(self.pid as u32)
            || Self::read(self.pid as u32).is_some_and(|now| now != self)
    }
}

#[cfg(unix)]
impl ShellJob {
    /// A private PID file alone is no signal authority.
    fn owned(&self) -> Result<OwnedShellPid, String> {
        let script = self.script.ok_or("script's start was unreadable")?;
        let text = std::fs::read_to_string(&self.pid_file).map_err(|e| e.to_string())?;
        let job = text
            .trim()
            .parse::<u32>()
            .ok()
            .and_then(OwnedShellPid::read)
            .ok_or("job's positive live PID/start was unreadable")?;
        let mut child = job;
        for _ in 0..8 {
            let parent = aterm_uds::process::read_process_parent(child.pid)
                .and_then(OwnedShellPid::read)
                .ok_or("job's parent was unreadable")?;
            if !child.current() {
                return Err("job ancestry changed".to_owned());
            }
            if parent == script {
                return (script.current() && job.current())
                    .then_some(job)
                    .ok_or_else(|| "job or script identity changed".to_owned());
            }
            child = parent;
        }
        Err("job is not a descendant of this script".to_owned())
    }

    fn preserve(&self) {
        if let Some(root) = self.root.lock().unwrap_or_else(|p| p.into_inner()).take() {
            eprintln!("unconfirmed shell job: kept {}", root.keep().display());
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
fn pump(mut reader: impl std::io::Read + Send + 'static) -> std::sync::Arc<Pumped> {
    let sink = std::sync::Arc::new(Pumped {
        bytes: std::sync::Mutex::new(Vec::new()),
        grew: std::sync::Condvar::new(),
        ended: std::sync::atomic::AtomicBool::new(false),
        eof: std::sync::atomic::AtomicBool::new(false),
    });
    let out = std::sync::Arc::clone(&sink);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    out.eof.store(true, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
                Ok(n) => out
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&buf[..n]),
            }
            out.grew.notify_all();
        }
        out.ended.store(true, std::sync::atomic::Ordering::SeqCst);
        let _held = out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        out.grew.notify_all();
    });
    sink
}

/// A pty's master read as a pipe: its end once no process holds the slave, which Linux
/// answers `EIO`, is an EOF.
#[cfg(unix)]
struct PtyMaster(std::fs::File);

#[cfg(unix)]
impl std::io::Read for PtyMaster {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match std::io::Read::read(&mut self.0, buf) {
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

/// What a [`pump`] has read from one of a shell's pipes.
#[cfg(unix)]
struct Pumped {
    bytes: std::sync::Mutex<Vec<u8>>,
    grew: std::sync::Condvar,
    ended: std::sync::atomic::AtomicBool,
    /// Only a successful zero-byte read proves EOF.
    eof: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl Pumped {
    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Vec<u8>>> {
        self.bytes.lock()
    }
}

#[cfg(unix)]
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(unix)]
impl LiveShell {
    /// A hang detector, never a latency budget.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    /// The shell holds its three pipes and nothing else of this process ([`Self::defaults`]).
    fn spawn(label: &str, cmd: &mut Command) -> Self {
        Self::defaults(cmd);
        detach_session(cmd);
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|error| panic!("{label}: spawn: {error}"));
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = pump(child.stdout.take().expect("piped stdout"));
        let stderr = pump(child.stderr.take().expect("piped stderr"));
        Self::started(
            label,
            child,
            std::fs::File::from(std::os::fd::OwnedFd::from(stdin)),
            stdout,
            stderr,
        )
    }

    /// [`Self::spawn`] on a new pty of its own (aterm-pty's pair), as a tab's shell runs: the
    /// slave its stdin, stdout, stderr and controlling terminal, in a session it leads. The test
    /// writes to and reads the master; its stderr reaches the master with its stdout, so
    /// [`Self::stderr`] reads a pipe nothing holds.
    fn spawn_on_pty(label: &str, cmd: &mut Command) -> Self {
        use std::os::fd::{FromRawFd as _, OwnedFd};
        use std::os::unix::process::CommandExt as _;
        let (master, slave) = aterm_pty::open_pty_pair()
            .unwrap_or_else(|error| panic!("{label}: open a pty: {error}"));
        // SAFETY: the pair just opened, both close-on-exec, each owned here alone.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let end = |fd: &OwnedFd| {
            std::process::Stdio::from(
                fd.try_clone()
                    .unwrap_or_else(|error| panic!("{label}: the pty's slave: {error}")),
            )
        };
        Self::defaults(cmd);
        cmd.stdin(end(&slave))
            .stdout(end(&slave))
            .stderr(end(&slave));
        // SAFETY: between fork and exec, `getsid`, `getpid`, `setsid` and `ioctl`
        // alone, all async-signal-safe: a new session unless already started,
        // the slave on stdin its terminal.
        unsafe {
            cmd.pre_exec(|| {
                if (libc::getsid(0) != libc::getpid() && libc::setsid() < 0)
                    || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .unwrap_or_else(|error| panic!("{label}: spawn: {error}"));
        drop(slave);
        let reader = master
            .try_clone()
            .unwrap_or_else(|error| panic!("{label}: the pty's master: {error}"));
        let stdout = pump(PtyMaster(std::fs::File::from(reader)));
        let (nothing, _) =
            std::io::pipe().unwrap_or_else(|error| panic!("{label}: a pipe: {error}"));
        Self::started(
            label,
            child,
            std::fs::File::from(master),
            stdout,
            pump(nothing),
        )
    }

    /// What every live shell starts with ([`Self::spawn`], [`Self::spawn_on_pty`]): nothing
    /// of this process, the default SIGINT and SIGQUIT, and a fish's completions made
    /// ([`mark_fish_completions_made`]). Its argv goes to the test's captured output, which a
    /// failure prints, so a test that runs more than one bash says which one failed.
    fn defaults(cmd: &mut Command) {
        use aterm_uds::spawnfd::InheritNothing as _;
        use std::os::unix::process::CommandExt as _;
        eprintln!(
            "live shell: {:?}",
            std::iter::once(cmd.get_program())
                .chain(cmd.get_args())
                .collect::<Vec<_>>()
        );
        cmd.inherit_nothing();
        // SAFETY: between fork and exec, `signal` alone, which is
        // async-signal-safe: no allocation, no lock.
        unsafe {
            cmd.pre_exec(|| {
                for sig in [libc::SIGINT, libc::SIGQUIT] {
                    if libc::signal(sig, libc::SIG_DFL) == libc::SIG_ERR {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        mark_fish_completions_made(cmd);
    }

    /// A live shell just spawned: `stdin` what the test writes it, `stdout` and `stderr`
    /// what it has written.
    fn started(
        label: &str,
        child: std::process::Child,
        stdin: std::fs::File,
        stdout: std::sync::Arc<Pumped>,
        stderr: std::sync::Arc<Pumped>,
    ) -> Self {
        Self {
            label: label.to_owned(),
            child,
            stdin,
            stdout,
            stderr,
            home: None,
            job: None,
            job_cleanup_started: false,
        }
    }

    /// Keep `home` — the [`ShellCommand`]'s, which this shell's environment
    /// points into — for as long as this shell is held.
    fn holding(mut self, home: aterm_tempfile::TempDir) -> Self {
        self.home = Some(home);
        self
    }

    /// Retire a positively owned background job before the direct child, on success or unwinding.
    fn stop_owned_job(&mut self) -> Result<(), String> {
        use std::io::Write as _;
        let Some(mut job) = self.job.take() else {
            return Ok(());
        };
        self.job_cleanup_started = true;
        let until = std::time::Instant::now() + Self::TIMEOUT;
        let result = (|| {
            let owned = job.owned()?;
            // SAFETY: positive PID of this still-current child's descendant,
            // with its kernel start checked immediately before this signal.
            // No group/zero/negative PID or pathname-derived executable is signalled.
            if !owned.current() || unsafe { libc::kill(owned.pid, libc::SIGKILL) } != 0 {
                return Err("owned job could not be killed".to_owned());
            }
            job.release
                .write_all(b"go\n")
                .map_err(|e| format!("release the owned shell's hold: {e}"))?;
            self.stdin
                .write_all(format!("wait {}\n", owned.pid).as_bytes())
                .and_then(|()| self.stdin.flush())
                .map_err(|e| format!("ask the owned shell to reap its job: {e}"))?;
            while !owned.ended() {
                if std::time::Instant::now() >= until {
                    return Err(format!("owned shell did not reap its job: {owned:?}"));
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(owned)
        })();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let mut reaped = false;
        loop {
            reaped |= matches!(self.child.try_wait(), Ok(Some(_)));
            let job_ended = result.as_ref().is_ok_and(|owned| owned.ended());
            let pipes_ended = [&self.stdout, &self.stderr]
                .into_iter()
                .all(|pipe| pipe.eof.load(std::sync::atomic::Ordering::SeqCst));
            if reaped && job_ended && pipes_ended {
                return Ok(());
            }
            if std::time::Instant::now() >= until || (reaped && result.is_err()) {
                job.preserve();
                if let Some(home) = self.home.take() {
                    eprintln!("unconfirmed shell job: kept {}", home.keep().display());
                }
                return Err(result.err().unwrap_or_else(|| {
                    format!("cleanup unconfirmed: script_reaped={reaped}, job_ended={job_ended}, pipes_ended={pipes_ended}")
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn stderr_text(&self) -> String {
        let err = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&err).into_owned()
    }

    /// What the shell wrote, under the shell's label (a bash's is its path, so a failure
    /// says which bash it was).
    fn transcript(&self) -> String {
        let out = self
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let err = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        format!(
            "--- {} stdout ---\n{}\n--- stderr ---\n{}",
            self.label,
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        )
    }

    fn send(&mut self, line: &str) {
        use std::io::Write as _;
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| self.stdin.flush())
            .unwrap_or_else(|error| {
                panic!(
                    "{}: write {line:?}: {error}\n{}",
                    self.label,
                    self.transcript()
                )
            });
    }

    /// Wait until `marker` appears in stdout at or after byte offset `from`.
    fn wait_for_after(&mut self, marker: &str, from: usize) -> usize {
        let pipe = std::sync::Arc::clone(&self.stdout);
        self.wait_on(&pipe, &format!("{marker:?}"), |out| {
            find_bytes(&out[from.min(out.len())..], marker.as_bytes())
                .map(|pos| from + pos + marker.len())
        })
    }

    /// Wait until `found` finds what it looks for in what `pipe` has read so far.
    fn wait_on<T>(&mut self, pipe: &Pumped, what: &str, found: impl Fn(&[u8]) -> Option<T>) -> T {
        let started = std::time::Instant::now();
        let mut out = pipe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(found) = found(&out) {
                return found;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                drop(out);
                panic!(
                    "{}: shell exited ({status}) before {what} appeared\n{}",
                    self.label,
                    self.transcript()
                );
            }
            if started.elapsed() >= Self::TIMEOUT {
                drop(out);
                panic!(
                    "{}: timed out waiting for {what}\n{}",
                    self.label,
                    self.transcript()
                );
            }
            out = pipe
                .grew
                .wait_timeout(out, std::time::Duration::from_millis(15))
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Wait for the shell to exit and reap it.
    fn wait_exit_status(&mut self, what: &str) -> std::process::ExitStatus {
        let started = std::time::Instant::now();
        let pipe = std::sync::Arc::clone(&self.stdout);
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status;
            }
            if started.elapsed() >= Self::TIMEOUT {
                let _ = self.child.kill();
                panic!("{}: {what}\n{}", self.label, self.transcript());
            }
            let out = pipe
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let tick = if pipe.ended.load(std::sync::atomic::Ordering::SeqCst) {
                std::time::Duration::from_millis(1)
            } else {
                std::time::Duration::from_millis(15)
            };
            drop(
                pipe.grew
                    .wait_timeout(out, tick)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        }
    }

    /// The text after the last `key` before `end`.
    fn value_before(&self, key: &str, end: usize) -> String {
        let out = self
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = String::from_utf8_lossy(&out[..end.min(out.len())]);
        text.rsplit(key)
            .next()
            .unwrap_or("")
            .chars()
            .take_while(|c| !c.is_control())
            .collect()
    }

    fn finish(mut self) {
        if self.job.is_some() {
            self.stop_owned_job().expect("owned background job retired");
            return;
        }
        self.send("exit");
        self.wait_exit_status("did not exit after `exit`");
    }
}

/// A shell its test lets go of, at its end or in a failed assertion's unwinding.
#[cfg(unix)]
impl Drop for LiveShell {
    fn drop(&mut self) {
        let owns_job = self.job.is_some() || self.job_cleanup_started;
        if let Err(error) = self.stop_owned_job() {
            eprintln!("{}: {error}", self.label);
        }
        if owns_job {
            // Its bounded cleanup already reaped the direct child, or kept
            // every directory on an unconfirmed result. Do not enter a second,
            // unbounded wait during unwinding.
            return;
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A user's machine in a temp dir.
#[cfg(unix)]
struct LiveFixture {
    _dir: aterm_tempfile::TempDir,
    base: std::path::PathBuf,
    home: std::path::PathBuf,
    foreign: std::path::PathBuf,
    agents: std::path::PathBuf,
    reroute: std::path::PathBuf,
    bin: std::path::PathBuf,
}

#[cfg(unix)]
fn write_executable(path: &Path, body: &str) {
    aterm_tempfile::stand_in(path, body)
        .unwrap_or_else(|error| panic!("stand-in {}: {error}", path.display()));
}

#[cfg(unix)]
impl LiveFixture {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        let foreign = root.join("foreign");
        let prefix = root.join("Application Support").join("pkg");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&foreign).expect("foreign");
        write_executable(&foreign.join("claude"), "#!/bin/sh\necho foreign\n");
        Self {
            base: root.join("si"),
            home,
            foreign,
            agents: prefix.join("agents"),
            reroute: prefix.join("reroute"),
            bin: prefix.join("bin"),
            _dir: dir,
        }
    }

    fn shell_d(&self) -> std::path::PathBuf {
        self.home.join(".aterm").join("shell.d")
    }

    fn hook_body(&self, golden: &str) -> String {
        golden
            .replace(
                ATPKG_HOOK_FIXTURE_AGENTS,
                self.agents.to_str().expect("UTF-8"),
            )
            .replace(ATPKG_HOOK_FIXTURE_BIN, self.bin.to_str().expect("UTF-8"))
            .replace(
                ATPKG_HOOK_FIXTURE_REROUTE,
                self.reroute.to_str().expect("UTF-8"),
            )
    }

    /// Atpkg's pass, as the running shell sees it.
    fn lay(&self, hook_ext: &str, golden: &str, twin_says: &str) {
        self.lay_dirs();
        self.lay_twin(twin_says);
        self.lay_hook(hook_ext, golden);
    }

    /// The three managed dirs (agents/ empty, no twin yet) and the reroute marker.
    fn lay_dirs(&self) {
        std::fs::create_dir_all(&self.agents).expect("agents");
        std::fs::create_dir_all(&self.reroute).expect("reroute");
        std::fs::create_dir_all(&self.bin).expect("bin");
        std::fs::write(self.reroute.join(REROUTE_DIR_MARKER), "").expect("marker");
    }

    /// The hook alone, in hooks.rs's exact format.
    fn lay_hook(&self, ext: &str, golden: &str) {
        self.write_hook(ext, &self.hook_body(golden));
    }

    /// The twin, atomically (`stand_in` lays it by temp + rename).
    fn lay_twin(&self, says: &str) {
        write_executable(
            &self.agents.join("claude"),
            &format!("#!/bin/sh\necho {says}\n"),
        );
    }

    /// The seam's reroute dir with its marker and one stub in it (`aterm help reroute`).
    fn lay_reroute_stub(&self, name: &str, says: &str) {
        std::fs::create_dir_all(&self.reroute).expect("reroute");
        std::fs::write(self.reroute.join(REROUTE_DIR_MARKER), "").expect("marker");
        write_executable(
            &self.reroute.join(name),
            &format!("#!/bin/sh\necho {says}\n"),
        );
    }

    fn write_hook(&self, ext: &str, body: &str) {
        let shell_d = self.shell_d();
        std::fs::create_dir_all(&shell_d).expect("shell.d");
        let tmp = shell_d.join(format!(".00-atpkg.{ext}.tmp"));
        std::fs::write(&tmp, body).expect("hook tmp");
        std::fs::rename(&tmp, shell_d.join(format!("00-atpkg.{ext}"))).expect("rename hook");
    }

    fn assert_path_order(&self, label: &str, path: &str) {
        let (reroute, agents, bin) = (
            self.reroute.to_str().expect("UTF-8"),
            self.agents.to_str().expect("UTF-8"),
            self.bin.to_str().expect("UTF-8"),
        );
        assert!(
            path.starts_with(&format!("{reroute}:{agents}:")),
            "{label}: PATH must lead with reroute then agents: {path:?}"
        );
        assert!(
            path.ends_with(&format!(":{bin}")),
            "{label}: the managed bin/ must stay LAST: {path:?}"
        );
        for (name, dir) in [("reroute", reroute), ("agents", agents), ("bin", bin)] {
            assert_eq!(
                path.split(':').filter(|e| *e == dir).count(),
                1,
                "{label}: {name} exactly once: {path:?}"
            );
        }
        assert!(
            path.split(':')
                .any(|e| e == self.foreign.to_str().expect("UTF-8")),
            "{label}: the foreign dir is demoted, not removed: {path:?}"
        );
    }
}

/// An interactive zsh through the crate's own zdotdir wrapper, reading the fixture home's .zshrc.
#[cfg(unix)]
fn spawn_live_zsh(
    zsh: &str,
    fx: &LiveFixture,
    rc_prelude: &str,
    extra: &[(&str, &str)],
) -> LiveShell {
    std::fs::write(
        fx.home.join(".zshrc"),
        format!(
            "{rc_prelude}export PATH=\"{}:$PATH\"\n",
            fx.foreign.display()
        ),
    )
    .expect(".zshrc");
    let InjectionEnv { env_add, .. } = prepare_into(ShellType::Zsh, &fx.base)
        .expect("prepare")
        .expect("zsh injection");
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

/// An interactive `bash` through the crate's own `--rcfile` wrapper.
#[cfg(unix)]
fn spawn_live_bash(
    bash: &str,
    fx: &LiveFixture,
    rc_prelude: &str,
    extra: &[(&str, &str)],
) -> LiveShell {
    std::fs::write(
        fx.home.join(".bashrc"),
        format!(
            "{rc_prelude}export PATH=\"{}:$PATH\"\n",
            fx.foreign.display()
        ),
    )
    .expect(".bashrc");
    let InjectionEnv {
        env_add,
        argv_override,
    } = prepare_into(ShellType::Bash, &fx.base)
        .expect("prepare")
        .expect("bash injection");
    let argv = argv_override.expect("bash uses --rcfile");
    let mut cmd = shell_command(bash);
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
    LiveShell::spawn(bash, &mut cmd).holding(cmd.into_home())
}

/// A home shaped like Ubuntu's `/etc/skel`.
#[cfg(all(unix, not(target_os = "macos")))]
fn skel_shaped_home(fx: &LiveFixture) {
    for (name, body) in [
        (".bashrc", "echo RAN-BASHRC\n"),
        (".profile", "echo RAN-PROFILE\n. \"$HOME/.bashrc\"\n"),
        (".bash_logout", "echo LOGGED-OUT >\"$HOME/logout-mark\"\n"),
    ] {
        std::fs::write(fx.home.join(name), body).expect("a startup file");
    }
}

/// `bash` as aterm starts it with `shell_args` = `args` ([`merge_shell_args`]).
#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_bash_with_shell_args(bash: &str, fx: &LiveFixture, args: &[&str]) -> LiveShell {
    let mut injection = prepare_into(ShellType::Bash, &fx.base)
        .expect("prepare")
        .expect("bash injection");
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let home = fx.home.display().to_string();
    let env = |key: &str| (key == "HOME").then(|| home.clone());
    assert_eq!(
        shell_args::merge_shell_args_in(ShellType::Bash, &mut injection, &args, &env),
        ArgsMerge::Integrated
    );
    let argv = injection.argv_override.expect("bash's argv");
    let mut cmd = shell_command(bash);
    cmd.args(&argv[1..])
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1")
        .env_remove("ENV")
        .env_remove("HISTFILE");
    for (key, value) in injection.env_add {
        cmd.env(key, value);
    }
    LiveShell::spawn(bash, &mut cmd).holding(cmd.into_home())
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn bash_reads_each_startup_file_once_non_login_and_login() {
    let probe = "echo \"LOGIN=$(shopt -q login_shell && echo y || echo n) \
                 POSIX=$(shopt -qo posix && echo y || echo n) \
                 HOOKED=${ATERM_SHELL_INTEGRATION_INSTALLED:+y}\"; echo MARK$((40 + 2))";
    let count =
        |sh: &LiveShell, at: usize, what: &str| stdout_between(sh, 0, at).matches(what).count();

    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        skel_shaped_home(&fx);
        let mut sh = spawn_bash_with_shell_args(bash, &fx, &["-i"]);
        sh.send(probe);
        let at = sh.wait_for_after("MARK42", 0);
        assert_eq!(count(&sh, at, "RAN-BASHRC"), 1, "{}", sh.transcript());
        assert_eq!(count(&sh, at, "RAN-PROFILE"), 0, "{}", sh.transcript());
        assert_eq!(
            sh.value_before("LOGIN=", at),
            "n POSIX=n HOOKED=y",
            "{bash}"
        );
        sh.finish();

        let fx = LiveFixture::new();
        skel_shaped_home(&fx);
        let mut sh = spawn_bash_with_shell_args(bash, &fx, &["-l", "-i"]);
        sh.send(probe);
        let at = sh.wait_for_after("MARK42", 0);
        assert_eq!(count(&sh, at, "RAN-BASHRC"), 1, "{}", sh.transcript());
        assert_eq!(count(&sh, at, "RAN-PROFILE"), 1, "{}", sh.transcript());
        assert_eq!(
            sh.value_before("LOGIN=", at),
            "y POSIX=n HOOKED=y",
            "{bash}"
        );
        sh.send("logout");
        sh.wait_exit_status("`logout` did not end the login shell");
        assert_eq!(
            std::fs::read_to_string(fx.home.join("logout-mark"))
                .ok()
                .as_deref(),
            Some("LOGGED-OUT\n"),
            "~/.bash_logout ran\n{}",
            sh.transcript()
        );

        let fx = LiveFixture::new();
        skel_shaped_home(&fx);
        std::fs::write(fx.home.join(".myrc"), "echo RAN-MYRC\n").expect(".myrc");
        let mut sh = spawn_bash_with_shell_args(bash, &fx, &["--rcfile", "~/.myrc", "-i"]);
        sh.send(probe);
        let at = sh.wait_for_after("MARK42", 0);
        assert_eq!(count(&sh, at, "RAN-MYRC"), 1, "{}", sh.transcript());
        assert_eq!(count(&sh, at, "RAN-BASHRC"), 0, "{}", sh.transcript());
        assert_eq!(
            sh.value_before("LOGIN=", at),
            "n POSIX=n HOOKED=y",
            "{bash}"
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn test_zsh_loads_zstat_only_once_there_is_a_hook_to_stamp() {
    let zsh = need_zsh();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.zsh",
        env!("CARGO_MANIFEST_DIR")
    );
    let scenario = "\
source \"$ATERM_TEST_SCRIPT\"\n\
print -rn -- \"[at-load=${+builtins[zstat]}]\"\n\
__aterm_managed_path_live\n\
print -rn -- \"[no-hook=${+builtins[zstat]}]\"\n\
mkdir -p \"$HOME/.aterm/shell.d\"\n\
print -r -- 'export ATERM_TEST_HOOK_RAN=1' >\"$HOME/.aterm/shell.d/00-atpkg.zsh\"\n\
__aterm_managed_path_live\n\
print -rn -- \"[hook=${+builtins[zstat]} ran=${ATERM_TEST_HOOK_RAN:-0}]\"\n";
    let refuse =
        "zmodload() { [[ \"$*\" == *zsh/stat* ]] && return 1; builtin zmodload \"$@\"; }\n";
    for (label, prefix, expected) in [
        ("loads", "", "[at-load=0][no-hook=0][hook=1 ran=1]"),
        ("refused", refuse, "[at-load=0][no-hook=0][hook=0 ran=1]"),
    ] {
        let output = shell_command(zsh)
            .args(["-f", "-i", "-c", &format!("{prefix}{scenario}")])
            .env("ATERM_TEST_SCRIPT", &script)
            .env("ATERM_CHILD", "1")
            .output()
            .unwrap_or_else(|error| panic!("spawn zsh for the zstat load scenario: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "{label}: stdout: {stdout:?}; stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains(expected),
            "{label}: zsh/stat loads with the first hook, and that hook is sourced: {stdout:?}"
        );
    }
}
/// An interactive fish inside a session.
#[cfg(unix)]
fn spawn_live_fish(fish: &str, fx: &LiveFixture, extra: &[(&str, &str)]) -> LiveShell {
    let config = fx.home.join("config").join("fish");
    std::fs::create_dir_all(&config).expect("config.fish dir");
    std::fs::write(
        config.join("config.fish"),
        format!("set -gx PATH \"{}\" $PATH\n", fx.foreign.display()),
    )
    .expect("config.fish");
    let InjectionEnv { env_add, .. } = prepare_into(ShellType::Fish, &fx.base)
        .expect("prepare")
        .expect("fish injection");
    let mut cmd = pty_command(&[fish, "-i"]);
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
    for (key, value) in env_add {
        if key == "XDG_DATA_DIRS" {
            cmd.env(
                key,
                format!(
                    "{}:{}",
                    fx.base.join("fish-xdg").display(),
                    hermetic_data(hermetic.home()).display()
                ),
            );
        } else {
            cmd.env(key, value);
        }
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn_on_pty("fish", &mut cmd).holding(hermetic.into_home())
}

#[cfg(unix)]
const ZSH_PRINT_PATH: &str = "print -r -- \"LIVEPATH=$PATH\"; print -r -- \"LIVERR=$ATERM_REROUTE_DIR\"; print -r -- \"MARK$((40+2))\"";
#[cfg(unix)]
const BASH_PRINT_PATH: &str = "printf 'LIVEPATH=%s\\n' \"$PATH\"; printf 'LIVERR=%s\\n' \"$ATERM_REROUTE_DIR\"; echo \"MARK$((40+2))\"";
#[cfg(unix)]
const FISH_PRINT_PATH: &str = "printf 'LIVEPATH=%s\\n' (string join : -- $PATH); printf 'LIVERR=%s\\n' \"$ATERM_REROUTE_DIR\"; echo MARK(math 40 + 2)";

/// The command for `argv`, to run on a pty of its own with [`LiveShell::spawn_on_pty`]
/// (aterm-pty's pair, never `script(1)`, whose BSD and util-linux forms differ).
#[cfg(unix)]
fn pty_command(argv: &[&str]) -> Command {
    let (program, args) = argv.split_first().expect("a program to run");
    let mut cmd = Command::new(program);
    cmd.args(args);
    cmd
}

/// A shell [`LiveShell::spawn_on_pty`] starts has the pty for its three standard descriptors and
/// its controlling terminal, and leads its process group. A non-interactive `sh` opens no
/// terminal of its own (an interactive one's open, as session leader, would take it), so its
/// reaching the master through `/dev/tty` pins the spawn's `TIOCSCTTY` on Linux, which hands a
/// session a terminal only on that ioctl or an open; macOS gives the session the pty without it.
/// The probe never names the terminal: `ttyname` and `ps -o tty` walk `/dev`, which can miss a
/// pty while other tests' come and go. Its markers are computed because the pty echoes the probe
/// as typed, and its scratch `HOME` takes the history an interactive `sh` writes at exit. The
/// same shell on pipes has no terminal at all.
#[cfg(unix)]
#[test]
fn a_shell_on_its_pty_has_it_for_its_terminal() {
    const TERMINALS: &str = "[ -t 0 ] && [ -t 1 ] && [ -t 2 ] && echo TERMINALS=$((1+2))";
    const PROBE: &str = "echo STDERR=$((1+1)) >&2; echo CTTY=$((0+1)) >/dev/tty; \
        /bin/ps -o pid= -o pgid= -p $$; echo \"HOME=$HOME\"; echo MARK$((40+2))";
    let mut cmd = shell_command("/bin/sh");
    cmd.arg("-i");
    let home = cmd.home().to_path_buf();
    let mut on_pty = LiveShell::spawn_on_pty("sh", &mut cmd).holding(cmd.into_home());
    on_pty.send(&format!("{TERMINALS}; {PROBE}"));
    let at = on_pty.wait_for_after("MARK42", 0);
    let seen = {
        let text = on_pty
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&text[..at]).into_owned()
    };
    // A line may follow the prompt: the probe is typed before the shell prints its first, so
    // the pty echoes it ahead of the prompt, and the prompt then leads the probe's first line
    // ("$ TERMINALS=3", the Linux lane's dash on a loaded host).
    let printed = |line: &str| {
        seen.lines()
            .any(|l| l.trim().rsplit("$ ").next() == Some(line))
    };
    assert!(
        printed("TERMINALS=3"),
        "fds 0, 1 and 2 are terminals\n{seen}"
    );
    assert!(printed("STDERR=2"), "its stderr is the pty\n{seen}");
    assert!(printed("CTTY=1"), "its /dev/tty is the pty\n{seen}");
    let pid = on_pty.child.id().to_string();
    assert!(
        seen.lines()
            .any(|l| l.split_whitespace().collect::<Vec<_>>() == [pid.as_str(), pid.as_str()]),
        "the shell leads its process group\n{seen}"
    );
    assert!(
        printed(&format!("HOME={}", home.display())),
        "the shell's HOME is the scratch one\n{seen}"
    );
    assert_ne!(
        std::env::var_os("HOME").as_deref(),
        Some(home.as_os_str()),
        "the scratch HOME is not this process's"
    );
    on_pty.finish();
    let mut cmd = shell_command("/bin/sh");
    cmd.args([
        "-c",
        "echo CTTY=$((0+1)) >/dev/tty; echo MARK$((40+2)); read line",
    ]);
    let mut quiet = LiveShell::spawn_on_pty("sh -c", &mut cmd).holding(cmd.into_home());
    let at = quiet.wait_for_after("MARK42", 0);
    let seen = {
        let text = quiet
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&text[..at]).into_owned()
    };
    assert!(
        seen.lines().any(|l| l.trim() == "CTTY=1"),
        "a shell that opens no terminal has the pty for its own\n{seen}"
    );
    quiet.finish();
    // On pipes the shell leads a fresh session with no controlling terminal, so `/dev/tty` fails.
    let mut cmd = shell_command("/bin/sh");
    let mut on_pipes = LiveShell::spawn("sh", &mut cmd).holding(cmd.into_home());
    on_pipes.send(&format!(
        "{TERMINALS} || echo TERMINALS=0; \
         (echo CTTY=$((0+1)) >/dev/tty) 2>/dev/null || echo NO_CTTY=$((0+1)); \
         echo MARK$((40+2))"
    ));
    let at = on_pipes.wait_for_after("MARK42", 0);
    let seen = {
        let text = on_pipes
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&text[..at]).into_owned()
    };
    assert!(seen.lines().any(|l| l.trim() == "TERMINALS=0"), "{seen}");
    assert!(seen.lines().any(|l| l.trim() == "NO_CTTY=1"), "{seen}");
    assert!(!seen.lines().any(|l| l.trim() == "CTTY=1"), "{seen}");
    on_pipes.finish();
}

#[cfg(unix)]
#[test]
fn piped_interactive_shells_in_a_background_group_neither_stop_nor_touch_the_tty() {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    use std::os::unix::process::CommandExt as _;

    const STAGE_VAR: &str = "ATERM_TEST_BG_TTY_STAGE";
    const TEST_NAME: &str =
        "tests::piped_interactive_shells_in_a_background_group_neither_stop_nor_touch_the_tty";

    match std::env::var(STAGE_VAR).ok().as_deref() {
        Some("bg") => {
            assert!(
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/tty")
                    .is_ok(),
                "background test group must inherit a controlling /dev/tty to exercise the hazard"
            );
            let mut shells = ShellLanes::default();
            let lanes = posix_lanes(&mut shells);
            let bashes = shells.bashes();
            shells.absent.clear();
            let script = format!(
                "{}/src/scripts/aterm_shell_integration",
                env!("CARGO_MANIFEST_DIR")
            );
            for (shell, args, file, _) in lanes {
                let out = shell_command(shell)
                    .args(args)
                    .args([
                        "-c",
                        &format!(
                            "source \"{script}.{ext}\"; echo OK$((40+2))",
                            ext = file.rsplit('.').next().unwrap_or("sh")
                        ),
                    ])
                    .env("ATERM_CHILD", "1")
                    .output()
                    .unwrap_or_else(|e| panic!("{shell}: {e}"));
                let stdout = String::from_utf8_lossy(&out.stdout);
                assert!(
                    out.status.success() && stdout.contains("OK42"),
                    "{shell}: status={:?} stdout={stdout:?} stderr={:?}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            for bash in bashes {
                let fx = LiveFixture::new();
                let mut sh = spawn_live_bash(bash, &fx, "", &[("ATERM_SHELL_NONCE", REKEY_OLD)]);
                sh.send("echo LIVE_BG$((40+2))");
                sh.wait_for_after("LIVE_BG42", 0);
                sh.finish();
            }
            return;
        }
        Some("leader") => {
            let exe = std::env::current_exe().expect("test binary");
            let mut cmd = Command::new(exe);
            cmd.inherit_nothing();
            cmd.args(["--exact", TEST_NAME, "--nocapture"])
                .env(STAGE_VAR, "bg")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .process_group(0);
            let mut child = cmd.spawn().expect("spawn background group");
            let out = pump(child.stdout.take().expect("bg stdout"));
            let err = pump(child.stderr.take().expect("bg stderr"));
            let started = std::time::Instant::now();
            let status = loop {
                if let Some(status) = child.try_wait().expect("try_wait bg") {
                    break status;
                }
                if started.elapsed() >= LiveShell::TIMEOUT {
                    if let Ok(pgid) = i32::try_from(child.id()) {
                        // SAFETY: `child` was spawned with `process_group(0)` and has not been
                        // reaped, so `-pgid` names its process group alone.
                        unsafe {
                            libc::kill(-pgid, libc::SIGKILL);
                        }
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "background test group hung or stopped on /dev/tty:\nstdout:\n{}\nstderr:\n{}",
                        String::from_utf8_lossy(
                            &out.lock().unwrap_or_else(PoisonError::into_inner)
                        ),
                        String::from_utf8_lossy(
                            &err.lock().unwrap_or_else(PoisonError::into_inner)
                        ),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(15));
            };
            assert!(
                status.success(),
                "background test group failed ({status}):\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out.lock().unwrap_or_else(PoisonError::into_inner)),
                String::from_utf8_lossy(&err.lock().unwrap_or_else(PoisonError::into_inner)),
            );
            return;
        }
        Some(other) => panic!("unexpected {STAGE_VAR}={other}"),
        None => {}
    }

    let mut shells = ShellLanes::default();
    let _ = posix_lanes(&mut shells);

    let (master, slave) = aterm_pty::open_pty_pair().expect("open pty");
    // SAFETY: `open_pty_pair` returned two fresh, close-on-exec descriptors owned here alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let exe = std::env::current_exe().expect("test binary");
    let mut cmd = Command::new(exe);
    cmd.inherit_nothing();
    cmd.args(["--exact", TEST_NAME, "--nocapture"])
        .env(STAGE_VAR, "leader")
        .stdin(std::process::Stdio::from(slave))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SAFETY: between fork and exec, `setsid` and `ioctl` alone, both async-signal-safe:
    // `leader` owns a dedicated pty session whose slave is fd 0.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut leader = cmd.spawn().expect("spawn session leader");
    let pty_out = pump(PtyMaster(std::fs::File::from(master)));
    let leader_out = pump(leader.stdout.take().expect("leader stdout"));
    let leader_err = pump(leader.stderr.take().expect("leader stderr"));
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = leader.try_wait().expect("try_wait leader") {
            break status;
        }
        if started.elapsed() >= LiveShell::TIMEOUT {
            let _ = leader.kill();
            let _ = leader.wait();
            panic!(
                "pty session leader timed out:\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&leader_out.lock().unwrap_or_else(PoisonError::into_inner)),
                String::from_utf8_lossy(&leader_err.lock().unwrap_or_else(PoisonError::into_inner)),
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    };
    assert!(
        status.success(),
        "pty session leader failed ({status}):\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&leader_out.lock().unwrap_or_else(PoisonError::into_inner)),
        String::from_utf8_lossy(&leader_err.lock().unwrap_or_else(PoisonError::into_inner)),
    );
    while !pty_out.ended.load(std::sync::atomic::Ordering::SeqCst)
        && started.elapsed() < LiveShell::TIMEOUT
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let leaked = pty_out
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert!(
        leaked.is_empty(),
        "piped shells leaked bytes onto the caller's controlling /dev/tty: {:?}",
        String::from_utf8_lossy(&leaked)
    );
}

/// [`zsh_shell`] for a test that needs a real zsh.
#[cfg(unix)]
fn need_zsh() -> &'static str {
    zsh_shell()
        .unwrap_or_else(|| aterm_spec::host::could_not_run(aterm_spec::host::tool("zsh").refusal()))
}

/// Absolute path to a PowerShell 7 `pwsh`, or `None` where the host has none.
#[cfg(unix)]
fn pwsh_shell() -> Option<&'static str> {
    static PWSH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    interned_test_shell(&PWSH, "pwsh")
}

/// [`pwsh_shell`] for a test that needs a real pwsh.
#[cfg(unix)]
fn need_pwsh() -> &'static str {
    pwsh_shell().unwrap_or_else(|| {
        aterm_spec::host::could_not_run(aterm_spec::host::tool("pwsh").refusal())
    })
}

/// [`fish_shell`] for a test that needs a real fish.
#[cfg(unix)]
fn need_fish() -> &'static str {
    fish_shell().unwrap_or_else(|| {
        aterm_spec::host::could_not_run(aterm_spec::host::tool("fish").refusal())
    })
}

/// A live `program` of `dialect` ("bash", "zsh" or "fish": a lane of [`ShellLanes::lanes`])
/// inside a session, with no rc prelude, and the line that prints its PATH.
#[cfg(unix)]
fn spawn_live(
    (dialect, program): (&str, &str),
    fx: &LiveFixture,
    extra: &[(&str, &str)],
) -> (LiveShell, &'static str) {
    match dialect {
        "bash" => (spawn_live_bash(program, fx, "", extra), BASH_PRINT_PATH),
        "zsh" => (spawn_live_zsh(program, fx, "", extra), ZSH_PRINT_PATH),
        _ => (spawn_live_fish(program, fx, extra), FISH_PRINT_PATH),
    }
}

/// Print PATH and `$ATERM_REROUTE_DIR` in the live shell, from `at`.
#[cfg(unix)]
fn assert_live_path(label: &str, sh: &mut LiveShell, fx: &LiveFixture, at: usize, print: &str) {
    sh.send(print);
    let at = sh.wait_for_after("MARK42", at);
    fx.assert_path_order(label, &sh.value_before("LIVEPATH=", at));
    assert_eq!(
        sh.value_before("LIVERR=", at),
        fx.reroute.to_str().expect("UTF-8"),
        "{label}: ATERM_REROUTE_DIR derived beside agents/"
    );
}

/// The owner's scenario in one live bash or zsh (`label`, the hook's extension): `claude` runs the
/// foreign one, then the twin once atpkg's pass lays it and its hook, then a re-laid twin, and a
/// rewritten hook takes effect.
#[cfg(unix)]
fn posix_owner_scenario(label: &str, sh: &mut LiveShell, fx: &LiveFixture, print: &str) {
    sh.send("claude");
    let at = sh.wait_for_after("foreign", 0);
    sh.send("echo IDLE-$((1+0))");
    let at = sh.wait_for_after("IDLE-1", at);
    let at = sh.wait_for_after("\x1b]133;A", at);
    fx.lay(label, ATPKG_HOOK_POSIX_GOLDEN, "managed 1");
    sh.send("claude");
    let at = sh.wait_for_after("managed 1", at);
    fx.lay_twin("managed 2");
    sh.send("claude");
    let at = sh.wait_for_after("managed 2", at);
    fx.write_hook(
        label,
        &format!(
            "{}export ATPKG_HOOK_GEN=2\n",
            fx.hook_body(ATPKG_HOOK_POSIX_GOLDEN)
        ),
    );
    sh.send("echo \"GEN=${ATPKG_HOOK_GEN:-unset}\"");
    let at = sh.wait_for_after("GEN=2", at);
    assert_live_path(label, sh, fx, at, print);
}

#[cfg(unix)]
#[test]
fn test_zsh_already_running_session_shell_picks_up_the_managed_dirs_live() {
    let zsh = need_zsh();
    let spawn = |fx: &LiveFixture, extra: &[(&str, &str)]| spawn_live_zsh(zsh, fx, "", extra);

    let fx = LiveFixture::new();
    let mut sh = spawn(&fx, &[("ATERM_CHILD", "1")]);
    posix_owner_scenario("zsh", &mut sh, &fx, ZSH_PRINT_PATH);
    sh.finish();

    let fx = LiveFixture::new();
    let mut sh = spawn(&fx, &[]);
    sh.send("claude");
    let at = sh.wait_for_after("foreign", 0);
    fx.lay("zsh", ATPKG_HOOK_POSIX_GOLDEN, "managed 1");
    sh.send("claude; print -r -- \"MARK$((40+2))\"");
    let at = sh.wait_for_after("MARK42", at);
    let out = sh.value_before("\x1b]133;C\x07", at);
    assert!(
        out.starts_with("foreign") && !out.contains("managed"),
        "zsh: outside a session the live re-assert must be inert: {out:?}"
    );
    sh.finish();

    let fx = LiveFixture::new();
    let mut sh = spawn(
        &fx,
        &[("ATERM_CHILD", "1"), ("__ATERM_REROUTE_PASSTHROUGH", "1")],
    );
    sh.send("claude");
    let at = sh.wait_for_after("foreign", 0);
    fx.lay("zsh", ATPKG_HOOK_POSIX_GOLDEN, "managed 1");
    sh.send("claude");
    let at = sh.wait_for_after("managed 1", at);
    sh.send("print -r -- \"LIVEPATH=$PATH\"; print -r -- \"LIVERR=${ATERM_REROUTE_DIR:-unset}\"; print -r -- \"MARK$((40+2))\"");
    let at = sh.wait_for_after("MARK42", at);
    let path = sh.value_before("LIVEPATH=", at);
    assert!(
        path.starts_with(&format!("{}:", fx.agents.display())),
        "zsh: agents/ leads without the reroute dir: {path:?}"
    );
    assert!(
        !path.contains(fx.reroute.to_str().expect("UTF-8")),
        "zsh: no reroute dir on PATH under __ATERM_REROUTE_PASSTHROUGH=1: {path:?}"
    );
    assert_eq!(sh.value_before("LIVERR=", at), "unset");
    sh.finish();
}

#[cfg(unix)]
#[test]
fn test_bash_live_pickup_holds_under_set_u() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let mut sh = spawn_live_bash(bash, &fx, "set -u\n", &[]);
        posix_owner_scenario("bash", &mut sh, &fx, BASH_PRINT_PATH);
        let err = sh.stderr_text();
        assert!(
            !err.contains("unbound variable"),
            "{bash}: the integration must be nounset-clean at load and at every prompt:\n{err}"
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn test_reroute_stubs_survive_a_prompt_time_prepend_with_no_hook() {
    let mut shells = ShellLanes::default();
    for lane @ (label, program) in shells.lanes(&["bash", "zsh", "fish"]) {
        let fx = LiveFixture::new();
        fx.lay_reroute_stub("cargo", "stub-cargo");
        write_executable(&fx.foreign.join("cargo"), "#!/bin/sh\necho foreign-cargo\n");
        let reroute = fx.reroute.to_str().expect("UTF-8");
        let path = format!("{reroute}:/usr/bin:/bin");
        let env = [
            ("ATERM_CHILD", "1"),
            ("ATERM_REROUTE_DIR", reroute),
            ("PATH", path.as_str()),
        ];
        let (mut sh, print) = spawn_live(lane, &fx, &env);
        let (prepend, idle) = if label == "fish" {
            (
                format!("set -gx PATH \"{}\" $PATH", fx.foreign.display()),
                "echo IDLE-(math 1 + 0)",
            )
        } else {
            (
                format!("export PATH=\"{}:$PATH\"", fx.foreign.display()),
                "echo IDLE-$((1+0))",
            )
        };
        sh.send("cargo");
        let at = sh.wait_for_after("stub-cargo", 0);
        sh.send(&prepend);
        sh.send("cargo");
        let at = sh.wait_for_after("stub-cargo", at);
        sh.send(idle);
        let at = sh.wait_for_after("IDLE-1", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        sh.send("cargo");
        let at = sh.wait_for_after("stub-cargo", at);
        sh.send(print);
        let at = sh.wait_for_after("MARK42", at);
        let path = sh.value_before("LIVEPATH=", at);
        assert!(
            path.starts_with(&format!("{reroute}:")),
            "{program}: the reroute dir must lead PATH after the prepend: {path:?}"
        );
        assert!(
            path.split(':')
                .any(|e| e == fx.foreign.to_str().expect("UTF-8")),
            "{program}: the prepended dir is demoted, not removed: {path:?}"
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn test_twin_landing_in_an_agents_dir_already_leading_path_is_run_live() {
    let mut shells = ShellLanes::default();
    for (label, program) in shells.lanes(&["bash", "zsh"]) {
        let fx = LiveFixture::new();
        fx.lay_dirs();
        fx.lay_hook(label, ATPKG_HOOK_POSIX_GOLDEN);
        let foreign = fx.foreign.join("claude");
        let (mut sh, hash_foreign, print) = if label == "zsh" {
            (
                spawn_live_zsh(
                    program,
                    &fx,
                    "setopt ksh_arrays warn_create_global\n",
                    &[("ATERM_CHILD", "1")],
                ),
                format!("hash zzz={}", foreign.display()),
                ZSH_PRINT_PATH,
            )
        } else {
            (
                spawn_live_bash(program, &fx, "", &[]),
                format!("hash -p {} zzz", foreign.display()),
                BASH_PRINT_PATH,
            )
        };
        sh.send("claude");
        let at = sh.wait_for_after("foreign", 0);
        sh.send(&format!("{hash_foreign}; echo IDLE-$((1+0))"));
        let at = sh.wait_for_after("IDLE-1", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        sh.send("echo IDLE-$((2+0))");
        let at = sh.wait_for_after("IDLE-2", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        sh.send("zzz");
        let at = sh.wait_for_after("foreign", at);
        fx.lay_twin("managed 1");
        sh.send("claude");
        let at = sh.wait_for_after("managed 1", at);
        fx.lay_twin("managed 2");
        sh.send("claude");
        let at = sh.wait_for_after("managed 2", at);
        assert_live_path(program, &mut sh, &fx, at, print);
        let err = sh.stderr_text();
        assert!(
            !err.contains("created globally"),
            "{program}: sourcing the hook from the hot path must not trip warn_create_global:\n{err}"
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn test_hook_naming_an_agents_dir_that_appears_later_is_fronted_live() {
    let mut shells = ShellLanes::default();
    for lane @ (label, program) in shells.lanes(&["bash", "zsh"]) {
        let fx = LiveFixture::new();
        fx.lay_hook(label, ATPKG_HOOK_POSIX_GOLDEN);
        let (mut sh, print) = spawn_live(lane, &fx, &[("ATERM_CHILD", "1")]);
        sh.send("claude");
        let at = sh.wait_for_after("foreign", 0);
        sh.send("echo IDLE-$((1+0))");
        let at = sh.wait_for_after("IDLE-1", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        fx.lay_dirs();
        fx.lay_twin("managed 1");
        sh.send("claude");
        let at = sh.wait_for_after("managed 1", at);
        assert_live_path(program, &mut sh, &fx, at, print);
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn test_fish_already_running_session_shell_picks_up_the_managed_dirs_live() {
    let fish = need_fish();
    let fx = LiveFixture::new();
    let mut sh = spawn_live_fish(fish, &fx, &[]);
    sh.send("claude");
    let at = sh.wait_for_after("foreign", 0);
    sh.send("echo IDLE-(math 1 + 0)");
    let at = sh.wait_for_after("IDLE-1", at);
    let at = sh.wait_for_after("\x1b]133;A", at);
    fx.lay("fish", ATPKG_HOOK_FISH_GOLDEN, "managed 1");
    sh.send("claude");
    let at = sh.wait_for_after("managed 1", at);
    fx.lay_twin("managed 2");
    sh.send("claude");
    let at = sh.wait_for_after("managed 2", at);
    fx.write_hook(
        "fish",
        &format!(
            "{}set -gx ATPKG_HOOK_GEN 2\n",
            fx.hook_body(ATPKG_HOOK_FISH_GOLDEN)
        ),
    );
    sh.send("echo \"GEN=$ATPKG_HOOK_GEN\"");
    let at = sh.wait_for_after("GEN=2", at);
    assert_live_path("fish", &mut sh, &fx, at, FISH_PRINT_PATH);
    sh.finish();

    let fx = LiveFixture::new();
    fx.lay_dirs();
    fx.lay_hook("fish", ATPKG_HOOK_FISH_GOLDEN);
    let mut sh = spawn_live_fish(fish, &fx, &[]);
    sh.send("claude");
    let at = sh.wait_for_after("foreign", 0);
    sh.send("echo IDLE-(math 1 + 0)");
    let at = sh.wait_for_after("IDLE-1", at);
    let at = sh.wait_for_after("\x1b]133;A", at);
    fx.lay_twin("managed 1");
    sh.send("claude");
    let at = sh.wait_for_after("managed 1", at);
    fx.lay_twin("managed 2");
    sh.send("claude");
    let at = sh.wait_for_after("managed 2", at);
    assert_live_path("fish", &mut sh, &fx, at, FISH_PRINT_PATH);
    sh.finish();

    let fx = LiveFixture::new();
    fx.lay_hook("fish", ATPKG_HOOK_FISH_GOLDEN);
    let mut sh = spawn_live_fish(fish, &fx, &[]);
    sh.send("claude");
    let at = sh.wait_for_after("foreign", 0);
    sh.send("echo IDLE-(math 1 + 0)");
    let at = sh.wait_for_after("IDLE-1", at);
    let at = sh.wait_for_after("\x1b]133;A", at);
    fx.lay_dirs();
    fx.lay_twin("managed 1");
    sh.send("claude");
    let at = sh.wait_for_after("managed 1", at);
    assert_live_path("fish", &mut sh, &fx, at, FISH_PRINT_PATH);
    sh.finish();
}

#[cfg(unix)]
const REKEY_OLD: &str = "1111111111111111111111111111111111111111111111111111111111111111";
#[cfg(unix)]
const REKEY_NEW: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

/// The line that prints the globals a mark is signed with, then a marker whose number is computed.
#[cfg(unix)]
fn rekey_probe(label: &str, n: u32) -> (String, String) {
    let marker = format!("RK{}", 100 + n);
    let line = if label == "fish" {
        format!(
            "printf 'NONCE=%s\\n' \"$__aterm_shell_nonce\"; printf 'SUFFIX=%s\\n' \"$__aterm_id_suffix_str\"; printf 'ENV=%s\\n' \"$ATERM_REKEY_PATH\"; echo RK(math 100 + {n})"
        )
    } else {
        format!(
            "printf 'NONCE=%s\\n' \"$__aterm_shell_nonce\"; printf 'SUFFIX=%s\\n' \"$__aterm_id_suffix_str\"; printf 'ENV=%s\\n' \"${{ATERM_REKEY_PATH:-}}\"; echo RK$((100 + {n}))"
        )
    };
    (line, marker)
}

/// Hands a live shell `line` (a re-key's key, or a body's address) and its newline at `path`.
#[cfg(unix)]
fn deliver(path: &Path, line: &str) {
    let part = path.with_extension("part");
    std::fs::write(&part, format!("{line}\n")).unwrap();
    std::fs::rename(&part, path).unwrap();
}

#[cfg(unix)]
#[test]
fn a_live_shell_takes_a_rekey_at_its_next_prompt() {
    let mut shells = ShellLanes::default();
    for lane @ (label, program) in shells.lanes(&["bash", "zsh", "fish"]) {
        let fx = LiveFixture::new();
        let file = fx.home.join(format!("rekey-{label}"));
        let path = file.to_str().unwrap().to_string();
        let extra = [
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            ("ATERM_REKEY_PATH", path.as_str()),
        ];
        let (mut sh, _) = spawn_live(lane, &fx, &extra);
        let (line, marker) = rekey_probe(label, 1);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, 0);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_OLD,
            "{program}: spawned"
        );
        assert_eq!(sh.value_before("ENV=", at), "", "{program}: path scrubbed");

        deliver(&file, "NOT-A-KEY");
        sh.send("true");
        let (line, marker) = rekey_probe(label, 2);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_OLD,
            "{program}: a bad key changes nothing"
        );
        assert!(!file.exists(), "{program}: a bad key is consumed");

        deliver(&file, REKEY_NEW);
        sh.send("true");
        let (line, marker) = rekey_probe(label, 3);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_NEW,
            "{program}: re-keyed"
        );
        assert_eq!(
            sh.value_before("SUFFIX=", at),
            format!(";id={REKEY_NEW}"),
            "{program}"
        );
        assert!(
            !file.exists(),
            "{program}: the key file is removed once read"
        );
        let (line, marker) = rekey_probe(label, 4);
        sh.send(&line);
        let end = sh.wait_for_after(&marker, at);
        let tail = {
            let out = sh.stdout.lock().unwrap();
            String::from_utf8_lossy(&out[at..end]).into_owned()
        };
        assert!(
            tail.contains(&format!("133;A;id={REKEY_NEW}"))
                || tail.contains(&format!("133;C;id={REKEY_NEW}")),
            "{program}: marks carry the new key: {tail:?}"
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn a_live_bash_prompt_carries_one_133b_however_many_prompts() {
    let utf8 = if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    };
    let older = format!("PS1='\\u \\$ \\[\\033]133;B;id={REKEY_OLD}\\a\\]'\n");
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        for rc in ["", concat!(r"PS1=$'\xc9\\u \\$ '", "\n"), older.as_str()] {
            let fx = LiveFixture::new();
            let mut sh = spawn_live_bash(
                bash,
                &fx,
                rc,
                &[("ATERM_SHELL_NONCE", REKEY_OLD), ("LC_ALL", utf8)],
            );
            let mut at = 0;
            for n in 1..=3u32 {
                sh.send(&format!(
                    "printf 'PS1=%s\\n' \"$PS1\"; echo PS$((500 + {n}))"
                ));
                at = sh.wait_for_after(&format!("PS{}", 500 + n), at);
                let ps1 = sh.value_before("PS1=", at);
                assert_eq!(
                    (
                        ps1.matches("133;B").count(),
                        ps1.contains(r"\u \$ ") || rc.is_empty(),
                        ps1.contains(&format!("133;B;id={REKEY_OLD};aterm_input=start\\a")),
                    ),
                    (1, true, true),
                    "{bash}: rc {rc:?}, prompt {n}: {ps1:?}\n{}",
                    sh.transcript()
                );
            }
            sh.finish();
        }
    }
}

#[test]
fn the_typed_rekey_exists_only_for_scripted_shells() {
    for shell in [ShellType::Zsh, ShellType::Bash, ShellType::Fish] {
        assert!(typed_rekey(shell, "'/k'").is_some(), "{shell:?}");
    }
    for other in [
        ShellType::PowerShell,
        ShellType::Cmd,
        ShellType::Wsl,
        ShellType::Unknown,
    ] {
        assert_eq!(typed_rekey(other, "'/k'"), None, "{other:?}");
    }
}

/// POSIX single quotes, `'\''` for a quote.
#[cfg(any(unix, windows))]
fn posix_quoted(path: &Path) -> String {
    let path = path.to_str().expect("UTF-8");
    #[cfg(windows)]
    let path = path.replace('\\', "/");
    format!("'{}'", path.replace('\'', r"'\''"))
}

#[cfg(unix)]
#[test]
fn a_live_shell_without_the_channel_takes_a_typed_rekey() {
    let mut shells = ShellLanes::default();
    for lane @ (label, program) in shells.lanes(&["bash", "zsh"]) {
        let shell = ShellType::detect(label);
        let fx = LiveFixture::new();
        let extra = [("ATERM_SHELL_NONCE", REKEY_OLD)];
        let (mut sh, _) = spawn_live(lane, &fx, &extra);
        let file = fx.home.join("re key's");
        std::fs::write(&file, format!("{REKEY_NEW}\n")).unwrap();
        sh.send("true");
        let (line, marker) = rekey_probe(label, 1);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, 0);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_OLD,
            "{program}: before"
        );
        assert!(
            file.exists(),
            "{program}: no prompt of its own reads the file"
        );

        let typed = typed_rekey(shell, &posix_quoted(&file)).expect("scripted");
        sh.send(&format!("{typed} echo RAN$((200 + 1))"));
        let at = sh.wait_for_after("RAN201", at);
        let (line, marker) = rekey_probe(label, 2);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(sh.value_before("NONCE=", at), REKEY_NEW, "{program}: taken");
        assert_eq!(
            sh.value_before("SUFFIX=", at),
            format!(";id={REKEY_NEW}"),
            "{program}"
        );
        assert!(!file.exists(), "{program}: the one-use file is gone");
        let (line, marker) = rekey_probe(label, 3);
        sh.send(&line);
        let end = sh.wait_for_after(&marker, at);
        let tail = {
            let out = sh.stdout.lock().unwrap();
            String::from_utf8_lossy(&out[at..end]).into_owned()
        };
        assert!(
            tail.contains(&format!("133;A;id={REKEY_NEW}"))
                && tail.contains(&format!("133;C;id={REKEY_NEW}")),
            "{program}: the next prompt's marks carry the new key: {tail:?}"
        );

        let gone = fx.home.join("withdrawn");
        let typed = typed_rekey(shell, &posix_quoted(&gone)).expect("scripted");
        sh.send(&format!("{typed} echo RAN$((300 + 1))"));
        let at = sh.wait_for_after("RAN301", end);
        let (line, marker) = rekey_probe(label, 4);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_NEW,
            "{program}: a missing file changes nothing"
        );
        let err = sh.stderr_text().to_lowercase();
        assert!(!err.contains("no such file"), "{program}: quiet: {err}");
        sh.finish();
    }
}

#[test]
fn the_script_set_has_one_sixteen_hex_address() {
    let address = script_set_address();
    assert!(is_address(address), "{address}");
    for not in [
        "0123456789ABCDEF",
        "0123456789abcde",
        "0123456789abcdefa",
        ".tmp-x",
    ] {
        assert!(!is_address(not), "{not}");
    }
}

#[test]
fn a_foreign_build_cannot_change_what_this_builds_tabs_source() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("shell-integration");
    let injected_dir = |memo: &Mutex<Option<PathBuf>>| {
        let injection = prepare_cached(ShellType::Zsh, root.clone(), memo)
            .unwrap()
            .unwrap();
        let (_, dir) = injection
            .env_add
            .into_iter()
            .find(|(k, _)| k == "ATERM_SHELL_INTEGRATION_DIR")
            .expect("ATERM_SHELL_INTEGRATION_DIR");
        PathBuf::from(dir)
    };
    let warm = Mutex::new(None);
    let ours = injected_dir(&warm);
    assert_eq!(ours, root.join(script_set_address()));

    let foreign = "__aterm_foreign() { :; }\n";
    std::fs::write(root.join("aterm_shell_integration.zsh"), foreign).unwrap();
    let other = root.join("0123456789abcdef");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("aterm_shell_integration.zsh"), foreign).unwrap();

    for memo in [&warm, &Mutex::new(None)] {
        assert_eq!(injected_dir(memo), ours);
        assert!(script_set_matches(&ours));
    }
    assert_eq!(
        std::fs::read_to_string(root.join("aterm_shell_integration.zsh")).unwrap(),
        foreign
    );

    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(injected_dir(&warm), ours);
    assert!(
        script_set_matches(&ours),
        "re-installed after a mid-run delete"
    );
}

#[test]
fn a_damaged_content_folder_is_replaced_and_racing_writers_share_one() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("si");
    let dir = root.join(script_set_address());
    ensure_scripts(&dir).unwrap();
    std::fs::write(dir.join("bash/rcfile"), "# edited\n").unwrap();
    assert_eq!(install_script_set(&root).unwrap(), dir);
    assert_eq!(
        std::fs::read_to_string(dir.join("bash/rcfile")).unwrap(),
        BASH_WRAPPER
    );
    assert!(script_set_matches(&dir));
    let leftovers: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no scratch folder survives: {leftovers:?}"
    );
    let raced = tmp.path().join("race");
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let raced = raced.clone();
            std::thread::spawn(move || install_script_set(&raced).unwrap())
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), raced.join(script_set_address()));
    }
    assert!(script_set_matches(&raced.join(script_set_address())));
}

#[test]
fn scratch_suffixes_drawn_at_once_never_collide() {
    let handles: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(|| (0..500).map(|_| scratch_suffix()).collect::<Vec<_>>()))
        .collect();
    let mut all = std::collections::BTreeSet::new();
    let mut drawn = 0;
    for h in handles {
        for s in h.join().unwrap() {
            drawn += 1;
            all.insert(s);
        }
    }
    assert_eq!(all.len(), drawn, "a scratch suffix was drawn twice");
}

#[cfg(unix)]
#[test]
fn old_script_folders_are_collected_and_young_or_newest_ones_kept() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("si");
    install_script_set(&root).unwrap();
    std::fs::write(root.join("aterm_shell_integration.zsh"), "legacy").unwrap();
    let base = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let others: Vec<PathBuf> = (0..8u64)
        .map(|i| {
            let dir = root.join(format!("{i:016x}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::File::open(&dir)
                .and_then(|f| f.set_modified(base + std::time::Duration::from_secs(i)))
                .unwrap();
            dir
        })
        .collect();
    let scratch = root.join(".tmp-0000000000000000-1-1");
    std::fs::create_dir_all(&scratch).unwrap();

    collect_old_script_sets(&root, std::time::SystemTime::now());
    assert!(others.iter().all(|d| d.exists()) && scratch.exists());

    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30 * 24 * 3600);
    collect_old_script_sets(&root, later);
    for (i, dir) in others.iter().enumerate() {
        assert_eq!(dir.exists(), i >= 3, "{}", dir.display());
    }
    assert!(!scratch.exists());
    assert!(
        root.join(script_set_address()).exists(),
        "this build's own folder"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("aterm_shell_integration.zsh")).unwrap(),
        "legacy"
    );
}
