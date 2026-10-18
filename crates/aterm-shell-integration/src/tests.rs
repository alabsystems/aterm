// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
#[cfg(unix)]
use std::fmt::Write as _;
#[cfg(any(unix, windows))]
use std::process::Command;

#[cfg(unix)]
fn bash_shell() -> &'static str {
    static BASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BASH.get_or_init(|| {
        // `$ATERM_TEST_BASH` overrides (a newer bash than macOS's /bin/bash 3.2 —
        // see the resolver's doc); otherwise /bin/bash, then a `$PATH` scan, then
        // the bare name as before.
        #[cfg(unix)]
        if let Some(found) = resolve_test_shell("ATERM_TEST_BASH", &["/bin/bash"], "bash") {
            return found;
        }
        if std::path::Path::new("/bin/bash").exists() {
            "/bin/bash".to_string()
        } else {
            "bash".to_string()
        }
    })
}

/// Resolve an interpreter for the live-spawn tests: an explicit `$ATERM_TEST_<SHELL>`
/// override first, then the well-known absolute install paths, then a `$PATH` scan.
///
/// The `$PATH` scan and the override are LOAD-BEARING, not conveniences. Before
/// they existed, [`zsh_shell`] and [`fish_shell`] probed a fixed list of absolute
/// paths and nothing else, so on any host whose zsh/fish lives somewhere else —
/// a Homebrew-on-Linux prefix, a Nix profile, a `dpkg-deb -x` prefix on a box
/// without root — every one of the ~13 live zsh/fish tests took its
/// `else { return; }` arm and printed "not installed; skipping". The suite went
/// green while testing NOTHING, which is exactly how the fish loader-guard bug
/// (see `test_fish_integration_loads_under_an_empty_guard_override`) shipped: fish's
/// integration was dead in every aterm session and no test on the machine ever
/// ran fish to notice. A skip that is indistinguishable from a pass is worse
/// than a missing test, so make the probe find the shell wherever it is, and
/// give CI a way to say exactly which binary to use.
///
/// Returns an owned path (a `$PATH` hit is not a compile-time constant); the
/// callers' `Option<&'static str>` shape is preserved by interning the result in
/// a `OnceLock`, so no call site changes and the probe runs at most once.
#[cfg(unix)]
fn resolve_test_shell(env_override: &str, candidates: &[&str], name: &str) -> Option<String> {
    if let Some(explicit) = std::env::var_os(env_override) {
        let explicit = std::path::PathBuf::from(explicit);
        // An explicit override that does not exist is an OPERATOR ERROR, not a
        // reason to silently skip: fail loudly rather than pretend the shell is
        // simply absent.
        assert!(
            explicit.is_file(),
            "{env_override} points at {explicit:?}, which is not a file"
        );
        return Some(explicit.to_string_lossy().into_owned());
    }
    if let Some(found) = candidates
        .iter()
        .find(|candidate| std::path::Path::new(candidate).is_file())
    {
        return Some((*found).to_string());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.to_string_lossy().into_owned())
}

#[cfg(unix)]
/// Absolute path to a `zsh` binary, or `None` to skip the spawning tests on a
/// host without zsh installed (mirrors [`fish_shell`]). Probing an absolute path
/// rather than bare `"zsh"` keeps the spawn hermetic and avoids a misleading
/// "No such file or directory" failure where zsh simply isn't present.
/// `$ATERM_TEST_ZSH` overrides; otherwise `$PATH` is searched after the
/// well-known locations (see [`resolve_test_shell`]).
fn zsh_shell() -> Option<&'static str> {
    static ZSH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ZSH.get_or_init(|| {
        resolve_test_shell(
            "ATERM_TEST_ZSH",
            &[
                "/bin/zsh",
                "/usr/bin/zsh",
                "/usr/local/bin/zsh",
                "/opt/homebrew/bin/zsh",
            ],
            "zsh",
        )
    })
    .as_deref()
}

/// A shell [`Command`] in its hermetic environment, with the private, EMPTY
/// home directory that environment points at. Dereferences to the `Command`;
/// the home goes when this does, so hold it until the shell has exited (a
/// chained `shell_command(..).arg(..).output()` does). Tests that need their
/// own populated home (the live shells) still set `HOME` themselves
/// afterwards, which overrides this one.
///
/// The subdirectories are pre-created so fish never has to `mkdir` (and warn
/// about) its config/data dirs on first run.
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

    /// The home itself, for a [`LiveShell`] that outlives this command to hold
    /// ([`LiveShell::holding`]).
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

/// Spawn a shell with a hermetic environment for integration tests.
///
/// # The developer's own dotfiles are NOT part of the fixture
///
/// Every live test in this file spawns an INTERACTIVE shell (`fish -i -c`,
/// `zsh -i -c`, `bash --noprofile --norc -i -c`) and several of them assert on
/// byte-exact stdout. Without a redirected `HOME` those shells read whatever
/// the person running `cargo test` happens to have on disk, and anything those
/// files print lands in the same stdout the assertions compare:
///
///   - fish `-i` sources `$XDG_CONFIG_HOME/fish/config.fish` (default
///     `$HOME/.config/fish`), its `conf.d/`, and the *universal* variables in
///     `fish_variables` — where a stray `ATERM_*` would survive every
///     `env_remove` below, because it is restored INSIDE the child;
///   - zsh `-i` (the live shells pass no `-f`) sources
///     `$ZDOTDIR`/`$HOME`'s `.zshenv` and `.zshrc`;
///   - `$XDG_DATA_DIRS/fish/vendor_conf.d` auto-loads vendor snippets — which
///     is exactly how aterm installs its OWN fish integration, so a developer
///     with aterm installed had the shipped script loaded (setting the
///     "already loaded" guard to `1`) before the test's `source` ever ran;
///   - and the integration scripts themselves read `$HOME`: all of them
///     `source $HOME/.aterm/shell.d/*`, which bash executes even under
///     `--noprofile --norc`.
///
/// So point `HOME`, `ZDOTDIR` and the XDG base dirs at [`ShellCommand::home`] —
/// an empty directory this command owns. `XDG_DATA_DIRS` is narrowed to it too, so
/// no vendor `conf.d` (aterm's own included) is auto-loaded. A test that wants
/// one of these back sets it AFTER calling this, which overrides.
///
/// Residual, deliberately not overridden: system-wide files (`/etc/zshenv`,
/// `/etc/zsh/zshrc`, `/etc/fish/config.fish`). Those are not the developer's,
/// and suppressing them for zsh would need `-f`, which the live shells avoid
/// (they start through the real ZDOTDIR wrapper).
///
/// # The exported ATERM_* vars
///
/// A developer running these tests inside aterm has the shipped integration
/// active in their own shell, which `export`s several vars the scripts read at
/// *load* time. Those leak through `cargo test` into the shells spawned below:
///   - `ATERM_SHELL_INTEGRATION_INSTALLED` trips the "already loaded" guard
///     (`[[ -n "$..." ]] && return`), so the script returns before defining any
///     function and every sourced-script test sees "command not found";
///   - `ATERM_PROMPT_STYLE` / `ATERM_DISABLE_PROMPT_TITLES` alter prompt/title
///     behavior; the `ATERM_PROMPT_*_COLOR` quintet feeds `set_color`, and an
///     inherited empty one used to make fish print
///     `set_color: Unknown color ""` into the asserted stream.
///
/// `ATERM_BANNER_B64` and `ATERM_SUITE_VERSION` used to be stripped here too.
/// Both are gone from the scripts: the banner was a reader with no writer that
/// base64-decoded an ambient value to stdout at load, and the suite version was
/// an `export VAR="${VAR:-}"` self-passthrough nothing ever set or read.
///
/// Strip them all so each test sources the script fresh regardless of the
/// developer's own active integration. Tests that need a specific var
/// (e.g. ATERM_SHELL_NONCE) set it AFTER calling this, which overrides the
/// removal.
#[cfg(unix)]
fn shell_command(shell: &str) -> ShellCommand {
    let mut cmd = Command::new(shell);
    for var in [
        "ATERM_SHELL_INTEGRATION_INSTALLED",
        "ATERM_PROMPT_STYLE",
        "ATERM_DISABLE_PROMPT_TITLES",
        "ATERM_PROMPT_HOST_COLOR",
        "ATERM_PROMPT_PATH_COLOR",
        "ATERM_PROMPT_GIT_COLOR",
        "ATERM_PROMPT_ERROR_COLOR",
        "ATERM_PROMPT_SEP_COLOR",
        // The one ATERM_* var aterm really injects into every spawned shell
        // (lib.rs's spawn env). A suite that inherits a live session's nonce
        // is testing that session's identity, not the script's behaviour —
        // and the #8015 scrub it should be exercising becomes unobservable.
        "ATERM_SHELL_NONCE",
        // The reroute seam's pair: a developer running the suite
        // inside an aterm session inherits a live `$ATERM_REROUTE_DIR`, and every
        // script would then re-order the test shell's PATH; a live
        // `$__ATERM_REROUTE_PASSTHROUGH` (the `aterm --no-reroute` marker) is that
        // session's choice, not this fixture's.
        "ATERM_REROUTE_DIR",
        "__ATERM_REROUTE_PASSTHROUGH",
        // The agents dir: exported by a live session's atpkg shell.d
        // hook, and every script moves it to the front beside the reroute dir, so an
        // inherited one would re-order the test shell's PATH the same way.
        "ATPKG_AGENTS",
        // The session gate of the LIVE re-assert: inside an aterm
        // session both are set, and every script would then probe the (hermetic,
        // empty) ~/.aterm/shell.d at every prompt. Inert by default; the live
        // tests set ATERM_CHILD=1 themselves.
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        // The two host channels (2026-09-24, 2026-09-26): a shell spawned by a
        // live session never exports them (its loader scrubs both), but a suite
        // run under an embedder might — and a pointer inherited here would be a
        // real session's.
        "ATERM_REKEY_PATH",
        "ATERM_INTEGRATION_POINTER",
    ] {
        cmd.env_remove(var);
    }
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-si-hermetic-home-")
        .tempdir()
        .expect("create hermetic HOME for shell integration tests");
    let home = dir.path();
    for sub in ["config/fish/conf.d", "data/fish", "state", "cache"] {
        std::fs::create_dir_all(home.join(sub))
            .expect("create hermetic HOME for shell integration tests");
    }
    cmd.env("HOME", home)
        .env("ZDOTDIR", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_DATA_DIRS", home.join("data"));
    ShellCommand { cmd, home: dir }
}

/// `(shell, args, script, quiet)` — see [`posix_lanes`].
#[cfg(unix)]
type PosixLane = (
    &'static str,
    &'static [&'static str],
    &'static str,
    &'static str,
);

/// The two POSIX lanes the script-level tests drive: bash, and zsh where it is
/// installed — `(shell, args, script, quiet)`, where `quiet` silences the hooks
/// an interactive shell installs so only what the test calls reaches stdout.
/// zsh's runs under a user's `setopt ksh_arrays` (subscripts from 0), as every
/// zsh function runs with the user's options around it: a loop that read
/// `${string[$i]}` from 1 dropped the first byte and read one past the last —
/// in the OSC 7 encoder and in the 633;E one.
#[cfg(unix)]
fn posix_lanes() -> Vec<PosixLane> {
    let mut lanes = vec![(
        bash_shell(),
        &["--noprofile", "--norc", "-i"][..],
        "aterm_shell_integration.bash",
        "__aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=",
    )];
    if let Some(zsh) = zsh_shell() {
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

/// The reroute directory (`$ATERM_REROUTE_DIR`, set by aterm's spawn seam) and the
/// agents directory (`$ATPKG_AGENTS`, exported by the atpkg shell.d hook) lead PATH in
/// that order — MOVED there from wherever the inherited PATH listed them, each exactly
/// once, the rest kept in order — and every script is inert for an unset variable or
/// one naming nothing on disk (Windows lays no stubs). The measured failure
/// (2026-09-10, m27): path_helper and a ~/.zshrc prepend left the agents dir at PATH
/// position 14, so `codex` and `claude` ran foreign copies. `-c` has no first prompt,
/// so this is the load-time assert (the live tests below hold the order at every
/// prompt). bash and zsh (`-f`, no rc files) are checked byte-exactly; fish for
/// position and count only, because its own startup may add entries.
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
    let mut lanes: Vec<(&str, &[&str], &str, String, bool)> = posix_lanes()
        .into_iter()
        .map(|(shell, args, script, quiet)| {
            let print = format!("{quiet}; printf 'PATH=%s\\n' \"$PATH\"");
            (shell, args, script, print, true)
        })
        .collect();
    if let Some(fish) = fish_shell() {
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
        // `pre` runs before the source. The value is the LAST `PATH=`'s, up to the
        // first control byte.
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
        // Buried behind system entries, listed twice, the reroute dir after the agents
        // dir: reroute first, agents second, each once, the rest in order.
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
        // Already first: the same answer (a nested aterm stacks nothing).
        assert_eq!(run(both.0, both.1, &got, ""), got, "{shell}: idempotent");
        // Unset, or naming nothing on disk: nothing is prepended.
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
        // A sole EMPTY entry ("here") beside the agents dir survives both moves. PATH
        // is set INSIDE the shell so the harness still finds the shell binary.
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

/// `$HOME/.aterm/bin` is a folder nothing in aterm creates or manages (atpkg's
/// managed `bin/` lives under its prefix and is APPENDED, `crates/atpkg/src/hooks.rs`;
/// its command links go to `~/.local/bin`), so no integration puts it on PATH:
/// an executable dropped there would otherwise shadow every command in every
/// session. Live shells sourcing the real script with a populated
/// `~/.aterm/bin` holding an `ls`; bash and zsh (`-f`) byte-exactly, fish for
/// absence (its own startup may add entries). The PowerShell script is checked
/// as text, as no pwsh runs on the unix lanes.
#[cfg(unix)]
#[test]
fn test_an_unmanaged_aterm_bin_is_never_put_on_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let mut lanes: Vec<(&str, Vec<&str>, &str, &str, bool)> = vec![(
        bash_shell(),
        vec!["--noprofile", "--norc", "-i", "-c"],
        "aterm_shell_integration.bash",
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; printf 'PATH=%s\\n' \"$PATH\"",
        true,
    )];
    if let Some(zsh) = zsh_shell() {
        lanes.push((
            zsh,
            vec!["-f", "-i", "-c"],
            "aterm_shell_integration.zsh",
            "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; print -r -- \"PATH=$PATH\"",
            true,
        ));
    }
    if let Some(fish) = fish_shell() {
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
        // The last `PATH=` is the printf's (see the reroute and agents test above).
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
    // PowerShell, as text: no line of code (a comment may explain) names it.
    let named = scripts::POWERSHELL
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find(|line| line.contains(".aterm/bin") || line.contains(".aterm\\bin"));
    assert_eq!(
        named, None,
        "the PowerShell integration must not put ~/.aterm/bin on PATH either"
    );
}

/// The 633;E encoding of `line`, byte for byte: backslash, semicolon and space
/// spelled as escapes, every control byte as `\xNN`, every other byte as it is.
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

/// THE ENCODERS ANSWER IN `REPLY`, FORK-FREE, WITH THE BYTES THEY PRINTED: over
/// spaces, `%`, Unicode, a newline, a tab, ESC/BEL, DEL, `;` and `\\`, the empty
/// string and a path needing nothing, bash's and zsh's `__aterm_urlencode` and
/// `__aterm_encode_cmd` leave in `REPLY` the percent- and 633;E-encodings
/// spelled out above — zsh under a user's KSH_ARRAYS too. And a user's own
/// `REPLY` survives the prompt's OSC 7 (and zsh's preexec), whose callers keep
/// theirs local.
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
    let mut runs: Vec<(&str, &[&str], String, &str, &str)> = vec![(
        bash_shell(),
        &["--noprofile", "--norc", "-i"],
        bash_quiet.to_owned(),
        "aterm_shell_integration.bash",
        "",
    )];
    if let Some(zsh) = zsh_shell() {
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
        // An interactive bash may lead with its readline meta-mode probe.
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

/// OSC 7 from each POSIX lane's `__aterm_report_cwd`: a percent-encoded `file://`
/// URI for a cwd holding a space, `@`, `[`, `]`, `!` and non-ASCII.
#[cfg(unix)]
#[test]
fn test_report_cwd_emits_percent_encoded_osc_7() {
    let (_dir, cwd) = create_special_cwd();
    let expected = format!(
        "\u{1b}]7;file://aterm.test{}\u{7}",
        osc7_percent_encode(cwd.to_str().expect("cwd path should be UTF-8"))
    );
    for (shell, args, script_name, quiet) in posix_lanes() {
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

/// Git Bash (MSYS2) and Cygwin report the Windows drive form over OSC 7, not
/// `$PWD`'s POSIX view: `/c/Windows/Temp` names no Windows directory, so the tab
/// showed it verbatim and `tab new` fell back to the launch directory (measured
/// 2026-09-27 on 0.95.0). A drive mount is rewritten by expansion alone; any
/// other path asks `cygpath` once per directory. Driven here by assigning
/// `OSTYPE` and `PWD`, with `cygpath` a function that counts its calls.
#[cfg(unix)]
#[test]
fn test_bash_on_msys_and_cygwin_reports_the_windows_drive_form() {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let calls = dir.path().join("cygpath-calls");
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
        let output = shell_command(bash_shell())
            .args(["--noprofile", "--norc", "-i", "-c", &command])
            .env("ATERM_TEST_SCRIPT", &script)
            .env("ATERM_TEST_CALLS", &calls)
            .env("HOSTNAME", "aterm.test")
            .output()
            .unwrap_or_else(|error| panic!("spawn bash for MSYS cwd test: {error}"));
        assert!(
            output.status.success(),
            "bash should report OSC 7 as {ostype}; stderr: {:?}",
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

    // Git Bash: drive mounts need no cygpath; the rest resolve through it, once
    // per directory (the repeated /usr/bin is served from the one-entry cache).
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
        ]
    );
    assert_eq!(calls_made(), 2, "cygpath runs once per non-drive directory");

    // Cygwin mounts drives under /cygdrive, so a bare /c is not a drive there.
    let _ = std::fs::remove_file(&calls);
    let reported = run("cygwin", &["/cygdrive/c/Users", "/c"]);
    assert_eq!(reported, ["/C%3A/Users", "/C%3A/Program%20Files/Git/c"]);
    assert_eq!(calls_made(), 2);

    // Negative control: any other bash reports $PWD as it is and never asks.
    let _ = std::fs::remove_file(&calls);
    let reported = run("darwin24", &["/c/Windows/Temp"]);
    assert_eq!(reported, ["/c/Windows/Temp"]);
    assert_eq!(calls_made(), 0);
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
    // is a top-level DEBUG-trapped command, ours first, the siblings after ours
    // returned, and on bash 5.1+ aterm's last element after them (an unarmed no-op
    // before 5.1, where the wiring adds none). Two siblings, so the array is taken
    // as one (an array with nothing at index 1 runs the scalar way — the next
    // test). The marker rides an `__aterm_`-prefixed function so the guard skips
    // it identically before and after the fix.
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
        "the real command must emit the 133;C command start, unsigned with no nonce: {after:?}"
    );
}

/// An array PROMPT_COMMAND with nothing at index 1 is taken the scalar way, read
/// without a fork: ours takes element 0 and evals what it held, so the user's
/// element runs once a prompt on every bash. Taken as an array (what the forked
/// `declare -p` read made of it), ours went first and the user's after — which
/// bash before 5.1, running element 0 alone, never ran. A bash runs element 0 of
/// a one-element array on every version, so `eval "$PROMPT_COMMAND"` is the
/// prompt here.
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
    let output = shell_command(bash_shell())
        .args(["--noprofile", "--norc", "-i", "-c", command])
        .env("ATERM_TEST_SCRIPT", &script)
        .output()
        .unwrap_or_else(|error| panic!("spawn bash for the one-element array test: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stdout: {stdout:?}; stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("[n=1 first=__aterm_prompt_command]"),
        "ours takes element 0 and adds none: {stdout:?}"
    );
    assert_eq!(
        stdout.matches("[user-precmd]").count(),
        2,
        "the user's element runs once a prompt, two prompts: {stdout:?}"
    );
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
        // The bare aliases the PTY seam resolves, and the program paths they
        // resolve to (Windows separators, split on every host).
        ("wsl", ShellType::Wsl),
        ("cmd", ShellType::Cmd),
        ("C:\\Windows\\System32\\wsl.exe", ShellType::Wsl),
        ("C:\\Windows\\System32\\CMD.EXE", ShellType::Cmd),
        // Neighbours are not swept in: nu has no injection.
        ("nu.exe", ShellType::Unknown),
    ] {
        assert_eq!(ShellType::detect(path), shell, "{path:?}");
    }
}

/// A directory name may hold any byte but '/' and NUL, so a crafted one could
/// embed ESC/BEL and smuggle a nested OSC (a clipboard write) out of the OSC 0
/// cwd tab title the prompt hook emits. Driven through each POSIX prompt hook
/// (and fish's prompt, where fish is installed) from inside such a directory.
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
    if let Some(fish) = fish_shell() {
        let stdout = run_fish_integration(
            fish,
            &[("ATERM_TEST_CWD", cwd.to_str().expect("a UTF-8 crafted cwd"))],
            "cd -- \"$ATERM_TEST_CWD\"; function __aterm_original_fish_prompt; end; fish_prompt",
        );
        assert_clean(fish, stdout.as_bytes());
    }
    let mut lanes = vec![(
        bash_shell(),
        vec!["--noprofile", "--norc", "-i"],
        "aterm_shell_integration.bash",
        "__aterm_prompt_command",
    )];
    if let Some(zsh) = zsh_shell() {
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

/// A command line must not break out of the OSC 633;E that carries it, nor out of
/// the OSC 0 title. `__aterm_encode_cmd` hex-escapes every reserved byte, and
/// `__aterm_osc` must then copy the payload through byte-for-byte: zsh's `print`
/// WITHOUT `-r` did not — it expanded escapes in its argument, re-materializing
/// the raw ESC/BEL bytes the encoder exists to remove. Driven through the two
/// emissions `preexec` performs, spelled identically for bash and zsh (both
/// encoders answer in `REPLY`); fish's encoder (its writer is `printf '\e]%s\a'`)
/// must produce the same payload.
#[cfg(unix)]
#[test]
fn test_osc_frame_writer_preserves_payload_escapes() {
    // Two independent break-out attempts: raw ESC/BEL bytes, which the encoder must
    // hex-escape and the writer leave escaped; and the LITERAL text
    // `\e]52;c;aGVsbG8=\a` — printable, so the title's `[[:cntrl:]]` strip has
    // nothing to strip, and only a writer that expands escapes turns it into a
    // real nested OSC 52 clipboard write.
    const CMDLINE: &str = "a\u{1b}b\u{7}c;d e\\f \\e]52;c;aGVsbG8=\\a";
    // Every reserved byte the decoder's backslash escape, so the 633;E carries one
    // BEL; the title strips the control bytes and keeps the literal `\e`/`\a` text.
    const PAYLOAD: &str = r"a\x1bb\x07c\x3bd\x20e\\f\x20\\e]52\x3bc\x3baGVsbG8=\\a";
    let expected = format!(
        "\u{1b}]633;E;{PAYLOAD}\u{7}\u{1b}]0;{}\u{7}",
        r"abc;d e\f \e]52;c;aGVsbG8=\a"
    );
    if let Some(fish) = fish_shell() {
        let encoded = run_fish_integration(
            fish,
            &[("ATERM_TEST_INPUT", CMDLINE)],
            "printf '%s' \"$(__aterm_encode_cmd \"$ATERM_TEST_INPUT\")\"",
        );
        assert_eq!(encoded, PAYLOAD, "fish");
    }
    for (shell, args, script_name, quiet) in posix_lanes() {
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
        // `ends_with`: an interactive bash may prefix stdout with its readline
        // meta-mode probe on some hosts.
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
    // An unknown shell must not litter the cache dir with scripts — on
    // Windows the old fallback wrote them to C:\tmp on every spawn.
    assert!(
        !base.exists(),
        "prepare_into(Unknown) must not create the cache directory"
    );
}

// NOTE: the two tests that exercised prepare_zsh/prepare_fish with the
// relevant env var REMOVED (`ZDOTDIR`, `XDG_DATA_DIRS`) do not live here:
// this unit binary runs its tests on parallel threads, and mutating the
// process environment races every sibling test that calls `prepare_into`
// (which reads those vars). They live in the separate `shell_suite` binary
// instead (tests/prepare_zsh_unset_zdotdir.rs and
// tests/prepare_fish_xdg_default.rs), where `aterm_log::env::scoped_unset`'s
// lock serialises them. Do NOT add env-mutating tests to this file — add them
// to `shell_suite`, through `scoped_unset`.

// ─── fish ──────────────────────────────────────────────────────────────

/// Source the fish integration in an interactive fish, run `body`, and return
/// its stdout. `extra_env` is applied AFTER [`shell_command`]'s hermetic scrub, so
/// a test can put a variable back deliberately (including as an empty-but-SET
/// value, which is the whole point of the loader-guard regression).
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

/// The fish loader guard spelled `set -q` (definedness, not emptiness): with the
/// guard SET BUT EMPTY — what aterm's spawn seam injects into every integrated
/// session — the integration must still load, and scrub the nonce from the
/// environment (#8015: every child process would inherit it). Under `set -q`
/// a live fish tab had no marks, no blocks, and the nonce un-scrubbed.
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

/// Every OSC 133/633 mark survives an EMPTY nonce, and carries `;id=<hex>` when
/// one is set. fish glues a command substitution to a string as a CARTESIAN
/// PRODUCT, so the old `__aterm_osc "133;A"(__aterm_id_suffix)` spelling expanded
/// to ZERO arguments whenever the suffix was empty — the pre-nonce fallback, and
/// the manual `source`-from-config.fish install — and `printf '\e]%s\a'` emitted
/// a bare empty OSC instead. Measured on fish 4.2.1 before the fix: 133;A/B/C/D
/// and 633;E were ALL absent from the wire.
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

// `set -q` is definedness, and every ATERM_* read here wants EMPTINESS: the
// misspelling survived the loader-guard fix in the prompt-style selector, the
// tab-title guards and the prompt-colour defaults.

/// An EMPTY `ATERM_PROMPT_STYLE` must fall through to the user's own prompt:
/// under `set -q`, `__aterm_custom_prompt` ran with `style=""`, matched no case,
/// and fish printed NOTHING between 133;A and 133;B. Both branches (and the cwd
/// report) are stubbed, so the test needs none of fish's own `share/functions`.
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

/// An EMPTY `ATERM_DISABLE_PROMPT_TITLES` must NOT disable titles, as in bash and
/// zsh; fish's `not set -q` read empty as "disabled" and dropped the OSC 0 title.
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
    // Negative control: the opt-out itself must keep working.
    assert_eq!(
        run_fish_integration(fish, &[("ATERM_DISABLE_PROMPT_TITLES", "1")], body),
        without_title,
        "a non-empty ATERM_DISABLE_PROMPT_TITLES must still suppress the title"
    );
}

/// EMPTY `ATERM_PROMPT_*_COLOR` values fall back to the defaults bash and zsh get
/// from `${ATERM_PROMPT_*_COLOR:-N}`; under `set -q` they reached `set_color ""`
/// on every prompt. Each is a palette INDEX, which fish's `set_color` (names or
/// hex only) rejected with no output, so the script formats the `38;5;N` SGR
/// itself — asserted on the bytes. Only `prompt_pwd` is stubbed.
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
    // `minimal` renders <path-color> pwd <reset> <prompt-color> $ <reset>;
    // the defaults are path=4 and (last status 0) separator=8.
    assert_eq!(
        run_fish_integration(fish, &empty_colors, body),
        "\u{1b}[38;5;4m~/x\u{1b}[0m \u{1b}[38;5;8m$\u{1b}[0m ",
        "empty ATERM_PROMPT_*_COLOR values must fall back to the same defaults \
         bash and zsh get from `${{ATERM_PROMPT_*_COLOR:-N}}`"
    );
    // Positive control: an explicitly set colour is still honoured.
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

/// Absolute path to a `fish` binary, or `None` to skip the spawning tests on a
/// host without fish installed. `$ATERM_TEST_FISH` overrides; otherwise `$PATH`
/// is searched after the well-known locations (see [`resolve_test_shell`]).
#[cfg(unix)]
fn fish_shell() -> Option<&'static str> {
    static FISH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FISH.get_or_init(|| {
        resolve_test_shell(
            "ATERM_TEST_FISH",
            &[
                "/opt/homebrew/bin/fish",
                "/usr/local/bin/fish",
                "/usr/bin/fish",
            ],
            "fish",
        )
    })
    .as_deref()
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

/// #8015 through PS1: the prompt hook appends `;id=<nonce>` to PS1, and a PS1
/// that bash imported from its environment (or a user's rc exported) keeps the
/// export attribute, so every child the tab started read the nonce from
/// `$PS1`. The hook drops the attribute at each prompt; a user's later
/// `export PS1` is undone at the next one.
#[cfg(unix)]
#[test]
fn test_bash_children_never_inherit_the_nonce_through_ps1() {
    let nonce = "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        env!("CARGO_MANIFEST_DIR")
    );
    // Exit 96 if PS1 never got the nonce: then the env check proves nothing.
    let command = "\
source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1\n\
__aterm_prompt_command >/dev/null 2>&1\n\
[[ -n \"$__aterm_shell_nonce\" && \"$PS1\" == *\";id=$__aterm_shell_nonce\"* ]] || exit 96\n\
env >\"$HOME/child-env-1\"\n\
export PS1\n\
__aterm_prompt_command >/dev/null 2>&1\n\
env >\"$HOME/child-env-2\"\n";
    let mut cmd = shell_command(bash_shell());
    cmd.args(["--noprofile", "--norc", "-i", "-c", command])
        .env("ATERM_TEST_SCRIPT", &script)
        .env("ATERM_SHELL_NONCE", nonce)
        .env("PS1", "inherited$ ");
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn bash for PS1 nonce test: {error}"));
    assert!(
        output.status.success(),
        "bash PS1 nonce probe should succeed (96: PS1 never carried the nonce); \
         status: {:?}; stderr: {:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    for probe in ["child-env-1", "child-env-2"] {
        let env = std::fs::read_to_string(cmd.home().join(probe))
            .unwrap_or_else(|error| panic!("read {probe}: {error}"));
        assert!(
            env.contains("ATERM_TEST_SCRIPT="),
            "{probe} must hold the child's environment: {env:?}"
        );
        assert!(
            !env.contains(nonce),
            "a child of an integrated bash must not inherit the nonce ({probe}): {env:?}"
        );
    }
}

/// zsh's half of #8015 (bash's rides the PS1 test above, fish's the loader-guard
/// test): the nonce is captured into a shell-local and unset from the environment
/// at source time, and the marks still sign with it; with no nonce at all the
/// suffix is empty — an unsigned mark, for a host from before the nonce.
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

// ─── PowerShell / pwsh integration (Windows' default shells) ─────────────
//
// Functional tests spawn powershell.exe and are Windows-only (powershell.exe
// ships with every supported Windows).

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

/// Each run has its own hidden console, so the probe reads that console's code
/// page and the switch cannot leak into the next run or the test runner's
/// console. Outside aterm the script leaves both encodings exactly as a bare
/// pwsh on a fresh console has them.
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

/// How a test pwsh runs: without the integration script, with it outside an
/// aterm session, or with it in an aterm tab.
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

/// One run of `exe` (`powershell.exe`, or a pwsh 7 path) in a hidden console of
/// its own: `prelude` first (where a test plays the user's profile, which runs
/// before the integration in a real tab; end it with `; `), then the
/// integration script unless [`Pwsh::Bare`], then `snippet`. `env` overrides
/// the ordinary scrub; the exclusive home/profile-state roots always go last.
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
    // The prelude and snippet must avoid double quotes: powershell.exe's
    // command-line quote handling would otherwise interact with Rust's arg
    // quoting.
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
    // Hermetic spawn: a developer running the tests inside aterm has these
    // exported, which would trip the already-loaded guard, alter titles,
    // decide whether the shell is in an aterm tab, or switch the readline key
    // layer off.
    for var in [
        "ATERM_SHELL_INTEGRATION_INSTALLED",
        "ATERM_DISABLE_PROMPT_TITLES",
        "ATERM_DISABLE_READLINE_KEYS",
        "ATERM_CHILD",
        "ATERM_SESSION_ID",
        // Each PowerShell builds its own module path when this is unset. A suite
        // run from a pwsh 7 tab inherits pwsh's, and Windows PowerShell 5.1 then
        // imported pwsh's PSReadLine 2.4.5 instead of its own 2.0.0 (measured
        // 2026-10-16; pwsh rewrites it only for a powershell.exe it starts
        // itself, not one under cmd and cargo).
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
    // Confinement is last, so a test's ordinary environment overrides cannot
    // restore the runner's home or profile-state roots.
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

/// The scripts are staged in the per-user cache in every containment mode, and
/// that lies outside every root a Containment shell may write
/// (`aterm_containment::sbpl::WRITABLE_ROOTS`): a contained shell reads the
/// scripts the next tab's shell sources and cannot rewrite them. Staging them in
/// `/tmp` for Containment and Safety (#5575) let it.
#[cfg(unix)]
#[test]
fn test_cache_dir_is_the_per_user_cache_outside_the_containment_write_roots() {
    let (base, tail) = match (std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME")) {
        (Some(cache), _) => (PathBuf::from(cache), "aterm/shell-integration"),
        (None, Some(home)) => (PathBuf::from(home), ".cache/aterm/shell-integration"),
        (None, None) => (std::env::temp_dir(), "aterm-shell-integration"),
    };
    assert_eq!(
        cache_dir(),
        base.join(tail),
        "the per-user cache, whatever the mode"
    );
    // Resolved as Seatbelt resolves it.
    let Ok(base) = std::fs::canonicalize(&base) else {
        return;
    };
    let dir = std::fs::canonicalize(base.join(tail)).unwrap_or_else(|_| base.join(tail));
    let roots = aterm_containment::sbpl::WRITABLE_ROOTS;
    if roots.iter().any(|root| base.starts_with(root)) {
        eprintln!(
            "this run's cache base is itself under a temp root: {}",
            base.display()
        );
        return;
    }
    for root in roots {
        assert!(
            !dir.starts_with(root),
            "{} lies under the Containment write root {root}",
            dir.display()
        );
    }
}

// ---------------------------------------------------------------------------
// WSL + cmd.exe: the two first-class Windows shells that used to detect as
// `Unknown` and therefore got no injection at all.
// ---------------------------------------------------------------------------

/// The WSL injection crosses into the distro through `WSLENV` (ours first, see
/// below) and runs as `wsl --exec /bin/sh -c <program>` — `--exec` keeps argv
/// byte-for-byte, where `wsl -- …` re-quotes every argument into a `bash -c`
/// string. The program is driven here under this host's own `/bin/sh` with a
/// stand-in login shell: it enters the handed-over cwd and drops the variable (a
/// shell nested inside must not jump back), starts a bash login shell on the
/// EXISTING bash wrapper rcfile, and any other login shell plainly — forcing bash
/// on a zsh WSL user to gain marks would be a regression, not a feature.
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

/// `WSLENV` is the ONLY channel into the distro, so getting it wrong is silent:
/// without `/p` the integration dir arrives as an untranslated `C:\…` no Linux
/// shell can read, and without the nonce every mark is dropped. A user's own
/// entries survive after ours; nesting (aterm inside aterm) neither grows
/// duplicates nor keeps a stale flag spelling for one of our variables.
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

/// cmd renders `%PROMPT%` on every input line and understands `$E`/`$P`; that is
/// the whole integration surface it has. In order: the directory title (before
/// the user's prompt, so a prompt that sets its own title is the later write and
/// wins), 133;A, the cwd (AFTER 133;A: the engine files a 633 `P;Cwd` under the
/// block in progress, so reported before it the block for `cd /d X` said it ran
/// in `X`), cmd's own prompt, 133;B — every mark signed with the session nonce,
/// which `augment_with_nonce` also hands the shell. No C/D: cmd has no hook for
/// "a command started executing", and the phase machine needs A→B→C→D in order.
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

/// An inherited `%PROMPT%` is wrapped, not replaced: a user's own title is kept
/// whole AFTER ours (the later write wins), an empty one is cmd's default, and one
/// that already carries our marks (a nested aterm) is returned byte-identical —
/// nesting neither double-wraps nor stacks a second title.
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

/// A POSIX shell reads a script line by line and keeps a trailing CR as part of
/// the last token, so one `\r` per line shreds the whole file. The scripts are
/// `include_str!`d at build time, so a Windows build machine with Git's default
/// `core.autocrlf=true` shipped exactly that — measured: 446 CRs in the installed
/// `aterm_shell_integration.bash`, and sourcing it in WSL failed at line 16.
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
    // And the title is that same directory, in a real ESC-framed OSC 0 — the
    // bytes conhost turns into the console title ConPTY hands aterm. Every cmd
    // prompt carries it: there is no opt-out (a user's own PROMPT title is the
    // later write and wins), so its absence is a regression, never a choice.
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

/// Functional proof on the real thing: the WSL launcher, run through the
/// installed `wsl.exe`, lands OSC 133 marks with the session nonce. COULD NOT
/// RUN when WSL is not installed — aterm must never DEPEND on it.
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

// ─── fish: glued command substitutions, and prompt colours ──────────────────
//
// Two defects with one root, both fish-only, both invisible to bash and zsh.
//
// THE GLUE RULE. In fish, a string written next to a command substitution is a
// CARTESIAN PRODUCT over the substitution's output LINES, and a command that
// prints nothing produces a ZERO-ELEMENT list — so the product is zero
// arguments and the ENTIRE argument disappears. bash and zsh interpolate a
// possibly-empty parameter and cannot reproduce any of it.
//
// THE PALETTE RULE. Every `ATERM_PROMPT_*_COLOR` is a palette INDEX (bash spells
// it `\[\033[38;5;${hc}m\]`). fish's `set_color` takes colour NAMES or hex only,
// answers `set_color: Unknown color "4"` on stderr, and emits nothing — which,
// via the glue rule, also deleted `printf` arguments and slid the prompt layout
// sideways.

/// fish's OSC 7 stays a well-formed `file://` URI with the resolved host EMPTY:
/// the pre-fix failure was a wrong NUMBER OF ARGUMENTS — the host glued to a
/// substitution that printed nothing took the whole argument with it, and
/// `__aterm_osc` printed a bare `ESC ] BEL`. `file:///path` is the
/// empty-authority form RFC 8089 defines. (The named host is pinned by the
/// prompt's re-assert below.)
#[cfg(unix)]
#[test]
fn test_fish_osc7_is_well_formed_with_an_empty_host() {
    let fish = need_fish();
    let (_dir, cwd) = create_special_cwd();
    // A bare `cd`: the report rides fish's `--on-variable PWD` handler and fires
    // EXACTLY once (the source-time report is swallowed with the load).
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

/// Every prompt re-asserts the shell's own cwd before its 133;A, as zsh, bash
/// and pwsh do. OSC 7 carries no nonce (a remote shell over ssh must be able to
/// report a cwd), so a cwd a program reported — or a remote shell's, left when
/// ssh exits — went into the next signed block and became the new-tab folder
/// until the next `cd`, while the nonce gate dropped the same forgery sent as
/// `633;P;Cwd`. The prompt is drawn without its event: fish's own `fish_prompt`
/// handler re-sends OSC 7 from 4.3 on, and before that only on a `cd`. The
/// engine now holds an OSC 7 that arrives while the signed shell sits at its
/// prompt and applies the latest one at the shell's next `133;A`
/// (`aterm-core`'s `reported_cwd_tests`), so this report, the last before the
/// mark, is the one that lands.
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
    // A command's output names another folder; then the prompt.
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

/// The fallback prompt survives a host with no `hostname` command.
///
/// `echo -n (whoami)'@'(hostname)' '(prompt_pwd)' $ '` printed NOTHING when any
/// one of those four substitutions came back empty — the commonest trigger
/// being a container image with no `hostname` binary. The prompt is now built
/// from captured locals, so a missing segment costs its own text and nothing
/// else.
#[cfg(unix)]
#[test]
fn test_fish_fallback_prompt_survives_a_missing_hostname() {
    let fish = need_fish();
    // No ATERM_PROMPT_STYLE and no user prompt to wrap => the fallback arm.
    //
    // `$USER` is set INSIDE the body, not through the environment: fish
    // populates its own `$USER` global from the passwd database at startup, so
    // an exported one would be overwritten before the prompt ever renders.
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

// ---------------------------------------------------------------------------
// THE MULTIPLEXER BOUNDARY (screen / tmux)
//
// aterm hosts ONE session per PTY; start `screen` or `tmux` in it and every pane
// lives inside that SAME session. Measured with GNU screen 4.09.01 before any of
// this existed: the whole screen run collapsed into ONE outer block, and a
// flagless `aterm ctl status` typed IN a pane answered from the OUTER session —
// success, wrong terminal. A pane is genuinely not an aterm session, so the
// crossing is made VISIBLE: marked ($ATERM_MUX + the outer sid, which `aterm ctl`
// refuses flagless self-drive on) and said once out loud. The guard is what makes
// the detection trustworthy: aterm's spawn seam forces
// `$ATERM_SHELL_INTEGRATION_INSTALLED` EMPTY for every session it starts, so a
// NON-empty value means we did not come straight from aterm.
// ---------------------------------------------------------------------------

/// Source a script under `shell` in a synthesized environment and report the
/// boundary variables it left behind, plus whatever it said on stderr.
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
        // A pane's own environment is synthesized per case; start from a clean
        // slate so the developer's real shell cannot decide the outcome.
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

/// The boundary in each real shell: a pane shell (inherited guard) is MARKED with
/// its multiplexer and the outer sid, and says so once per MULTIPLEXER (a stamp
/// under `$XDG_RUNTIME_DIR` keyed by `$TMUX`/`$STY`), not once per pane; `$TMUX`
/// names tmux though its default TERM is `screen-256color`. An aterm SESSION shell
/// (guard empty) is not marked, CLEARS a marker inherited from the pane it was
/// launched out of, and — the fallback for a host without the spawn seam — stamps
/// the multiplexer it was born into as `$ATERM_MUX_BASE`, which `aterm ctl`
/// compares a pane's own `$TMUX`/`$STY` against (a bash or zsh pane shell never
/// sources the integration: `--rcfile` and `$ZDOTDIR` are one-shot mechanisms of
/// whoever starts the shell, and a pane is started by the multiplexer).
#[cfg(unix)]
#[test]
fn test_each_shell_marks_a_multiplexer_pane_and_clears_a_stale_marker() {
    const PANE: (&str, &str) = ("ATERM_SHELL_INTEGRATION_INSTALLED", "1");
    const SESSION: (&str, &str) = ("ATERM_SHELL_INTEGRATION_INSTALLED", "");
    const TMUX: (&str, &str) = ("TMUX", "/tmp/tmux-1000/default,9,0");
    const SCREEN: (&str, &str) = ("STY", "4242.pts-3.host");
    const OUTER: (&str, &str) = ("ATERM_PARENT_SESSION_ID", "s-outer");
    // (environment, the report, the multiplexer announced on stderr, run in fish
    // too — every case runs in bash and zsh).
    type Case = (
        &'static [(&'static str, &'static str)],
        &'static str,
        Option<&'static str>,
        bool,
    );
    let cases: &[Case] = &[
        // A GNU screen window inside an aterm session — the measured case.
        (
            &[PANE, OUTER, ("TERM", "screen.xterm-256color"), SCREEN],
            "mux=screen outer=s-outer base=UNSET",
            Some("screen"),
            true,
        ),
        // Its second pane: quiet.
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
        // ATERM_MUX_NOTICE=0 silences the line without disturbing the marks.
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
        // A user's ATERM_MUX=0 turns the check off: kept, and nothing announced.
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
        // No aterm session in scope: marked (so `aterm ctl mux` can say what it
        // sees), no outer sid, nothing announced — there is no block model to lose.
        (
            &[PANE, ("TERM", "screen"), ("STY", "77.pts-1.h")],
            "mux=screen outer=UNSET base=UNSET",
            None,
            false,
        ),
        // A plain nested shell inside a session: no multiplexer, nothing to mark.
        (
            &[PANE, OUTER, ("TERM", "xterm-256color")],
            "mux=UNSET outer=UNSET base=UNSET",
            None,
            false,
        ),
        // An ordinary session shell, nothing nested yet: the base is stamped anyway,
        // as the empty pair, which is what a later pane's own `$STY`/`$TMUX` differs from.
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
        // A session shell in a window LAUNCHED FROM a tmux pane: the inherited
        // marker is cleared, and the base it stamps is its own, so the pane's
        // `$TMUX` matches it and the window is not mistaken for a pane.
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
    // stdout is redirected, stderr is NOT: the notice IS the stderr of the source.
    // Then the suite's usual teardown: where the guard is EMPTY the integration
    // installs, and its preexec would stamp OSC marks into the asserted stdout.
    let mut lanes = vec![(
        bash_shell(),
        &["--noprofile", "--norc", "-i"][..],
        "aterm_shell_integration.bash",
        "source \"$ATERM_TEST_SCRIPT\" >/dev/null; \
         __aterm_in_prompt_cmd=1; trap - DEBUG 2>/dev/null || true; PROMPT_COMMAND=; \
         printf 'mux=%s outer=%s base=%s' \"${ATERM_MUX-UNSET}\" \
         \"${ATERM_MUX_OUTER_SESSION_ID-UNSET}\" \"${ATERM_MUX_BASE-UNSET}\"",
    )];
    if let Some(zsh) = zsh_shell() {
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
    if let Some(fish) = fish_shell() {
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
            // fish's report leaves the base out.
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

// ---------------------------------------------------------------------------
// LIVE: the tab that is ALREADY OPEN picks the managed dirs up.
//
// Owner: "aterm atpkg DID install the latest but it didn't make them available
// for me. instead, it is telling me to open a new tab. NO! all the latest and
// best MUST WORK IN THE SAME TAB with live update! fix this and this message and
// audit that this is the actual behavior." Measured: the tab's zsh (pid 1784)
// was spawned 10:44:24 by the previous app build and adopted across the seamless
// update (app 10:44:32); <prefix>/agents and the shell.d hooks were created at
// 10:46 by the new build — and nothing in the running shell ever learned of them.
// ---------------------------------------------------------------------------

/// The atpkg hook in `crates/atpkg/src/hooks.rs`'s EXACT format — a golden of
/// `hook_files()` for three fixture paths, pinned to that function from atpkg's side
/// (`crates/atpkg/tests/shell_integration_hook_pin.rs`), with the fixture paths
/// substituted here. `aterm-shell-integration` cannot depend on `atpkg`.
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

/// One live, INTERACTIVE shell driven over pipes: commands go in one line at a
/// time, and the test waits for each command's own output before it changes the
/// world and sends the next — so a directory laid between two commands is laid
/// while the shell sits at an idle prompt, exactly the owner's tab. Interactive
/// (`-i`) with a piped stdin still runs every hook this crate installs (measured
/// on zsh 5.9 and bash 3.2.57: precmd/PROMPT_COMMAND fire before every prompt,
/// preexec/the DEBUG trap before every command); fish needs a real pty for its
/// `fish_prompt`/`fish_preexec` events and is wrapped in `script` for that.
#[cfg(unix)]
struct LiveShell {
    label: String,
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    /// The hermetic home the shell's environment points into, when the
    /// [`ShellCommand`] that made it is gone ([`Self::holding`]).
    home: Option<aterm_tempfile::TempDir>,
}

// `#[cfg(test)]` is redundant for the compiler — this file is `include!`d into
// lib.rs's `#[cfg(test)] mod tests` — but it is what the wasm-process census
// READS. That census walks `crates/**/*.rs`, so it sees this file on its own,
// where the enclosing gate is nowhere in sight and the `thread::spawn` below
// looks like a thread in the shipped wasm closure (OB-12). The marker states
// here what the module states there.
#[cfg(test)]
#[cfg(unix)]
fn pump(
    mut reader: impl std::io::Read + Send + 'static,
) -> std::sync::Arc<std::sync::Mutex<Vec<u8>>> {
    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = std::sync::Arc::clone(&sink);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .extend_from_slice(&buf[..n]),
            }
        }
    });
    sink
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
    /// A hang detector, never a latency budget: a marker the shell must print
    /// and the exit it must take each end the wait the moment they happen.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    /// The shell holds its three pipes and nothing else of this process
    /// (`inherit_nothing`; the callers' `Command`s are std's, built with the
    /// hermetic [`ShellCommand`]): a std child on a std without the SP-1 strip
    /// keeps any descriptor another test thread had open unflagged at the
    /// spawn, and a live shell would hold that pipe as long as its test runs.
    fn spawn(label: &str, cmd: &mut Command) -> Self {
        use aterm_uds::spawnfd::InheritNothing as _;
        cmd.inherit_nothing()
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|error| panic!("{label}: spawn: {error}"));
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = pump(child.stdout.take().expect("piped stdout"));
        let stderr = pump(child.stderr.take().expect("piped stderr"));
        Self {
            label: label.to_owned(),
            child,
            stdin,
            stdout,
            stderr,
            home: None,
        }
    }

    /// Keep `home` — the [`ShellCommand`]'s, which this shell's environment
    /// points into — for as long as this shell is held.
    fn holding(mut self, home: aterm_tempfile::TempDir) -> Self {
        self.home = Some(home);
        self
    }

    fn stderr_text(&self) -> String {
        let err = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&err).into_owned()
    }

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
            "--- stdout ---\n{}\n--- stderr ---\n{}",
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

    /// Wait until `marker` appears in stdout at or after byte offset `from`;
    /// returns the offset just past it. Panics with the whole transcript on
    /// timeout or when the shell exits first.
    fn wait_for_after(&mut self, marker: &str, from: usize) -> usize {
        let started = std::time::Instant::now();
        loop {
            {
                let out = self
                    .stdout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(pos) = find_bytes(&out[from.min(out.len())..], marker.as_bytes()) {
                    return from + pos + marker.len();
                }
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "{}: shell exited ({status}) before {marker:?} appeared\n{}",
                    self.label,
                    self.transcript()
                );
            }
            assert!(
                started.elapsed() < Self::TIMEOUT,
                "{}: timed out waiting for {marker:?}\n{}",
                self.label,
                self.transcript()
            );
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    /// The text after the LAST `key` before `end` (the OSC 633;E mark quotes the
    /// command line, which carries `key` too — but the output comes after it),
    /// cut at the first control byte.
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
        self.send("exit");
        let started = std::time::Instant::now();
        loop {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            if started.elapsed() > Self::TIMEOUT {
                let _ = self.child.kill();
                panic!(
                    "{}: did not exit after `exit`\n{}",
                    self.label,
                    self.transcript()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }
}

/// The owner's machine in a temp dir: a home whose rc prepends a FOREIGN `claude`
/// (their ~/.zshrc prepends ~/.local/bin), and a managed prefix that does not
/// exist yet — atpkg's first pass lays it while the shell is already running.
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
        // A prefix with a SPACE, like the real one under "Application Support".
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

    /// atpkg's pass, as the running shell sees it: agents/ + reroute/ (+ its
    /// marker) + bin/ created, the twin laid, then the hook written temp+rename.
    fn lay(&self, hook_ext: &str, golden: &str, twin_says: &str) {
        self.lay_dirs();
        self.lay_twin(twin_says);
        self.lay_hook(hook_ext, golden);
    }

    /// The three managed dirs (agents/ EMPTY — no twin yet) and the reroute marker:
    /// what the spawn seam / the seed pass creates before any program is installed.
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

    /// The twin, atomically (`stand_in` lays it by temp + rename), the way atpkg
    /// re-lays it on update.
    fn lay_twin(&self, says: &str) {
        write_executable(
            &self.agents.join("claude"),
            &format!("#!/bin/sh\necho {says}\n"),
        );
    }

    /// The seam's reroute dir with its marker and ONE stub in it (`aterm help
    /// reroute`): what a session shell inherits before any agent program exists.
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

/// An interactive zsh through the crate's own ZDOTDIR wrapper, reading the
/// fixture home's .zshrc — `rc_prelude` (a user's `setopt` lines, say) then the
/// foreign prepend — exactly the real launch shape.
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
    // This interactive fixture reads commands from a pipe. An inherited
    // controlling TTY must not redirect ZLE's reads or engage job control:
    // background test runs otherwise hang or exit with a TTY read error.
    // Keep -i so the real precmd/preexec hooks still run on every cycle.
    cmd.args(["-i", "-o", "NO_ZLE", "-o", "NO_MONITOR"])
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    // The wrapper restores ZDOTDIR to the test home, so the shell reads ITS
    // .zshrc (the foreign prepend) — the real launch shape.
    cmd.env_remove("ATERM_UNSET_ZDOTDIR")
        .env("ATERM_ORIGINAL_ZDOTDIR", &fx.home);
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("zsh", &mut cmd).holding(cmd.into_home())
}

/// An interactive bash through the crate's own `--rcfile` wrapper (which sources
/// /etc/profile — on macOS, path_helper — then the home's .bashrc: `rc_prelude` (a
/// user's `set -u`, say) then the foreign prepend, exactly the owner's shape),
/// inside a session; `extra` is applied last, so it can override PATH.
#[cfg(unix)]
fn spawn_live_bash(fx: &LiveFixture, rc_prelude: &str, extra: &[(&str, &str)]) -> LiveShell {
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

/// A home shaped like Ubuntu's `/etc/skel`: `~/.profile` reads `~/.bashrc`, and
/// `~/.bash_logout` leaves a mark. Each startup file says when it runs.
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

/// bash as aterm starts it with `shell_args` = `args` ([`merge_shell_args`]):
/// the integration's argv and env, and the merge's.
#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_bash_with_shell_args(fx: &LiveFixture, args: &[&str]) -> LiveShell {
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
    let mut cmd = shell_command(bash_shell());
    cmd.args(&argv[1..])
        .env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "dumb")
        .env("ATERM_CHILD", "1")
        .env_remove("BASH_ENV")
        .env_remove("ENV")
        .env_remove("HISTFILE");
    for (key, value) in injection.env_add {
        cmd.env(key, value);
    }
    LiveShell::spawn("bash", &mut cmd).holding(cmd.into_home())
}

/// Off macOS bash starts as every terminal there starts it — an interactive
/// NON-login shell — and reads each startup file once: the loader reads
/// `~/.bashrc` and not `~/.profile`, which on Ubuntu reads `~/.bashrc` again (the
/// old login-style wrapper ran it twice, and `/etc/bash.bashrc` twice with it).
/// With `shell_args = ["-l"]` it is a real login shell: `~/.profile` once, through
/// it `~/.bashrc` once, `logout` works and `~/.bash_logout` runs. The integration
/// loads in both.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn bash_reads_each_startup_file_once_non_login_and_login() {
    let probe = "echo \"LOGIN=$(shopt -q login_shell && echo y || echo n) \
                 POSIX=$(shopt -qo posix && echo y || echo n) \
                 HOOKED=${ATERM_SHELL_INTEGRATION_INSTALLED:+y}\"; echo MARK$((40 + 2))";
    let count =
        |sh: &LiveShell, at: usize, what: &str| stdout_between(sh, 0, at).matches(what).count();

    let fx = LiveFixture::new();
    skel_shaped_home(&fx);
    let mut sh = spawn_bash_with_shell_args(&fx, &["-i"]);
    sh.send(probe);
    let at = sh.wait_for_after("MARK42", 0);
    assert_eq!(count(&sh, at, "RAN-BASHRC"), 1, "{}", sh.transcript());
    assert_eq!(count(&sh, at, "RAN-PROFILE"), 0, "{}", sh.transcript());
    assert_eq!(sh.value_before("LOGIN=", at), "n POSIX=n HOOKED=y");
    sh.finish();

    let fx = LiveFixture::new();
    skel_shaped_home(&fx);
    let mut sh = spawn_bash_with_shell_args(&fx, &["-l", "-i"]);
    sh.send(probe);
    let at = sh.wait_for_after("MARK42", 0);
    assert_eq!(count(&sh, at, "RAN-BASHRC"), 1, "{}", sh.transcript());
    assert_eq!(count(&sh, at, "RAN-PROFILE"), 1, "{}", sh.transcript());
    assert_eq!(sh.value_before("LOGIN=", at), "y POSIX=n HOOKED=y");
    sh.send("logout");
    let started = std::time::Instant::now();
    while sh.child.try_wait().ok().flatten().is_none() {
        assert!(
            started.elapsed() < LiveShell::TIMEOUT,
            "bash: `logout` did not end the login shell\n{}",
            sh.transcript()
        );
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    assert_eq!(
        std::fs::read_to_string(fx.home.join("logout-mark"))
            .ok()
            .as_deref(),
        Some("LOGGED-OUT\n"),
        "~/.bash_logout ran\n{}",
        sh.transcript()
    );

    // `--rcfile ~/.myrc`, as TOML spells it: bash tilde-expands the file it is
    // handed, and so does the loader that reads it in its place (it read
    // neither ~/.myrc nor ~/.bashrc).
    let fx = LiveFixture::new();
    skel_shaped_home(&fx);
    std::fs::write(fx.home.join(".myrc"), "echo RAN-MYRC\n").expect(".myrc");
    let mut sh = spawn_bash_with_shell_args(&fx, &["--rcfile", "~/.myrc", "-i"]);
    sh.send(probe);
    let at = sh.wait_for_after("MARK42", 0);
    assert_eq!(count(&sh, at, "RAN-MYRC"), 1, "{}", sh.transcript());
    assert_eq!(count(&sh, at, "RAN-BASHRC"), 0, "{}", sh.transcript());
    assert_eq!(sh.value_before("LOGIN=", at), "n POSIX=n HOOKED=y");
    sh.finish();
}

/// zsh/stat is loaded at the first stamp of a hook that exists, never for an
/// absent one: a session shell with no hook to stamp loads nothing, at load or
/// at a prompt — the first tab of a fresh machine, which opens before atpkg lays
/// the hook — and the same shell loads it, and sources the hook, at the first
/// prompt after one is laid. A zsh whose module will not load (a `zmodload`
/// that refuses it stands in) sources that hook at the same prompt, through the
/// `-f` fallback.
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
/// An interactive fish inside a session. fish's `fish_prompt`/`fish_preexec`
/// events fire only from its interactive reader, which needs a real pty, so the
/// shell runs under `script` (macOS and util-linux spellings). The integration
/// arrives the way aterm delivers it — the vendor conf.d on XDG_DATA_DIRS — and
/// the foreign prepend lives in the home's config.fish. TERM=dumb, like the
/// other two lanes: under a terminal-class TERM fish 4.9.3 queries Primary
/// Device Attributes and waits 10 s for the reply nobody here sends before its
/// first prompt (measured 2026-09-16: 10.1 s to the prompt against 0.04 s under
/// dumb) — three spawns spent 30 of the lane's 32 s idle and left the first
/// `wait_for_after` half its budget. The OSC 133;A marker is the integration's
/// own, not fish's terminal layer's, so it still arrives (pinned below).
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
    let mut cmd = Command::new("script");
    if cfg!(target_os = "macos") {
        cmd.args(["-q", "/dev/null", fish, "-i"]);
    } else {
        cmd.args(["-q", "-c", &format!("{fish} -i"), "/dev/null"]);
    }
    // The hermetic environment of `shell_command`, applied to `script` (fish
    // inherits it), then the fixture's home and the conf.d injection.
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
            // Narrowed to the injection's own conf.d plus the hermetic data dir:
            // `prepare_fish` folds in the TEST PROCESS's XDG_DATA_DIRS otherwise.
            cmd.env(
                key,
                format!(
                    "{}:{}",
                    fx.base.join("fish-xdg").display(),
                    hermetic.home().join("data").display()
                ),
            );
        } else {
            cmd.env(key, value);
        }
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    LiveShell::spawn("fish", &mut cmd).holding(hermetic.into_home())
}

#[cfg(unix)]
const ZSH_PRINT_PATH: &str = "print -r -- \"LIVEPATH=$PATH\"; print -r -- \"LIVERR=$ATERM_REROUTE_DIR\"; print -r -- \"MARK$((40+2))\"";
#[cfg(unix)]
const BASH_PRINT_PATH: &str = "printf 'LIVEPATH=%s\\n' \"$PATH\"; printf 'LIVERR=%s\\n' \"$ATERM_REROUTE_DIR\"; echo \"MARK$((40+2))\"";
#[cfg(unix)]
const FISH_PRINT_PATH: &str = "printf 'LIVEPATH=%s\\n' (string join : -- $PATH); printf 'LIVERR=%s\\n' \"$ATERM_REROUTE_DIR\"; echo MARK(math 40 + 2)";

#[cfg(unix)]
fn script_can_allocate_a_pty() -> bool {
    Command::new("script").arg("-V").output().is_ok()
        || Command::new("script").arg("--version").output().is_ok()
}

/// [`zsh_shell`] for a test that needs a real zsh: COULD NOT RUN where the host
/// has none, never a silent pass.
#[cfg(unix)]
fn need_zsh() -> &'static str {
    zsh_shell().unwrap_or_else(|| {
        aterm_spec::host::could_not_run("no zsh on this host (ATERM_TEST_ZSH=<zsh> names one)")
    })
}

/// [`fish_shell`] for a test that needs a real fish: COULD NOT RUN where the
/// host has none (install fish, or `ATERM_TEST_FISH=<fish>` names one), never a
/// silent pass.
#[cfg(unix)]
fn need_fish() -> &'static str {
    fish_shell().unwrap_or_else(|| {
        aterm_spec::host::could_not_run(
            "no fish on this host (install fish, or ATERM_TEST_FISH=<fish> names one)",
        )
    })
}

/// A `script(1)` that can allocate a pty, for a live-pty test: COULD NOT RUN
/// where the host has none, never a silent pass.
#[cfg(unix)]
fn need_pty_script() {
    if !script_can_allocate_a_pty() {
        aterm_spec::host::could_not_run("no `script` that can allocate a pty on this host");
    }
}

/// A live `label` shell ("bash", "zsh" or "fish") inside a session, with no rc
/// prelude, and the line that prints its PATH — or `None` where that shell (or,
/// for fish, the `script` that gives it a pty) is not installed.
#[cfg(unix)]
fn spawn_live(
    label: &str,
    fx: &LiveFixture,
    extra: &[(&str, &str)],
) -> Option<(LiveShell, &'static str)> {
    match label {
        "bash" => Some((spawn_live_bash(fx, "", extra), BASH_PRINT_PATH)),
        "zsh" => zsh_shell().map(|zsh| (spawn_live_zsh(zsh, fx, "", extra), ZSH_PRINT_PATH)),
        _ => fish_shell()
            .filter(|_| script_can_allocate_a_pty())
            .map(|fish| (spawn_live_fish(fish, fx, extra), FISH_PRINT_PATH)),
    }
}

/// Print PATH and `$ATERM_REROUTE_DIR` in the live shell, from `at` on: reroute,
/// agents, …, bin last, each once, and the reroute dir derived beside agents/.
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

/// THE OWNER'S SCENARIO in one live bash or zsh (`label`, the hook's extension),
/// started inside a session with no ATPKG_AGENTS, no ATERM_REROUTE_DIR, no hook
/// and no agents dir, its rc putting a foreign `claude` first: `claude` → foreign;
/// a whole idle prompt cycle with nothing laid; atpkg's pass lays the hook
/// (hooks.rs's exact format), agents/ with a twin and reroute/ with its marker;
/// `claude` → "managed 1" (preexec's pickup); the twin re-laid temp+rename →
/// "managed 2"; the hook REWRITTEN under the live shell (a later pass) →
/// re-sourced (zsh sees the new inode, bash compares the text — residual R1 of
/// 2026-09-16, closed); then PATH and the derived reroute dir.
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

/// The owner's scenario in zsh, through the crate's own ZDOTDIR wrapper; then the
/// two negatives: outside a session (no ATERM_CHILD) nothing happens live, and an
/// engaged __ATERM_REROUTE_PASSTHROUGH (the `aterm --no-reroute` marker) fronts
/// agents/ but derives no reroute dir.
#[cfg(unix)]
#[test]
fn test_zsh_already_running_session_shell_picks_up_the_managed_dirs_live() {
    let zsh = need_zsh();
    let spawn = |fx: &LiveFixture, extra: &[(&str, &str)]| spawn_live_zsh(zsh, fx, "", extra);

    let fx = LiveFixture::new();
    let mut sh = spawn(&fx, &[("ATERM_CHILD", "1")]);
    posix_owner_scenario("zsh", &mut sh, &fx, ZSH_PRINT_PATH);
    sh.finish();

    // Outside a session: inert. The hook and dirs appear; `claude` stays foreign.
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

    // The `--no-reroute` marker engaged: agents/ is fronted, no reroute dir derived.
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

/// The owner's scenario in bash under a user's `set -u` (in .bashrc, so it
/// governs the script's load AND every prompt). Before 2026-09-16 the prompt
/// command aborted on the unbound `__aterm_pending_prompt_setup` at every prompt,
/// leaving `__aterm_in_prompt_cmd=1` stuck and the DEBUG-trap preexec — the
/// per-command re-assert — dead: the `claude` typed after the twins landed ran
/// the FOREIGN copy. Pinned: the whole scenario holds, and stderr carries no
/// "unbound variable".
#[cfg(unix)]
#[test]
fn test_bash_live_pickup_holds_under_set_u() {
    let fx = LiveFixture::new();
    let mut sh = spawn_live_bash(&fx, "set -u\n", &[]);
    posix_owner_scenario("bash", &mut sh, &fx, BASH_PRINT_PATH);
    let err = sh.stderr_text();
    assert!(
        !err.contains("unbound variable"),
        "bash: the integration must be nounset-clean at load and at every prompt:\n{err}"
    );
    sh.finish();
}

/// No hook at all — a fresh machine before any agent program is installed — and
/// the seam's ATERM_REROUTE_DIR leading PATH with a `cargo` stub in it. The user
/// prepends a dir holding its own `cargo` AT THE PROMPT (a `. ~/.cargo/env`, a
/// tool installer): the very next `cargo` must still run the stub (preexec's
/// re-front), and so must one typed a prompt cycle later. Review finding
/// 2026-09-16: bash and fish returned from the hot path's step 1 whenever
/// $ATPKG_AGENTS was unset, hook or no hook, so the prepend shadowed the stubs
/// for the life of the shell — only zsh held.
#[cfg(unix)]
#[test]
fn test_reroute_stubs_survive_a_prompt_time_prepend_with_no_hook() {
    for label in ["bash", "zsh", "fish"] {
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
        let Some((mut sh, print)) = spawn_live(label, &fx, &env) else {
            continue;
        };
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
            "{label}: the reroute dir must lead PATH after the prepend: {path:?}"
        );
        assert!(
            path.split(':')
                .any(|e| e == fx.foreign.to_str().expect("UTF-8")),
            "{label}: the prepended dir is demoted, not removed: {path:?}"
        );
        sh.finish();
    }
}

/// The OTHER order the owner's gesture reaches: the hook and an EMPTY agents/
/// exist when the shell starts — a fresh machine creates agents/ at launch and
/// lays the twin only once the managed program is installed — so `claude` runs
/// the foreign copy and the shell HASHES that path. The twin lands, with NO hook
/// rewrite: `claude` must run it. Before the twin watch both shells kept the
/// hashed foreign path for the life of the shell (measured: zsh 5.9 and bash
/// 3.2.57). Also pinned: the steady state assigns nothing — a manual hash entry
/// (`zzz`) survives two idle prompt cycles — and, for zsh, a user rc with
/// `setopt ksh_arrays warn_create_global` neither truncates PATH nor prints a
/// "created globally" line (the hot path runs under `emulate -L zsh`).
#[cfg(unix)]
#[test]
fn test_twin_landing_in_an_agents_dir_already_leading_path_is_run_live() {
    let mut lanes = vec![("bash", None)];
    if let Some(zsh) = zsh_shell() {
        lanes.push(("zsh", Some(zsh)));
    }
    for (label, zsh) in lanes {
        let fx = LiveFixture::new();
        fx.lay_dirs();
        fx.lay_hook(label, ATPKG_HOOK_POSIX_GOLDEN);
        let foreign = fx.foreign.join("claude");
        let (mut sh, hash_foreign, print) = match zsh {
            Some(zsh) => (
                spawn_live_zsh(
                    zsh,
                    &fx,
                    "setopt ksh_arrays warn_create_global\n",
                    &[("ATERM_CHILD", "1")],
                ),
                format!("hash zzz={}", foreign.display()),
                ZSH_PRINT_PATH,
            ),
            None => (
                spawn_live_bash(&fx, "", &[]),
                format!("hash -p {} zzz", foreign.display()),
                BASH_PRINT_PATH,
            ),
        };
        sh.send("claude");
        let at = sh.wait_for_after("foreign", 0);
        sh.send(&format!("{hash_foreign}; echo IDLE-$((1+0))"));
        let at = sh.wait_for_after("IDLE-1", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        sh.send("echo IDLE-$((2+0))");
        let at = sh.wait_for_after("IDLE-2", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        // The hash table survived two steady prompts: nothing was assigned or flushed.
        sh.send("zzz");
        let at = sh.wait_for_after("foreign", at);
        fx.lay_twin("managed 1");
        sh.send("claude");
        let at = sh.wait_for_after("managed 1", at);
        fx.lay_twin("managed 2");
        sh.send("claude");
        let at = sh.wait_for_after("managed 2", at);
        assert_live_path(label, &mut sh, &fx, at, print);
        let err = sh.stderr_text();
        assert!(
            !err.contains("created globally"),
            "{label}: sourcing the hook from the hot path must not trip warn_create_global:\n{err}"
        );
        sh.finish();
    }
}

/// The hook exists at spawn but names an agents/ atpkg has NOT created yet (a
/// hook written on a machine with no agent program installed, or a wiped managed
/// prefix): `claude` → foreign; agents/ + reroute/ + bin/ and the twin appear
/// with NO hook rewrite; `claude` must run the twin. Before the re-probe,
/// `$__aterm_managed_want` stayed empty and the hot path returned early forever.
#[cfg(unix)]
#[test]
fn test_hook_naming_an_agents_dir_that_appears_later_is_fronted_live() {
    for label in ["bash", "zsh"] {
        let fx = LiveFixture::new();
        fx.lay_hook(label, ATPKG_HOOK_POSIX_GOLDEN);
        let Some((mut sh, print)) = spawn_live(label, &fx, &[("ATERM_CHILD", "1")]) else {
            continue;
        };
        sh.send("claude");
        let at = sh.wait_for_after("foreign", 0);
        sh.send("echo IDLE-$((1+0))");
        let at = sh.wait_for_after("IDLE-1", at);
        let at = sh.wait_for_after("\x1b]133;A", at);
        fx.lay_dirs();
        fx.lay_twin("managed 1");
        sh.send("claude");
        let at = sh.wait_for_after("managed 1", at);
        assert_live_path(label, &mut sh, &fx, at, print);
        sh.finish();
    }
}

/// The same three orders in fish, when installed: the owner's scenario — with
/// the hook REWRITTEN under the live shell and re-sourced (fish reads the hook's
/// text with `read -z` and compares; residual R1 of 2026-09-16, closed); the hook
/// and an empty agents/ at spawn with the twin landing later (fish keeps no
/// command hash, so this is the order check plus PATH's own walk); and the hook
/// first, the dirs later (the re-probe).
#[cfg(unix)]
#[test]
fn test_fish_already_running_session_shell_picks_up_the_managed_dirs_live() {
    let fish = need_fish();
    need_pty_script();
    // 1. The owner's scenario.
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
    // The hook rewritten under the live shell (a later pass): re-sourced.
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

    // 2. Hook + empty agents/ at spawn; the twin lands later, no hook rewrite.
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

    // 3. The hook first, naming an agents/ that does not exist yet.
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

// ─── The re-key channel ───

#[cfg(unix)]
const REKEY_OLD: &str = "1111111111111111111111111111111111111111111111111111111111111111";
#[cfg(unix)]
const REKEY_NEW: &str = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

/// The line that prints the globals a mark is signed with, then a marker whose
/// number is computed (so the echoed command line never matches it).
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

/// Hands a live shell `line` (a re-key's key, or a body's address) and its
/// newline at `path`, renamed into place whole, as the host writes both
/// (aterm-gui's `shell_rekey::deliver`). Written in place, a prompt's check at
/// times read the file before its bytes landed — an empty line, consumed like
/// any malformed one — and the shell never took what the test handed it.
#[cfg(unix)]
fn deliver(path: &Path, line: &str) {
    let part = path.with_extension("part");
    std::fs::write(&part, format!("{line}\n")).unwrap();
    std::fs::rename(&part, path).unwrap();
}

/// Each live shell spawned with the re-key channel: the nonce it was spawned with
/// and the path scrubbed from its environment; a malformed key is consumed and
/// changes nothing; a key written while it idles is taken at its NEXT prompt —
/// the globals change, the file is gone, and the marks after it carry
/// `;id=<new>`.
#[cfg(unix)]
#[test]
fn a_live_shell_takes_a_rekey_at_its_next_prompt() {
    for label in ["bash", "zsh", "fish"] {
        let fx = LiveFixture::new();
        let file = fx.home.join(format!("rekey-{label}"));
        let path = file.to_str().unwrap().to_string();
        let extra = [
            ("ATERM_SHELL_NONCE", REKEY_OLD),
            ("ATERM_REKEY_PATH", path.as_str()),
        ];
        let Some((mut sh, _)) = spawn_live(label, &fx, &extra) else {
            continue;
        };
        let (line, marker) = rekey_probe(label, 1);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, 0);
        assert_eq!(sh.value_before("NONCE=", at), REKEY_OLD, "{label}: spawned");
        assert_eq!(sh.value_before("ENV=", at), "", "{label}: path scrubbed");

        deliver(&file, "NOT-A-KEY");
        sh.send("true");
        let (line, marker) = rekey_probe(label, 2);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_OLD,
            "{label}: a bad key changes nothing"
        );
        assert!(!file.exists(), "{label}: a bad key is consumed");

        deliver(&file, REKEY_NEW);
        sh.send("true");
        let (line, marker) = rekey_probe(label, 3);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(
            sh.value_before("NONCE=", at),
            REKEY_NEW,
            "{label}: re-keyed"
        );
        assert_eq!(
            sh.value_before("SUFFIX=", at),
            format!(";id={REKEY_NEW}"),
            "{label}"
        );
        assert!(!file.exists(), "{label}: the key file is removed once read");
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
            "{label}: marks carry the new key: {tail:?}"
        );
        sh.finish();
    }
}

/// bash's default prompt carries ONE 133;B however many prompts it has drawn:
/// each prompt takes the previous mark off PS1 before it appends its own. The
/// strip once matched nothing (its pattern's backslashes read as glob escapes),
/// so PS1 gained a mark per prompt and every prompt re-sent them all; then, as a
/// regex, nothing in a PS1 holding a byte the tab's UTF-8 locale cannot decode
/// (a Latin-1 `É`, under the UTF-8 `LC_CTYPE` aterm's spawn seam forces). That
/// PS1 starts with such a byte before a backslash, which bash 5's `%` empties.
#[cfg(unix)]
#[test]
fn a_live_bash_prompt_carries_one_133b_however_many_prompts() {
    let utf8 = if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    };
    for rc in ["", concat!(r"PS1=$'\xc9\\u \\$ '", "\n")] {
        let fx = LiveFixture::new();
        let mut sh = spawn_live_bash(
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
                    ps1.contains(r"\u \$ ") || rc.is_empty()
                ),
                (1, true),
                "rc {rc:?}, prompt {n}: {ps1:?}\n{}",
                sh.transcript()
            );
        }
        sh.finish();
    }
}

// ─── The typed re-key ───

/// The typed re-key exists only for a shell with a POSIX-or-fish integration
/// script: typed into PowerShell, cmd, a WSL launcher or an unknown program,
/// the line would be a syntax error at the user's prompt.
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

/// POSIX single quotes, `'\''` for a quote: the quoting the harness's relaunch
/// line uses.
#[cfg(unix)]
fn posix_quoted(path: &Path) -> String {
    format!("'{}'", path.to_str().expect("UTF-8").replace('\'', r"'\''"))
}

/// A LIVE SHELL WITHOUT THE CHANNEL — spawned as every shell before 0.94 was,
/// with its nonce and no `ATERM_REKEY_PATH` — never reads the key file at a
/// prompt of its own, and takes the key from the typed line: the globals change
/// at once, the file is gone, and the marks of the very next prompt carry the new
/// id. The path has a space and a quote in it (the real control dir sits under
/// "Application Support"). A second line naming a file that is not there — a key
/// withdrawn before the line ran — changes nothing and prints no error, and the
/// command after it still runs.
#[cfg(unix)]
#[test]
fn a_live_shell_without_the_channel_takes_a_typed_rekey() {
    for (label, shell) in [("bash", ShellType::Bash), ("zsh", ShellType::Zsh)] {
        let fx = LiveFixture::new();
        let Some((mut sh, _)) = spawn_live(label, &fx, &[("ATERM_SHELL_NONCE", REKEY_OLD)]) else {
            continue;
        };
        let file = fx.home.join("re key's");
        std::fs::write(&file, format!("{REKEY_NEW}\n")).unwrap();
        sh.send("true");
        let (line, marker) = rekey_probe(label, 1);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, 0);
        assert_eq!(sh.value_before("NONCE=", at), REKEY_OLD, "{label}: before");
        assert!(
            file.exists(),
            "{label}: no prompt of its own reads the file"
        );

        let typed = typed_rekey(shell, &posix_quoted(&file)).expect("scripted");
        sh.send(&format!("{typed} echo RAN$((200 + 1))"));
        let at = sh.wait_for_after("RAN201", at);
        let (line, marker) = rekey_probe(label, 2);
        sh.send(&line);
        let at = sh.wait_for_after(&marker, at);
        assert_eq!(sh.value_before("NONCE=", at), REKEY_NEW, "{label}: taken");
        assert_eq!(
            sh.value_before("SUFFIX=", at),
            format!(";id={REKEY_NEW}"),
            "{label}"
        );
        assert!(!file.exists(), "{label}: the one-use file is gone");
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
            "{label}: the next prompt's marks carry the new key: {tail:?}"
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
            "{label}: a missing file changes nothing"
        );
        // (bash's line editor echoes the typed line to stderr, path and all; an
        // error would name the missing file.)
        let err = sh.stderr_text().to_lowercase();
        assert!(!err.contains("no such file"), "{label}: quiet: {err}");
        sh.finish();
    }
}

// ─── The content-addressed script folder ───

/// The address is what collection (`collect_old_script_sets`) and the loaders
/// (`is_integration_rev`) recognise a script folder by: exactly 16 lowercase hex
/// digits, and nothing else — not a scratch folder, not a near miss.
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

/// THE MEASURED DEFECT: a foreign start rewrote the shared folder and every new
/// tab of the running window sourced its scripts. Now a new window (a fresh memo)
/// and the running one (a warm memo) both keep pointing their shells at THIS
/// build's folder, whose bytes are this build's, whatever a foreign build wrote —
/// an older aterm to the legacy flat files (left for the builds that read them),
/// a build of this scheme to its own content folder. And a folder deleted mid-run
/// (by a person, or by another build's collection) is installed again by the
/// next spawn: the warm memo is checked against the disk, never trusted alone.
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

/// A content folder that exists but does not hold this set — a wrapper edited by
/// hand, a file half-written by a writer that died — is replaced on the first
/// `prepare` of a process, never trusted (every file of the set is checked, not
/// just the script); no scratch folder survives; racing writers of the same set
/// all land on one folder.
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

/// No two writers share a scratch folder: suffixes drawn at once by many
/// threads of one process — the same clock tick, the same pid — are all
/// distinct (the race the test above can only catch under load: two threads on
/// one microsecond wrote into one scratch folder, and the first rename took it
/// from under the second).
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

/// Collection keeps this build's folder, the five newest others and everything
/// younger than two weeks; it removes older folders beyond those and dead
/// writers' scratch folders, and never touches the legacy flat files.
/// Unix only: stamping a folder's mtime opens it with `File::open`, which
/// Windows refuses for a directory without `FILE_FLAG_BACKUP_SEMANTICS`.
#[cfg(unix)]
#[test]
fn old_script_folders_are_collected_and_young_or_newest_ones_kept() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().join("si");
    install_script_set(&root).unwrap();
    std::fs::write(root.join("aterm_shell_integration.zsh"), "legacy").unwrap();
    // Distinct mtimes, oldest first, stamped rather than waited for: a coarse
    // filesystem clock would tie folders created back to back.
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

    // Now: everything is young, nothing goes.
    collect_old_script_sets(&root, std::time::SystemTime::now());
    assert!(others.iter().all(|d| d.exists()) && scratch.exists());

    // A month on: the three oldest others and the scratch folder go.
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
