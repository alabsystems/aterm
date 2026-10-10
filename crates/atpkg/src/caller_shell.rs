// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SHELL THAT TYPED THIS: which shell `aterm pkg doctor` / `which` are
//! answering, so the one remedy they print — `. ~/.aterm/shell.d/00-atpkg.zsh`, `source
//! …fish`, the dot-source for pwsh — is in the dialect of the shell it is typed into.
//!
//! `$SHELL` is the LOGIN shell, not the one in front of the user: aterm spawns the
//! configured `shell` (`--shell`, the config key) without re-pointing `$SHELL`, so a
//! fish tab on a zsh-login machine would be handed a zsh line fish cannot source. The
//! signal a tab actually carries is its
//! PROCESS TREE: `aterm pkg …` is a child of the shell that typed it (`exec`'d shims keep
//! the parent), so the parent's executable name is the shell — `zsh`, `bash`, `fish`,
//! `pwsh` — read once, by pid, with no fork: `sysctl(KERN_PROCARGS2)` on macOS (the exec
//! path leads the buffer), `/proc/<pid>/exe` on Linux. Only when the parent is not a
//! shell at all (`sudo`, `make`, a Python driver) does `$SHELL` answer.

/// The shell families this crate can name a remedy for, or hand a PATH line to:
/// the parent's executable name is admitted only when it is one of these, so a driver
/// that is not a shell never masquerades as one.
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

/// The shell this process was typed into, as the basename [`crate::hooks::hook_for_shell`]
/// keys on: the parent process's executable when it is a known shell, else `$SHELL`'s
/// basename, else `None`.
#[must_use]
pub fn invoking_shell() -> Option<String> {
    let parent = parent_exe_name();
    let login = std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    invoking_shell_from(parent.as_deref(), login.as_deref())
}

/// The pure half of [`invoking_shell`]: `parent` (the parent process's executable name,
/// when readable) wins whenever it is a known shell; otherwise `login` (`$SHELL`'s
/// basename). A leading `-` (a login shell's argv0 spelling, should a reader hand one
/// over) and a `.exe` suffix are stripped before the match.
#[must_use]
pub fn invoking_shell_from(parent: Option<&str>, login: Option<&str>) -> Option<String> {
    parent
        .and_then(known_shell)
        .or_else(|| login.and_then(known_shell))
        .or_else(|| login.map(str::to_string))
}

/// `name` normalised to a family in [`SHELLS`], or `None`.
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

/// The basename of `pid`'s executable, when this process may read it.
///
/// macOS: `sysctl` `KERN_PROCARGS2` — the buffer leads with `argc` (one `c_int`) and
/// then the executable's path, NUL-terminated, before the arguments; the kernel answers
/// for a process of the caller's uid (or any, as root). Linux: `/proc/<pid>/exe`, and
/// only where that link cannot be read, `/proc/<pid>/comm`. Elsewhere, and on any
/// refusal, `None`.
///
/// Linux prefers the exe link because `comm` is a 15-byte kernel label a process can
/// rewrite: `prctl(PR_SET_NAME, "bash")` on a non-shell is exactly the masquerade
/// [`SHELLS`] refuses. The link is annotated " (deleted)" once the file is replaced under
/// a running process — the routine state of a shell across a package upgrade — so that
/// suffix comes off the basename. `comm` remains the fallback only because it is
/// world-readable where the link is not (`/proc/1/exe` is `EACCES` for a non-root reader).
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
/// annotation removed. `None` when the link cannot be read (no such pid, or a process
/// this one may not `ptrace`-read).
#[cfg(target_os = "linux")]
fn linux_exe_link_name(pid: u32) -> Option<String> {
    let link = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let name = link.file_name()?.to_string_lossy();
    let name = name.strip_suffix(" (deleted)").unwrap_or(&name);
    (!name.is_empty()).then(|| name.to_string())
}

/// `/proc/<pid>/comm`: the kernel's label for the thread group — the exec'd file's
/// basename truncated to 15 bytes, unless the process has renamed itself. World-readable
/// where the exe link is not, which is the only reason it is still read.
#[cfg(target_os = "linux")]
fn linux_comm_name(pid: u32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let comm = comm.trim();
    (!comm.is_empty()).then(|| comm.to_string())
}

/// `sysctl` `kern.procargs2` for `pid`: the executable's path is the first NUL-terminated
/// string after the leading `argc`.
#[cfg(target_os = "macos")]
fn macos_exe_name(pid: u32) -> Option<String> {
    exe_name_of_procargs2(&macos_procargs2(pid)?)
}

/// The basename of the exec path a `KERN_PROCARGS2` buffer leads with, read on its own
/// ([`procargs2_exec_path`]) — never through the whole-layout [`parse_procargs2`]. The name
/// needs one string, and an argv the parser refuses must not cost the caller's shell (the
/// remedy line would fall back to `$SHELL`'s dialect).
#[cfg(any(target_os = "macos", test))]
fn exe_name_of_procargs2(buf: &[u8]) -> Option<String> {
    std::path::Path::new(procargs2_exec_path(buf)?)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// The bytes of the native-endian `c_int` argc a `KERN_PROCARGS2` buffer leads with.
#[cfg(any(target_os = "macos", test))]
const PROCARGS2_ARGC_BYTES: usize = 4;

/// The exec path a `KERN_PROCARGS2` buffer leads with: the first NUL-terminated string
/// after the argc, which this does not read. `None` when there is no NUL or the path is
/// not UTF-8.
#[cfg(any(target_os = "macos", test))]
fn procargs2_exec_path(buf: &[u8]) -> Option<&str> {
    let rest = buf.get(PROCARGS2_ARGC_BYTES..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&rest[..end]).ok()
}

/// One process's exec path, argument vector and environment, as the kernel recorded
/// them at `exec` (the environment is the INITIAL one: an `export` the process made
/// later is not in it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcArgs {
    /// The path the process was exec'd from, as passed to `exec` (not canonical).
    pub exec_path: String,
    /// `argv`, `argv[0]` first.
    pub argv: Vec<String>,
    /// `KEY=value` entries, in the order the kernel holds them.
    pub env: Vec<String>,
}

impl ProcArgs {
    /// The value of one environment variable at exec, where set.
    #[must_use]
    pub fn env_var(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .find_map(|kv| kv.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
    }
}

/// `pid`'s exec path, argv and initial environment, when this process may read them —
/// the process's own uid (or root). macOS: one `sysctl(KERN_PROCARGS2)`; Linux:
/// `/proc/<pid>/{exe,cmdline,environ}`. `None` on any refusal, on another platform, or on
/// a buffer this parser does not recognise — never a partial read passed off as whole.
///
/// Used by the agent live-upgrade driver (aterm-agent `harness::upgrade`), which must
/// relaunch a Claude Code session with the flags it was started with and find the aterm
/// tab it runs in; `ps -o command=` joins argv with spaces and loses a quoted word.
#[must_use]
pub fn process_args(pid: u32) -> Option<ProcArgs> {
    #[cfg(target_os = "macos")]
    {
        parse_procargs2(&macos_procargs2(pid)?)
    }
    #[cfg(target_os = "linux")]
    {
        let exec_path = std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()?
            .to_string_lossy()
            .into_owned();
        let argv = split_nul_list(&std::fs::read(format!("/proc/{pid}/cmdline")).ok()?);
        // An empty `environ` entry names no variable; only there is dropping one right.
        let mut env = split_nul_list(&std::fs::read(format!("/proc/{pid}/environ")).ok()?);
        env.retain(|kv| !kv.is_empty());
        Some(ProcArgs {
            exec_path,
            argv,
            env,
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// `/proc/<pid>/cmdline` (or `environ`) as words. The kernel ends EVERY word with a NUL,
/// so exactly one trailing NUL is a terminator and every other empty piece is an EMPTY
/// WORD — kept, as [`parse_procargs2`] keeps one on macOS. Dropping empties handed a flag
/// whose value was `""` the next word as its value: `--append-system-prompt ""
/// --dangerously-skip-permissions` would read back as the prompt swallowing the flag. No
/// trailing NUL (a process
/// that rewrote its title) splits as it stands; no bytes at all are no words.
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

/// Parse a `KERN_PROCARGS2` buffer: a native-endian `c_int` argc, the exec path and its
/// NUL, NUL padding, then `argc` NUL-terminated argv strings, then the environment's
/// NUL-terminated `KEY=value` strings until an empty string or the end. Pure, so the
/// layout is tested on built buffers as well as on a live read.
#[must_use]
#[cfg(any(target_os = "macos", test))]
pub fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs> {
    let argc = i32::from_ne_bytes(buf.get(..PROCARGS2_ARGC_BYTES)?.try_into().ok()?);
    let exec_path = procargs2_exec_path(buf)?.to_string();
    let rest = buf.get(PROCARGS2_ARGC_BYTES..)?;
    // The bound is the one the layout sets: every argv word takes at least one byte of
    // `rest` (its NUL, or its last byte at the very end), so a count past that is a buffer
    // this parser does not recognise. A fixed cap is not such a bound — ARG_MAX admits
    // more than 65 535 short words (a live `/bin/zsh` carrying 70 004 argv was measured),
    // and refusing one refused a real process whole.
    let argc = usize::try_from(argc).ok().filter(|&n| n <= rest.len())?;
    let mut at = exec_path.len();
    while rest.get(at) == Some(&0) {
        at += 1;
    }
    let mut next = || -> Option<String> {
        let tail = rest.get(at..)?;
        if tail.is_empty() {
            return None;
        }
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        let s = String::from_utf8_lossy(&tail[..end]).into_owned();
        at += end + 1;
        Some(s)
    };
    // Grown, not reserved up front: `argc` is bounded by the buffer, not by what an ordinary
    // process carries, and a reservation sized by it would be the largest allocation here.
    let mut argv = Vec::with_capacity(argc.min(1024));
    for _ in 0..argc {
        argv.push(next()?);
    }
    let mut env = Vec::new();
    while let Some(kv) = next() {
        if kv.is_empty() {
            break;
        }
        env.push(kv);
    }
    Some(ProcArgs {
        exec_path,
        argv,
        env,
    })
}

/// The raw `KERN_PROCARGS2` buffer for `pid`, sized by `KERN_ARGMAX`.
#[cfg(target_os = "macos")]
fn macos_procargs2(pid: u32) -> Option<Vec<u8>> {
    /// `KERN_PROCARGS2` (`sys/sysctl.h`): the process's `argc`, exec path, argv and env.
    const KERN_PROCARGS2: libc::c_int = 49;
    /// `KERN_ARGMAX`: the size that buffer must have.
    const KERN_ARGMAX: libc::c_int = 8;
    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut argmax: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::size_t;
    let mut mib = [libc::CTL_KERN, KERN_ARGMAX];
    // SAFETY: `mib` names a two-integer key; `argmax`/`len` are ours and outlive the
    // call; no new value is written (`newp` null, `newlen` 0).
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut argmax).cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    let argmax = usize::try_from(argmax).ok().filter(|&n| rc == 0 && n > 4)?;
    let mut buf = vec![0u8; argmax];
    let mut len = argmax as libc::size_t;
    let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, pid];
    // SAFETY: `buf` is `argmax` bytes and `len` says so; the kernel writes at most `len`
    // and updates it; no new value is written.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len.min(argmax));
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parent's shell wins over `$SHELL`; a non-shell parent falls back to it; a
    /// login-shell `-` and a `.exe` are stripped; an unknown `$SHELL` still passes
    /// through (the remedy's PATH-line arm names it).
    #[test]
    fn the_parent_shell_wins_and_the_login_shell_is_the_fallback() {
        assert_eq!(
            invoking_shell_from(Some("fish"), Some("zsh")).as_deref(),
            Some("fish")
        );
        assert_eq!(
            invoking_shell_from(Some("bash"), Some("zsh")).as_deref(),
            Some("bash")
        );
        assert_eq!(
            invoking_shell_from(Some("sudo"), Some("zsh")).as_deref(),
            Some("zsh")
        );
        assert_eq!(invoking_shell_from(Some("python3"), None).as_deref(), None);
        assert_eq!(
            invoking_shell_from(None, Some("fish")).as_deref(),
            Some("fish")
        );
        assert_eq!(
            invoking_shell_from(Some("-zsh"), Some("bash")).as_deref(),
            Some("zsh")
        );
        assert_eq!(
            invoking_shell_from(Some("pwsh.exe"), None).as_deref(),
            Some("pwsh")
        );
        assert_eq!(
            invoking_shell_from(Some("make"), Some("nu")).as_deref(),
            Some("nu")
        );
        assert_eq!(
            invoking_shell_from(Some("make"), Some("weird")).as_deref(),
            Some("weird")
        );
        assert_eq!(invoking_shell_from(None, None), None);
    }

    /// Poll until `pid` answers `want`, bounded. `Command::spawn` does not promise the
    /// child has exec'd — macOS takes the `posix_spawn` path, which returns as soon as the
    /// kernel has the process — so measuring straight after a spawn races the exec. A
    /// timeout here means the child never got there, not that the law under test failed.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn await_exec(pid: u32, want: &str) {
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        loop {
            if process_exe_name(pid).as_deref() == Some(want) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pid {pid} never came up as {want:?} within a minute — the spawn or the exec \
                 failed, which is not what this case is about"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The pid reader names THIS test binary and a spawned `sleep` by executable, and
    /// answers `None` for a pid nobody has.
    ///
    /// Our own name is the fixture that catches Linux's `comm`: cargo's `-<16 hex>` suffix
    /// puts a test binary past the 15-byte `TASK_COMM_LEN` cap. The length is asserted so a
    /// shorter binary name cannot quietly retire the case.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_pid_reader_names_a_process_by_its_executable() {
        let me = process_exe_name(std::process::id()).expect("our own name");
        let exe = std::env::current_exe().unwrap();
        let own = exe.file_name().unwrap().to_string_lossy();
        assert!(
            own.len() > 15,
            "the truncation this asserts away needs a name past 15 bytes, not {own:?}"
        );
        assert_eq!(me, own, "{me}");
        // `spawnfd`'s, as every stand-in here: it holds its stdio and nothing
        // else of this process, not another test thread's pipe while it lives.
        let mut child = aterm_uds::spawnfd::command("/bin/sleep")
            .arg("30")
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("/bin/sleep");
        await_exec(child.id(), "sleep");
        let name = process_exe_name(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name.as_deref(), Some("sleep"));
        assert_eq!(process_exe_name(u32::MAX), None);
    }

    /// End to end through a REAL shell: `sh -c` / `zsh -c` / `bash -c` / `fish -c` each
    /// spawn this test binary in a mode that prints what [`invoking_shell`] answers, with
    /// `$SHELL` pointed elsewhere — the parent shell is what comes back.
    #[cfg(unix)]
    #[test]
    fn a_real_parent_shell_is_named_over_the_login_shell() {
        if std::env::var_os(PROBE_ENV).is_some() {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let mut seen = 0;
        for (shell, expect) in [
            // macOS's `/bin/sh` may re-exec the shell `/var/select/sh` names.
            ("/bin/sh", "sh|bash|dash|zsh"),
            ("/bin/zsh", "zsh"),
            ("/bin/bash", "bash"),
            ("/opt/homebrew/bin/fish", "fish"),
            ("/usr/bin/fish", "fish"),
            ("/usr/local/bin/fish", "fish"),
        ] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            // `exec` keeps the shell as the parent only when the shell does NOT exec the
            // command itself (`sh -c 'cmd'` may); a `;` after the command stops that.
            let script = format!(
                "'{}' --exact --nocapture --quiet {}; true",
                exe.display(),
                "caller_shell::tests::probe_prints_the_invoking_shell_when_asked"
            );
            let out = std::process::Command::new(shell)
                .arg("-c")
                .arg(&script)
                .env(PROBE_ENV, "1")
                .env("SHELL", "/usr/bin/nowhere/elvish")
                .output()
                .expect("spawn the shell");
            let stdout = String::from_utf8_lossy(&out.stdout);
            let line = stdout
                .lines()
                .find_map(|l| l.strip_prefix("invoking_shell="))
                .unwrap_or_else(|| panic!("{shell}: no probe line in {stdout:?}"));
            assert!(
                expect.split('|').any(|e| e == line),
                "{shell}: expected one of {expect}, got {line}: {stdout}"
            );
            seen += 1;
        }
        assert!(
            seen >= 2,
            "at least sh and one of zsh/bash exist on a test host"
        );
    }

    /// The probe half of the test above: printed by the SAME test function when run as
    /// the child, so the harness needs no extra binary.
    #[cfg(unix)]
    const PROBE_ENV: &str = "ATPKG_CALLER_SHELL_PROBE";

    #[cfg(unix)]
    #[test]
    fn probe_prints_the_invoking_shell_when_asked() {
        if std::env::var_os(PROBE_ENV).is_none() {
            return;
        }
        println!(
            "invoking_shell={}",
            invoking_shell().unwrap_or_else(|| "-".into())
        );
    }

    /// The KERN_PROCARGS2 layout, built by hand: argc, the exec path, NUL padding, argv,
    /// then the environment up to the first empty string. A quoted word keeps its space
    /// (the reason `ps -o command=` is not used), and a short buffer is refused whole.
    #[test]
    fn procargs2_parses_argv_and_env_and_refuses_a_short_buffer() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/x/claude\0\0\0\0claude\0--add-dir\0/a b\0HOME=/h\0ATERM_PARENT_SESSION_ID=s-1\0\0junk");
        let got = parse_procargs2(&buf).expect("a whole buffer");
        assert_eq!(got.exec_path, "/x/claude");
        assert_eq!(got.argv, vec!["claude", "--add-dir", "/a b"]);
        assert_eq!(got.env, vec!["HOME=/h", "ATERM_PARENT_SESSION_ID=s-1"]);
        assert_eq!(got.env_var("ATERM_PARENT_SESSION_ID"), Some("s-1"));
        assert_eq!(got.env_var("ATERM_PARENT"), None, "a prefix is not a key");
        let mut short = 5i32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"/x\0a\0b\0");
        assert_eq!(parse_procargs2(&short), None, "fewer argv than argc");
        assert_eq!(parse_procargs2(&[1, 0]), None);
    }

    /// An argc past 65 535 is a real process, not a malformed buffer: ARG_MAX admits more
    /// short words than that (a live `/bin/zsh` carrying 70 004 argv read back whole). Such a
    /// buffer parses whole and names its executable; a count no buffer of its size can hold is
    /// refused without reserving for it.
    #[test]
    fn procargs2_admits_an_argc_past_65535_and_bounds_it_by_the_buffer() {
        let n = 70_000_usize;
        let mut buf = i32::try_from(n).unwrap().to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/zsh\0\0\0\0");
        for _ in 0..n {
            buf.extend_from_slice(b"x\0");
        }
        buf.extend_from_slice(b"K=v\0\0");
        let got = parse_procargs2(&buf).expect("70 000 argv is a whole buffer");
        assert_eq!(got.exec_path, "/bin/zsh");
        assert_eq!(got.argv.len(), n);
        assert_eq!(got.env_var("K"), Some("v"));
        assert_eq!(exe_name_of_procargs2(&buf).as_deref(), Some("zsh"));
        let mut lie = i32::MAX.to_ne_bytes().to_vec();
        lie.extend_from_slice(b"/x\0\0a\0");
        assert_eq!(
            parse_procargs2(&lie),
            None,
            "more argv than the buffer has bytes"
        );
    }

    /// The exe name needs the exec path and nothing after it: a buffer the whole-layout
    /// parser refuses still names its executable, as this reader did before the parser
    /// existed. Only a buffer with no exec path names nothing.
    #[test]
    fn the_exe_name_reads_the_exec_path_alone() {
        let mut short = 5i32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"/opt/homebrew/bin/fish\0\0a\0b\0");
        assert_eq!(parse_procargs2(&short), None, "fewer argv than argc");
        assert_eq!(exe_name_of_procargs2(&short).as_deref(), Some("fish"));
        let mut negative = (-1i32).to_ne_bytes().to_vec();
        negative.extend_from_slice(b"/bin/bash\0");
        assert_eq!(exe_name_of_procargs2(&negative).as_deref(), Some("bash"));
        assert_eq!(
            exe_name_of_procargs2(&[1, 0, 0, 0]),
            None,
            "nothing after argc"
        );
        assert_eq!(
            exe_name_of_procargs2(&[1, 0, 0, 0, b'/', b'x']),
            None,
            "no NUL"
        );
    }

    /// Linux's `/proc/<pid>/cmdline` keeps an empty argument, as the macOS parser does: only
    /// the one trailing NUL is a terminator. Pure, so it runs on every platform.
    #[test]
    fn a_nul_list_keeps_an_empty_word_and_drops_only_the_terminator() {
        let cmdline = b"claude\0--append-system-prompt\0\0--dangerously-skip-permissions\0";
        assert_eq!(
            split_nul_list(cmdline),
            vec![
                "claude",
                "--append-system-prompt",
                "",
                "--dangerously-skip-permissions"
            ]
        );
        assert_eq!(
            split_nul_list(b"claude\0--model\0\0"),
            vec!["claude", "--model", ""],
            "an empty LAST word, then the terminator"
        );
        assert_eq!(
            split_nul_list(b"a\0b"),
            vec!["a", "b"],
            "a rewritten title, no final NUL"
        );
        assert_eq!(split_nul_list(b"\0"), vec![""], "one word, and it is empty");
        assert_eq!(split_nul_list(b""), Vec::<String>::new(), "no words at all");
        // The macOS reader reads the same argv the same way.
        let mut buf = 4i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/x/claude\0\0");
        buf.extend_from_slice(cmdline);
        buf.extend_from_slice(b"K=v\0\0");
        assert_eq!(
            parse_procargs2(&buf).expect("whole").argv,
            split_nul_list(cmdline)
        );
    }

    /// A live process started with an EMPTY argument reads back with it, in place — on
    /// Linux through the `/proc` arm, on macOS through
    /// `KERN_PROCARGS2`. `read` is a shell builtin, so the shell itself is what waits, and
    /// closing its stdin ends it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_args_keeps_an_empty_argument_of_a_live_process() {
        let tail = [
            "--append-system-prompt",
            "",
            "--dangerously-skip-permissions",
        ];
        let mut child = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", "read -r _", "sh"])
            .args(tail)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("/bin/sh");
        // The last word is the sign the exec happened (see `await_exec`); what is asserted
        // is the word before it.
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let argv = loop {
            let argv = process_args(child.id()).map(|a| a.argv);
            if argv
                .as_ref()
                .and_then(|a| a.last())
                .is_some_and(|w| w == tail[2])
                || std::time::Instant::now() >= deadline
            {
                break argv;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        drop(child.stdin.take());
        let _ = child.kill();
        let _ = child.wait();
        let argv = argv.unwrap_or_default();
        assert!(
            argv.last().is_some_and(|w| w == tail[2]),
            "the shell never read back as exec'd within a minute ({argv:?}) — the spawn failed, \
             which is not what this case is about"
        );
        assert!(
            argv.ends_with(&tail.map(String::from)),
            "the empty argument must read back in place: {argv:?}"
        );
    }

    /// A live read of this test process: its own argv[0] and a variable it certainly has.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn process_args_reads_this_process() {
        let me = process_args(std::process::id()).expect("our own pid is readable");
        assert!(!me.argv.is_empty());
        assert!(me.exec_path.contains("atpkg"), "{}", me.exec_path);
        assert!(me.env.iter().any(|kv| kv.contains('=')));
    }
}
