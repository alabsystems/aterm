// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE SHELL THAT TYPED THIS (2026-09-16): which shell `aterm pkg doctor` / `which` are
//! answering, so the one remedy they print — `. ~/.aterm/shell.d/00-atpkg.zsh`, `source
//! …fish`, the dot-source for pwsh — is in the dialect of the shell it is typed into.
//!
//! `$SHELL` is the LOGIN shell, not the one in front of the user: aterm spawns the
//! configured `shell` (`ATERM_SHELL`, the config key) without re-pointing `$SHELL`, so a
//! fish tab on a zsh-login machine used to be told `. ~/.aterm/shell.d/00-atpkg.zsh` "here
//! picks the managed copy up" — a line fish cannot source (`${__atpkg_p//…}` is not fish)
//! — while the window's status row, keyed on the spawn shell, said the fish line in the
//! same tab (review finding, 2026-09-16). The signal a tab actually carries is its
//! PROCESS TREE: `aterm pkg …` is a child of the shell that typed it (`exec`'d shims keep
//! the parent), so the parent's executable name is the shell — `zsh`, `bash`, `fish`,
//! `pwsh` — read once, by pid, with no fork: `sysctl(KERN_PROCARGS2)` on macOS (the exec
//! path leads the buffer), `/proc/<pid>/exe` on Linux. Only when the parent is not a
//! shell at all (`sudo`, `make`, a Python driver) does `$SHELL` answer, as before.

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
    let filled = buf.get(..len.min(buf.len()))?;
    let after_argc = filled.get(std::mem::size_of::<libc::c_int>()..)?;
    let end = after_argc.iter().position(|&b| b == 0)?;
    let path = std::str::from_utf8(&after_argc[..end]).ok()?;
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
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
    /// kernel has the process — so measuring straight after a spawn races the exec, and the
    /// outliving case below would delete the file before exec ever read it. A timeout here
    /// means the child never got there, not that the law under test failed.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn await_exec(pid: u32, want: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if process_exe_name(pid).as_deref() == Some(want) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pid {pid} never came up as {want:?} within 30 s — the spawn or the exec \
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
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("/bin/sleep");
        await_exec(child.id(), "sleep");
        let name = process_exe_name(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(name.as_deref(), Some("sleep"));
        assert_eq!(process_exe_name(u32::MAX), None);
    }

    /// A program whose file is deleted under it is still named: the state of every shell
    /// running across a package upgrade, and the one where Linux's exe link reads
    /// `/…/sleep (deleted)`. macOS keeps answering the exec path it recorded, so both
    /// platforms are asserted the same way.
    ///
    /// The fixture must be a copy of this test binary, not of a system one: macOS refuses
    /// to exec a copy of `/bin/sleep` (an AMFI launch constraint on `com.apple.sleep`), and
    /// a child that never reaches exec leaves nothing to name.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_program_outliving_its_own_file_is_still_named() {
        if std::env::var_os(PROBE_ENV).is_some() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("atpkg-exe-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A basename of our own choosing, which is what is asserted: the reader must answer
        // the file's name, not the name of the binary it was copied from.
        let exe = dir.join("outliver");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).expect("copy this test binary");
        let mut perms = std::fs::metadata(&exe).unwrap().permissions();
        {
            use std::os::unix::fs::PermissionsExt as _;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&exe, perms).unwrap();
        // Run it as the probe: it prints one line and waits to be killed, so there is a live
        // process whose file can be deleted under it.
        let mut child = std::process::Command::new(&exe)
            .args(["--exact", "--nocapture", "--quiet", OUTLIVER_PROBE])
            .env(PROBE_ENV, "1")
            .env(OUTLIVER_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the copy of this test binary");
        // The exec must have happened before the file goes, or the test measures a failed
        // spawn instead of a program outliving its file (see `await_exec`).
        await_exec(child.id(), "outliver");
        std::fs::remove_file(&exe).expect("delete it under the running process");
        let name = process_exe_name(child.id());
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            name.as_deref(),
            Some("outliver"),
            "the reader must still name a process whose file was deleted under it"
        );
    }

    /// The env that puts the copy above into its waiting mode, and the name of the case it
    /// runs there: one test, so the copy does nothing but park until it is killed.
    const OUTLIVER_ENV: &str = "ATPKG_CALLER_SHELL_OUTLIVER";
    const OUTLIVER_PROBE: &str = "caller_shell::tests::probe_parks_until_killed";

    /// The copy's whole job: exist, under its own name, until the parent kills it.
    ///
    /// No wall clock: the park ends on an event — killed (the normal path) or the parent
    /// going away. A fixed timeout would let a descheduled parent come back to a probe that
    /// had already exited, so an expired fixture would read as the law failing. Watching
    /// the parent also keeps a stray out of the process table if the parent dies first.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn probe_parks_until_killed() {
        if std::env::var_os(OUTLIVER_ENV).is_none() {
            return;
        }
        // SAFETY: `getppid` takes no arguments, reads no memory we own and cannot
        // fail (POSIX gives it no error return).
        let spawner = unsafe { libc::getppid() };
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            // SAFETY: as above.
            if unsafe { libc::getppid() } != spawner {
                return;
            }
        }
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
}
