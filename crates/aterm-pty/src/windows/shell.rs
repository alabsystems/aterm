// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shell selection and `argv` mapping for Windows process spawning.

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::Path;

use super::cmdline::wide_nul;
use super::ffi;

/// Whether the resolved shell is `wsl.exe` (by file stem, case-insensitive).
pub(crate) fn is_wsl_shell(shell: &OsStr) -> bool {
    Path::new(shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("wsl"))
}

/// Whether `name` is used verbatim (an explicit absolute/relative path) rather than PATH-searched.
pub(crate) fn is_path_like(name: &str) -> bool {
    name.contains('\\') || name.contains('/') || name.contains(':')
}

/// PATH-resolve a `-e` program name, the Windows twin of the Unix `resolve_program`.
pub(crate) fn resolve_program_windows(name: &str) -> OsString {
    if name.is_empty() || is_path_like(name) {
        return OsString::from(name);
    }
    if let Some(hit) = search_path(name) {
        return hit;
    }
    for ext in pathext() {
        if ext.eq_ignore_ascii_case(".exe") {
            continue;
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
/// default.
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

/// `SearchPathW(NULL, name, L".exe", …)`.
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
            return None;
        }
        let n = n as usize;
        if n < buf.len() {
            return Some(OsString::from_wide(&buf[..n]));
        }
        buf = vec![0; n + 1];
    }
}

/// What the spawn's own name resolution made of a requested shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellResolution {
    /// A bare name the spawn's own resolution found (alias discovery or `SearchPathW`).
    Resolved(OsString),
    /// A bare name that resolved to nothing.
    Unresolved,
    /// Path-like (`\`, `/`, or a drive colon).
    Verbatim(OsString),
}

/// Classify a shell name exactly as the spawn will (see [`ShellResolution`]).
pub fn classify_shell_name(name: &OsStr) -> ShellResolution {
    let s = name.to_string_lossy();
    if is_path_like(&s) {
        return ShellResolution::Verbatim(name.to_os_string());
    }
    if let Some(hit) = discover_shell(&s) {
        return ShellResolution::Resolved(hit);
    }
    if let Some(hit) = search_path(&s) {
        return ShellResolution::Resolved(hit);
    }
    ShellResolution::Unresolved
}

/// Resolve a shell name the user asked for (config `shell` key or `--shell` flag) to a runnable.
pub(crate) fn resolve_shell_name(name: &OsStr) -> OsString {
    match classify_shell_name(name) {
        ShellResolution::Resolved(hit) | ShellResolution::Verbatim(hit) => hit,
        ShellResolution::Unresolved => name.to_os_string(),
    }
}

/// Find a known shell by alias at its standard install location.
pub(crate) fn discover_shell(name: &str) -> Option<OsString> {
    let lname = name.trim().to_ascii_lowercase();
    match lname.as_str() {
        "bash" | "git-bash" | "gitbash" => discover_git_bash(),
        "wsl" => search_path("wsl"),
        "nu" | "nushell" => search_path("nu"),
        _ => None,
    }
}

/// Locate Git for Windows' `bash.exe`.
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

/// Which arm of [`select_shell_with_origin`] chose the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOrigin {
    /// The caller's `override_shell`.
    Override,
    /// `pwsh.exe` on PATH.
    Pwsh,
    /// `powershell.exe` on PATH.
    PowerShell,
    /// `%COMSPEC%` — neither PowerShell on PATH.
    Comspec,
    /// The literal `cmd.exe`: nothing else resolved and comspec is unset/empty.
    CmdLiteral,
}

impl ShellOrigin {
    /// The one word a report prints for this arm (`shell_origin=default:<word>`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Pwsh => "pwsh",
            Self::PowerShell => "powershell",
            Self::Comspec => "COMSPEC",
            Self::CmdLiteral => "cmd.exe",
        }
    }
}

/// Select the interactive shell.
pub(crate) fn select_shell(override_shell: Option<&OsStr>) -> OsString {
    select_shell_with_origin(override_shell).0
}

/// [`select_shell`] with the arm that decided attached.
pub fn select_shell_with_origin(override_shell: Option<&OsStr>) -> (OsString, ShellOrigin) {
    if let Some(ov) = override_shell.filter(|o| !o.is_empty()) {
        return (resolve_shell_name(ov), ShellOrigin::Override);
    }
    if let Some(p) = search_path("pwsh") {
        return (p, ShellOrigin::Pwsh);
    }
    if let Some(p) = search_path("powershell") {
        return (p, ShellOrigin::PowerShell);
    }
    if let Some(c) = std::env::var_os("COMSPEC")
        && !c.is_empty()
    {
        return (c, ShellOrigin::Comspec);
    }
    (OsString::from("cmd.exe"), ShellOrigin::CmdLiteral)
}

/// Resolve the spawn target.
pub(crate) fn resolve_spawn_target(
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[OsString]>,
) -> (OsString, Vec<OsString>) {
    if let Some(cmd) = exec_command.filter(|c| !c.is_empty()) {
        let program = cmd[0]
            .to_str()
            .map_or_else(|| cmd[0].clone(), resolve_program_windows);
        return (program, cmd.to_vec());
    }
    let shell = select_shell(shell_override.map(OsStr::new));
    if let Some(ov) = argv_override {
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
    if let Some(args) = shell_args.filter(|a| !a.is_empty()) {
        let mut argv = vec![shell.clone()];
        argv.extend(args.iter().map(OsString::from));
        return (shell, argv);
    }
    let argv = vec![shell.clone()];
    (shell, argv)
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
