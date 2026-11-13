// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Caller shell detection and cross-platform process argv/environment inspection.

const SHELLS: &[&str] = &[
    "zsh",
    "bash",
    "fish",
    "pwsh",
    "powershell",
    "sh",
    "dash",
    "ksh",
    "mksh",
    "tcsh",
    "csh",
    "nu",
    "xonsh",
    "elvish",
];

/// Detect the shell this process was invoked from (parent shell or `$SHELL` fallback).
#[must_use]
pub fn invoking_shell() -> Option<String> {
    let parent = parent_exe_name();
    let login = std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    invoking_shell_from(parent.as_deref(), login.as_deref())
}

/// Resolve the invoking shell from `parent` executable name and `login` (`$SHELL`).
#[must_use]
pub fn invoking_shell_from(parent: Option<&str>, login: Option<&str>) -> Option<String> {
    parent
        .and_then(known_shell)
        .or_else(|| login.and_then(known_shell))
        .or_else(|| login.map(str::to_string))
}

fn known_shell(name: &str) -> Option<String> {
    let name = name.trim().trim_start_matches('-');
    let name = name.strip_suffix(".exe").unwrap_or(name);
    SHELLS
        .iter()
        .find(|s| s.eq_ignore_ascii_case(name))
        .map(|s| (*s).to_string())
}

/// The parent process's executable name, when the platform can say.
#[must_use]
fn parent_exe_name() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: `getppid(2)` takes nothing and touches no memory of ours.
        let ppid = unsafe { libc::getppid() };
        let ppid = u32::try_from(ppid).ok()?;
        process_exe_name(ppid)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Return the basename of `pid`'s executable when readable.
#[must_use]
#[cfg(unix)]
pub(crate) fn process_exe_name(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        macos_exe_name(pid)
    }
    #[cfg(target_os = "linux")]
    {
        linux_exe_link_name(pid).or_else(|| linux_comm_name(pid))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// The basename of the file `/proc/<pid>/exe` points at, with procfs's " (deleted)"
/// annotation removed.
#[cfg(target_os = "linux")]
fn linux_exe_link_name(pid: u32) -> Option<String> {
    let link = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let name = link.file_name()?.to_string_lossy();
    let name = name.strip_suffix(" (deleted)").unwrap_or(&name);
    (!name.is_empty()).then(|| name.to_string())
}

/// `/proc/<pid>/comm`: the kernel's label for the thread group, the exec'd file's basename
/// truncated to 15 bytes unless the process renamed itself.
#[cfg(target_os = "linux")]
fn linux_comm_name(pid: u32) -> Option<String> {
    let comm =
        aterm_uds::process::read_proc_text(std::path::Path::new(&format!("/proc/{pid}/comm")))
            .ok()?;
    let comm = comm.trim();
    (!comm.is_empty()).then(|| comm.to_string())
}

/// `sysctl` `kern.procargs2` for `pid` ([`aterm_uds::procargs`]): the basename of the
/// executable's path.
#[cfg(target_os = "macos")]
fn macos_exe_name(pid: u32) -> Option<String> {
    exe_name_of_procargs2(&aterm_uds::procargs::read(pid)?)
}

/// The basename of the exec path a `KERN_PROCARGS2` buffer leads with, read whether or not
/// the buffer holds every argument word: an argv [`parse_procargs2`] refuses must not cost
/// the caller its shell.
#[cfg(any(target_os = "macos", test))]
fn exe_name_of_procargs2(buf: &[u8]) -> Option<String> {
    let exec_path = aterm_uds::procargs::parse(buf)?.exec_path;
    std::path::Path::new(std::str::from_utf8(exec_path).ok()?)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Process executable path, argument vector, and environment read from the OS.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcArgs {
    /// Executable path of the process.
    pub exec_path: String,
    /// Argument vector (`argv[0]` first).
    pub argv: Vec<String>,
    /// Environment `KEY=value` entries.
    pub env: Vec<String>,
}

impl ProcArgs {
    /// Look up environment variable `key` in [`Self::env`].
    #[must_use]
    pub fn env_var(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .find_map(|kv| env_value(kv, key, name_fold()))
    }
}

fn name_fold() -> fn(char) -> char {
    #[cfg(windows)]
    {
        aterm_sysprobe::paths::fold_case
    }
    #[cfg(not(windows))]
    {
        std::convert::identity
    }
}

fn env_value<'a>(kv: &'a str, key: &str, fold: fn(char) -> char) -> Option<&'a str> {
    let mut chars = kv.char_indices();
    for want in key.chars() {
        let (_, got) = chars.next()?;
        if fold(got) != fold(want) {
            return None;
        }
    }
    let (at, _) = chars.next().filter(|&(_, c)| c == '=')?;
    kv.get(at + 1..)
}

/// Read `pid`'s executable path, `argv`, and environment when permitted.
#[must_use]
pub fn process_args(pid: u32) -> Option<ProcArgs> {
    #[cfg(target_os = "macos")]
    {
        parse_procargs2(&aterm_uds::procargs::read(pid)?)
    }
    #[cfg(target_os = "linux")]
    {
        let exec_path = aterm_uds::process::read_exec_path(pid)?
            .to_string_lossy()
            .into_owned();
        let argv = split_nul_list(&aterm_uds::process::read_cmdline(pid)?);
        let mut env = split_nul_list(&aterm_uds::process::read_environ(pid)?);
        env.retain(|kv| !kv.is_empty());
        Some(ProcArgs {
            exec_path,
            argv,
            env,
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let read = aterm_sysprobe::peb::parameters(pid)?;
        let image: Vec<u16> = read.image.as_os_str().encode_wide().collect();
        Some(ProcArgs {
            exec_path: String::from_utf16_lossy(&image),
            argv: split_command_line(&read.command_line, &image),
            env: split_environment(&read.environment)?,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = pid;
        None
    }
}

/// A Windows command line split into `argv` as the Microsoft C runtime splits it — the
/// UCRT's `parse_command_line`, its post-2008 rules, which Rust's std and Zig's (Bun's: the
/// native `claude.exe`) follow too.
#[cfg(any(windows, test))]
fn split_command_line(line: &[u16], image: &[u16]) -> Vec<String> {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let blank = |u: u16| u == u16::from(b' ') || u == u16::from(b'\t');
    let until_nul = |s: &[u16]| s.split(|&u| u == 0).next().unwrap_or_default().to_vec();
    let mut line = until_nul(line);
    if line.is_empty() {
        line = until_nul(image);
    }
    let mut at = 0;
    let mut argv = Vec::new();

    let mut word = Vec::new();
    let mut quoted = false;
    while let Some(&u) = line.get(at) {
        at += 1;
        if u == QUOTE {
            quoted = !quoted;
        } else if !quoted && blank(u) {
            break;
        } else {
            word.push(u);
        }
    }
    argv.push(String::from_utf16_lossy(&word));

    loop {
        while line.get(at).copied().is_some_and(blank) {
            at += 1;
        }
        if at >= line.len() {
            break;
        }
        let mut word = Vec::new();
        let mut quoted = false;
        loop {
            let mut slashes = 0;
            while line.get(at) == Some(&BACKSLASH) {
                slashes += 1;
                at += 1;
            }
            let mut kept = true;
            if line.get(at) == Some(&QUOTE) {
                // `& 1` is the odd test, side-stepping the toolchain's `% 2` lint.
                if slashes & 1 == 0 {
                    if quoted && line.get(at + 1) == Some(&QUOTE) {
                        at += 1;
                    } else {
                        kept = false;
                        quoted = !quoted;
                    }
                }
                slashes /= 2;
            }
            word.extend(std::iter::repeat_n(BACKSLASH, slashes));
            match line.get(at) {
                None => break,
                Some(&u) if !quoted && blank(u) => break,
                Some(&u) => {
                    if kept {
                        word.push(u);
                    }
                    at += 1;
                }
            }
        }
        argv.push(String::from_utf16_lossy(&word));
    }
    argv
}

/// A Windows environment block (UTF-16) as its `KEY=value` strings: each ends in a NUL, and
/// an empty one ends the block (a block whose first unit is a NUL holds none).
#[cfg(any(windows, test))]
fn split_environment(block: &[u16]) -> Option<Vec<String>> {
    let mut env = Vec::new();
    let mut rest = block;
    loop {
        let end = rest.iter().position(|&u| u == 0)?;
        if end == 0 {
            return Some(env);
        }
        env.push(String::from_utf16_lossy(&rest[..end]));
        rest = &rest[end + 1..];
    }
}

/// `/proc/<pid>/cmdline` (or `environ`) as words. The kernel ends every word with a NUL,
/// so exactly one trailing NUL is a terminator and every other empty piece is an empty word,
/// kept (as [`parse_procargs2`] keeps one): dropping it would give a flag whose value was
/// `""` the next word as its value.
#[cfg(any(test, target_os = "linux"))]
fn split_nul_list(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let body = bytes.strip_suffix(b"\0").unwrap_or(bytes);
    body.split(|&b| b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Parse a macOS `KERN_PROCARGS2` buffer into [`ProcArgs`].
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs> {
    let p = aterm_uds::procargs::parse(buf).filter(aterm_uds::procargs::ProcArgs2::whole)?;
    let text = |w: &[u8]| String::from_utf8_lossy(w).into_owned();
    Some(ProcArgs {
        exec_path: std::str::from_utf8(p.exec_path).ok()?.to_string(),
        argv: p.argv.iter().map(|w| text(w)).collect(),
        env: p.env().map(text).collect(),
    })
}

#[cfg(test)]
#[path = "caller_shell_tests.rs"]
mod tests;
