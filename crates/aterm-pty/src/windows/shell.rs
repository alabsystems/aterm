// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shell selection + argv mapping for the Windows spawn.
//!
//! Selection order: `%ATERM_SHELL%` → `pwsh.exe` → `powershell.exe` →
//! `%COMSPEC%` → literal `cmd.exe`. `%SHELL%` is deliberately NOT consulted: in
//! git-bash/MSYS sessions it holds a POSIX path (`/usr/bin/bash`) that
//! `CreateProcessW` cannot exec. No login-dash `argv[0]` either — that is a
//! POSIX login-shell convention; PowerShell/cmd would treat it as a bad path.
//! Default interactive argv is the bare `[shell]` (transparency, same as
//! Windows Terminal). The pure pieces (family detection, argv shapes, the
//! path-like classifier) are unit-tested below; only `SearchPathW` touches the
//! OS.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::Path;

use super::cmdline::wide_nul;
use super::ffi;

/// The shell family an `%ATERM_EXEC%` command is mapped through (detected from
/// the lowercased file stem of the resolved shell path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellFamily {
    /// `pwsh` / `powershell` — `-NoExit -Command <cmd>`.
    Pwsh,
    /// `cmd` — `/K <cmd>`.
    Cmd,
    /// Anything else: `%ATERM_EXEC%` is IGNORED (bare shell) — we cannot know a
    /// foreign shell's "run this then stay interactive" flag, and guessing one
    /// would garble its argv.
    Other,
}

/// Classify `shell` by its file stem (case-insensitive).
pub(crate) fn shell_family(shell: &OsStr) -> ShellFamily {
    let stem = Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase);
    match stem.as_deref() {
        Some("pwsh" | "powershell") => ShellFamily::Pwsh,
        Some("cmd") => ShellFamily::Cmd,
        _ => ShellFamily::Other,
    }
}

/// The argv for "run `%ATERM_EXEC%`'s command, then stay interactive", by
/// resolved shell family (see [`ShellFamily`]).
pub(crate) fn aterm_exec_argv(shell: &OsStr, cmd: &OsStr) -> Vec<OsString> {
    match shell_family(shell) {
        ShellFamily::Pwsh => vec![
            shell.to_os_string(),
            OsString::from("-NoExit"),
            OsString::from("-Command"),
            cmd.to_os_string(),
        ],
        ShellFamily::Cmd => vec![
            shell.to_os_string(),
            OsString::from("/K"),
            cmd.to_os_string(),
        ],
        ShellFamily::Other => vec![shell.to_os_string()],
    }
}

/// Whether the resolved shell is `wsl.exe` (by file stem, case-insensitive) —
/// the one shell whose `shell_args` are launcher options rather than shell
/// options. See [`resolve_spawn_target`].
pub(crate) fn is_wsl_shell(shell: &OsStr) -> bool {
    Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("wsl"))
}

/// Whether `name` is used VERBATIM (an explicit absolute/relative path) rather
/// than PATH-searched: it contains a separator (`\` or `/`) or a drive colon.
pub(crate) fn is_path_like(name: &str) -> bool {
    name.contains('\\') || name.contains('/') || name.contains(':')
}

/// PATH-resolve a `-e` program name, the Windows twin of the Unix
/// `resolve_program`: a path-like name is used verbatim; otherwise
/// `SearchPathW` probes with `.exe` first, then each remaining `%PATHEXT%`
/// entry — the `.cmd`/`.bat` shims Node tooling ships (npm/yarn/pnpm) live only
/// there, and `CreateProcessW` never PATH-searches a bare `lpApplicationName`
/// itself (it does run a RESOLVED full `.cmd`/`.bat` path fine). Falls back to
/// the name verbatim when nothing matches, so `CreateProcessW` fails cleanly
/// (the `_exit(127)` analog) instead of this resolver masking a not-found
/// command.
pub(crate) fn resolve_program_windows(name: &str) -> OsString {
    if name.is_empty() || is_path_like(name) {
        return OsString::from(name);
    }
    if let Some(hit) = search_path(name) {
        return hit;
    }
    for ext in pathext() {
        if ext.eq_ignore_ascii_case(".exe") {
            continue; // already probed above
        }
        if let Some(hit) = search_path_with_ext(name, &ext) {
            return hit;
        }
    }
    OsString::from(name)
}

/// The OS default `%PATHEXT%` head, used when the variable is unset/empty.
const PATHEXT_DEFAULT: &str = ".COM;.EXE;.BAT;.CMD";

/// Parse a `%PATHEXT%`-shaped list into its `.<ext>` entries; entries without a
/// leading dot (or a bare `.`) are dropped, unset/empty falls back to the OS
/// default. Pure, so it is unit-tested without mutating the process env.
fn parse_pathext(raw: Option<&str>) -> Vec<String> {
    let raw = match raw {
        Some(s) if !s.trim().is_empty() => s,
        _ => PATHEXT_DEFAULT,
    };
    raw.split(';')
        .map(str::trim)
        .filter(|e| e.len() > 1 && e.starts_with('.'))
        .map(str::to_string)
        .collect()
}

/// The live `%PATHEXT%` entries (see [`parse_pathext`]).
fn pathext() -> Vec<String> {
    let raw = std::env::var("PATHEXT").ok();
    parse_pathext(raw.as_deref())
}

/// `SearchPathW(NULL, name, L".exe", …)`: the standard search order (app dir,
/// system dirs, `%PATH%`), appending `.exe` when `name` has no extension.
pub(crate) fn search_path(name: &str) -> Option<OsString> {
    search_path_with_ext(name, ".exe")
}

/// [`search_path`] with an explicit default extension (a `%PATHEXT%` entry).
fn search_path_with_ext(name: &str, ext: &str) -> Option<OsString> {
    let name_w = wide_nul(OsStr::new(name));
    let ext_w = wide_nul(OsStr::new(ext));
    let mut buf: Vec<u16> = vec![0; 260];
    loop {
        // SAFETY: `name_w`/`ext_w` are NUL-terminated wide strings live for the
        // call; `buf` is a writable buffer of the advertised length.
        let n = unsafe {
            ffi::SearchPathW(
                std::ptr::null(),
                name_w.as_ptr(),
                ext_w.as_ptr(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
                buf.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if n == 0 {
            return None; // not found
        }
        let n = n as usize;
        if n < buf.len() {
            return Some(OsString::from_wide(&buf[..n]));
        }
        buf = vec![0; n + 1]; // n = required length (incl. NUL) when too small
    }
}

/// Resolve a shell NAME the user asked for (config `shell` key, `--shell` flag,
/// or `%ATERM_SHELL%`) to a runnable program. Precedence per name:
///   1. path-like (`\`, `/`, or drive colon) → verbatim (the user gave a path);
///   2. a KNOWN ALIAS that is not on PATH → its well-known install location
///      ([`discover_shell`] — this is what makes `shell = "bash"` find Git Bash
///      even though its `bin` is off the user's PATH);
///   3. `SearchPathW` (`.exe` + `%PATHEXT%`) — a bare name on PATH;
///   4. verbatim fallback → `CreateProcessW` fails cleanly, never a silent
///      substitution of a different shell than the user named.
pub(crate) fn resolve_shell_name(name: &OsStr) -> OsString {
    let s = name.to_string_lossy();
    if is_path_like(&s) {
        return name.to_os_string();
    }
    if let Some(hit) = discover_shell(&s) {
        return hit;
    }
    if let Some(hit) = search_path(&s) {
        return hit;
    }
    name.to_os_string()
}

/// Find a KNOWN shell by alias at its standard install location — the discovery
/// that lets a user name a shell that isn't on `%PATH%`. Case-insensitive.
///   * `bash` / `git-bash` / `gitbash` → Git for Windows `bin\bash.exe` (probed
///     beside a PATH-resolved `git.exe`, then the standard Program Files /
///     per-user install dirs);
///   * `wsl` → `wsl.exe` (a `.exe` on the system PATH, but aliased for symmetry
///     so `shell = "wsl"` reads naturally);
///   * `nu` / `nushell` → `nu.exe` on PATH.
///
/// Returns `None` for an unknown alias or a known one that isn't installed, so
/// the caller falls through to `SearchPathW`.
pub(crate) fn discover_shell(name: &str) -> Option<OsString> {
    let lname = name.trim().to_ascii_lowercase();
    match lname.as_str() {
        "bash" | "git-bash" | "gitbash" => discover_git_bash(),
        "wsl" => search_path("wsl"),
        "nu" | "nushell" => search_path("nu"),
        _ => None,
    }
}

/// Locate Git for Windows' `bash.exe`. First beside a PATH-resolved `git.exe`
/// (`<gitroot>\bin\bash.exe`, where `git.exe` sits in `<gitroot>\cmd`), then the
/// standard machine + per-user install roots. The FIRST existing hit wins.
fn discover_git_bash() -> Option<OsString> {
    // Beside git.exe: cmd\git.exe → ..\bin\bash.exe.
    if let Some(git) = search_path("git") {
        let p = Path::new(&git);
        if let Some(root) = p.parent().and_then(Path::parent) {
            let bash = root.join("bin").join("bash.exe");
            if bash.is_file() {
                return Some(bash.into_os_string());
            }
        }
    }
    // Standard install roots (machine + WOW64 + per-user).
    let roots = [
        std::env::var_os("ProgramFiles"),
        std::env::var_os("ProgramFiles(x86)"),
        std::env::var_os("ProgramW6432"),
        std::env::var_os("LocalAppData").map(|l| {
            let mut p = std::path::PathBuf::from(l);
            p.push("Programs");
            p.into_os_string()
        }),
    ];
    for root in roots.into_iter().flatten() {
        let bash = Path::new(&root).join("Git").join("bin").join("bash.exe");
        if bash.is_file() {
            return Some(bash.into_os_string());
        }
    }
    None
}

/// Select the interactive shell. Precedence: the caller's `override_shell`
/// (config `shell` / `--shell`) → `%ATERM_SHELL%` → `pwsh.exe` → `powershell.exe`
/// → `%COMSPEC%` → literal `cmd.exe`. See the module docs for the deliberate
/// `%SHELL%` omission; `override_shell`/`ATERM_SHELL` both go through
/// [`resolve_shell_name`] (path-like verbatim, else alias discovery, else PATH).
pub(crate) fn select_shell(override_shell: Option<&OsStr>) -> OsString {
    if let Some(ov) = override_shell.filter(|o| !o.is_empty()) {
        return resolve_shell_name(ov);
    }
    if let Some(sh) = std::env::var_os("ATERM_SHELL")
        && !sh.is_empty()
    {
        return resolve_shell_name(&sh);
    }
    if let Some(p) = search_path("pwsh") {
        return p;
    }
    if let Some(p) = search_path("powershell") {
        return p;
    }
    if let Some(c) = std::env::var_os("COMSPEC")
        && !c.is_empty()
    {
        return c;
    }
    OsString::from("cmd.exe")
}

/// Resolve the spawn target: `(program, argv)`. Precedence is identical to the
/// Unix seam: `exec_command` (`-e`, runs the command directly — when it exits
/// the session closes) > `argv_override` (the future shell-integration hook;
/// program stays the selected shell) > `%ATERM_EXEC%` (run then stay
/// interactive, by shell family) > bare interactive `[shell]`.
pub(crate) fn resolve_spawn_target(
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[String]>,
) -> (OsString, Vec<OsString>) {
    if let Some(cmd) = exec_command.filter(|c| !c.is_empty()) {
        let program = resolve_program_windows(&cmd[0]);
        let argv = cmd.iter().map(OsString::from).collect();
        return (program, argv);
    }
    let shell = select_shell(shell_override.map(OsStr::new));
    if let Some(ov) = argv_override {
        // `wsl.exe` is the one shell whose `shell_args` are OPTIONS OF THE
        // LAUNCHER rather than of the interactive shell: `-d <distro>`,
        // `-u <user>`, `--system` choose WHICH Linux the tab is. The historical
        // "an argv override REPLACES argv" rule would silently drop them the
        // moment shell integration started supplying an override, quietly
        // moving a `shell = "wsl"` + `shell_args = ["-d", "Debian"]` user from
        // Debian to their default distro. Splice them in ahead of the override's
        // own arguments (wsl.exe takes options before the command), keeping
        // argv[0] — the display token — first.
        //
        // Deliberately WSL-only. bash's override is `bash --rcfile <wrapper>`,
        // and splicing a `shell_args = ["-l"]` in front of it would make bash a
        // LOGIN shell, which reads `.bash_profile` INSTEAD of the `--rcfile` —
        // i.e. it would disable the very integration the override installs.
        let args = shell_args.unwrap_or(&[]);
        if args.is_empty() || !is_wsl_shell(&shell) {
            let argv = ov.iter().map(OsString::from).collect();
            return (shell, argv);
        }
        let mut argv: Vec<OsString> = Vec::with_capacity(ov.len() + args.len());
        argv.extend(ov.first().map(OsString::from));
        argv.extend(args.iter().map(OsString::from));
        argv.extend(ov.iter().skip(1).map(OsString::from));
        return (shell, argv);
    }
    // Configured `shell_args` (e.g. `["-l", "-i"]` for a login bash) — argv[0] is
    // the resolved shell, then the user's args verbatim. Empty is the bare shell.
    if let Some(args) = shell_args.filter(|a| !a.is_empty()) {
        let mut argv = vec![shell.clone()];
        argv.extend(args.iter().map(OsString::from));
        return (shell, argv);
    }
    if let Some(cmd) = std::env::var_os("ATERM_EXEC") {
        let argv = aterm_exec_argv(&shell, &cmd);
        return (shell, argv);
    }
    let argv = vec![shell.clone()];
    (shell, argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_detection_is_stem_and_case_insensitive() {
        for (s, f) in [
            ("pwsh.exe", ShellFamily::Pwsh),
            (
                "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
                ShellFamily::Pwsh,
            ),
            ("PowerShell.EXE", ShellFamily::Pwsh),
            ("powershell", ShellFamily::Pwsh),
            ("cmd.exe", ShellFamily::Cmd),
            ("C:\\Windows\\System32\\CMD.EXE", ShellFamily::Cmd),
            ("nu.exe", ShellFamily::Other),
            ("bash.exe", ShellFamily::Other),
        ] {
            assert_eq!(shell_family(OsStr::new(s)), f, "family of {s}");
        }
    }

    #[test]
    fn aterm_exec_argv_shapes_per_family() {
        let cmd = OsStr::new("dir C:\\");
        let pwsh = aterm_exec_argv(OsStr::new("pwsh.exe"), cmd);
        assert_eq!(
            pwsh,
            ["pwsh.exe", "-NoExit", "-Command", "dir C:\\"]
                .map(OsString::from)
                .to_vec()
        );
        let cmdsh = aterm_exec_argv(OsStr::new("C:\\Windows\\System32\\cmd.exe"), cmd);
        assert_eq!(
            cmdsh,
            ["C:\\Windows\\System32\\cmd.exe", "/K", "dir C:\\"]
                .map(OsString::from)
                .to_vec()
        );
        // Unknown family: ATERM_EXEC ignored, bare shell (documented).
        let other = aterm_exec_argv(OsStr::new("nu.exe"), cmd);
        assert_eq!(other, [OsString::from("nu.exe")].to_vec());
    }

    #[test]
    fn path_like_classifier() {
        assert!(is_path_like("C:\\Windows\\cmd.exe"));
        assert!(is_path_like("bin/tool.exe"));
        assert!(is_path_like("C:cmd")); // drive-relative counts as explicit
        assert!(!is_path_like("cmd"));
        assert!(!is_path_like("pwsh"));
    }

    #[test]
    fn search_path_finds_cmd_and_misses_nonsense() {
        // cmd.exe lives in a system dir SearchPathW always probes, so this is
        // hermetic (no PATH mutation needed — the harness is multi-threaded and
        // env writes are UB-adjacent under edition 2024).
        let hit = search_path("cmd").expect("cmd.exe must resolve on Windows");
        assert!(
            hit.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("cmd.exe"),
            "resolved {hit:?}"
        );
        assert!(search_path("aterm-no-such-prog-xyz").is_none());
    }

    #[test]
    fn resolve_program_passes_paths_verbatim_and_falls_back_verbatim() {
        assert_eq!(
            resolve_program_windows("C:\\nonexistent\\x.exe"),
            OsString::from("C:\\nonexistent\\x.exe"),
            "path-like: verbatim, even when absent (CreateProcessW fails cleanly)"
        );
        assert_eq!(
            resolve_program_windows("aterm-no-such-prog-xyz"),
            OsString::from("aterm-no-such-prog-xyz"),
            "unresolved bare name: verbatim fallback"
        );
        let cmd = resolve_program_windows("cmd");
        assert!(
            cmd.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("cmd.exe"),
            "bare name resolves via SearchPathW: {cmd:?}"
        );
    }

    #[test]
    fn parse_pathext_defaults_and_drops_junk() {
        assert_eq!(
            parse_pathext(None),
            [".COM", ".EXE", ".BAT", ".CMD"].map(String::from).to_vec(),
            "unset PATHEXT falls back to the OS default head"
        );
        assert_eq!(
            parse_pathext(Some("  ")),
            [".COM", ".EXE", ".BAT", ".CMD"].map(String::from).to_vec(),
            "blank PATHEXT falls back too"
        );
        assert_eq!(
            parse_pathext(Some(".COM;.EXE;.BAT;.CMD;.PS1")),
            [".COM", ".EXE", ".BAT", ".CMD", ".PS1"]
                .map(String::from)
                .to_vec()
        );
        assert_eq!(
            parse_pathext(Some(".EXE;;junk;.;  .CMD  ")),
            [".EXE", ".CMD"].map(String::from).to_vec(),
            "dotless/empty entries are dropped, whitespace trimmed"
        );
    }

    #[test]
    fn resolve_program_finds_cmd_and_bat_shims_via_pathext() {
        // Hermetic (no PATH/PATHEXT mutation — multi-threaded harness): probe a
        // stock System32 `.cmd`, a dir SearchPathW always searches. winrm.cmd
        // ships on every Windows; skip gracefully if a trimmed SKU lacks it.
        let sysroot = match std::env::var_os("SystemRoot") {
            Some(r) => r,
            None => return,
        };
        let shim = Path::new(&sysroot).join("System32").join("winrm.cmd");
        if !shim.is_file() {
            return;
        }
        let hit = resolve_program_windows("winrm");
        assert!(
            hit.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("winrm.cmd"),
            "bare .cmd shim must resolve via the PATHEXT retry: {hit:?}"
        );
    }

    #[test]
    fn exec_command_takes_precedence_and_argv_is_verbatim() {
        let cmd = vec!["cmd".to_string(), "/c".to_string(), "echo hi".to_string()];
        let ov = vec!["ignored".to_string()];
        let (program, argv) = resolve_spawn_target(None, None, Some(&ov), Some(&cmd));
        assert!(
            program
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("cmd.exe"),
            "-e program is PATH-resolved: {program:?}"
        );
        assert_eq!(
            argv,
            ["cmd", "/c", "echo hi"].map(OsString::from).to_vec(),
            "argv[0] stays the name as GIVEN (Unix parity)"
        );
    }

    #[test]
    fn shell_override_selects_and_resolves_the_named_shell() {
        // A bare known name resolves via SearchPathW (cmd is always present).
        let (program, argv) = resolve_spawn_target(Some("cmd"), None, None, None);
        assert!(
            program
                .to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("cmd.exe"),
            "override 'cmd' resolves to cmd.exe: {program:?}"
        );
        assert_eq!(argv, vec![program], "bare interactive argv is [shell]");
        // A path-like override is used verbatim (even if absent → fails cleanly).
        let (p2, _) = resolve_spawn_target(Some("C:\\x\\my.exe"), None, None, None);
        assert_eq!(p2, OsString::from("C:\\x\\my.exe"), "path-like verbatim");
    }

    #[test]
    fn shell_args_shape_the_interactive_argv() {
        let args = vec!["-l".to_string(), "-i".to_string()];
        let (program, argv) = resolve_spawn_target(Some("cmd"), Some(&args), None, None);
        assert_eq!(argv.len(), 3, "argv = [shell, -l, -i]");
        assert_eq!(argv[0], program);
        assert_eq!(argv[1], OsString::from("-l"));
        assert_eq!(argv[2], OsString::from("-i"));
    }

    #[test]
    fn discover_shell_unknown_alias_is_none() {
        assert!(
            discover_shell("definitely-not-a-shell").is_none(),
            "unknown alias falls through to SearchPathW"
        );
        // `bash` discovery is environment-dependent (Git for Windows may or may
        // not be installed on CI), so we only assert the None path here; the
        // positive path is covered by the live smoke on the owner's machine.
    }

    #[test]
    fn wsl_shell_args_survive_the_integration_argv_override() {
        // `shell = "wsl"` + `shell_args = ["-d", "Debian"]` chooses WHICH distro
        // the tab is. Once shell integration started supplying an argv override,
        // the historical "override replaces argv" rule silently moved that user
        // to their DEFAULT distro. The options must be spliced in ahead of the
        // override's own arguments (wsl.exe takes options before the command).
        let args = vec!["-d".to_string(), "Debian".to_string()];
        let ov = vec![
            "wsl".to_string(),
            "--exec".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            "exec bash".to_string(),
        ];
        let (program, argv) = resolve_spawn_target(Some("wsl"), Some(&args), Some(&ov), None);
        assert!(
            program
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("wsl"),
            "override 'wsl' still resolves to wsl.exe: {program:?}"
        );
        assert_eq!(
            argv,
            [
                "wsl",
                "-d",
                "Debian",
                "--exec",
                "/bin/sh",
                "-c",
                "exec bash"
            ]
            .map(OsString::from)
            .to_vec()
        );
        // No shell_args → the override is used exactly as given.
        let (_, plain) = resolve_spawn_target(Some("wsl"), None, Some(&ov), None);
        assert_eq!(plain, ov.iter().map(OsString::from).collect::<Vec<_>>());
    }

    #[test]
    fn non_wsl_argv_override_still_replaces_argv_entirely() {
        // bash's override IS the integration: splicing `-l` in front of
        // `--rcfile` would make bash a LOGIN shell, which reads .bash_profile
        // instead of the rcfile — i.e. it would disable the injection. Only
        // wsl.exe gets the splice.
        let args = vec!["-l".to_string()];
        let ov = vec![
            "bash".to_string(),
            "--rcfile".to_string(),
            "C:\\cache\\bash\\rcfile".to_string(),
        ];
        let (_, argv) = resolve_spawn_target(Some("cmd"), Some(&args), Some(&ov), None);
        assert_eq!(
            argv,
            ["bash", "--rcfile", "C:\\cache\\bash\\rcfile"]
                .map(OsString::from)
                .to_vec(),
            "a non-wsl override must be verbatim, shell_args dropped as before"
        );
    }

    #[test]
    fn is_wsl_shell_matches_the_stem_case_insensitively() {
        assert!(is_wsl_shell(OsStr::new("wsl")));
        assert!(is_wsl_shell(OsStr::new("C:\\Windows\\System32\\WSL.EXE")));
        assert!(!is_wsl_shell(OsStr::new("C:\\x\\wslconfig.exe")));
        assert!(!is_wsl_shell(OsStr::new("bash.exe")));
    }

    #[test]
    fn path_like_names_bypass_discovery() {
        // resolve_shell_name must pass an explicit path verbatim without probing.
        assert_eq!(
            resolve_shell_name(OsStr::new("C:\\Program Files\\Git\\bin\\bash.exe")),
            OsString::from("C:\\Program Files\\Git\\bin\\bash.exe")
        );
    }
}
