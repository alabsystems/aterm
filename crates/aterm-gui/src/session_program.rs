// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PROGRAM IDENTITY: `status program=` and the `sessions` row's `program=`,
//! the argv[0] basename of the PTY's foreground process-group leader.
//!
//! WHY argv[0]. `detail=` comes from shell integration alone, so an adopted
//! shell whose marks were lost, or a shell with no integration, reads `-` for
//! the life of whatever it runs. The process table always knows. But not by
//! its executable path: Claude Code's self-updater runs the binary from
//! `~/.local/share/claude/versions/2.1.280`, so `proc_pidpath` and `p_comm`
//! both say `2.1.280` (measured 2026-09-23 with `lsof`), while argv[0] — what
//! `ps -o comm` prints — says `claude`. So this reads argv[0]
//! (`KERN_PROCARGS2` on macOS, `/proc/<pid>/cmdline` on Linux).
//!
//! THE SHIM WINDOW. `claude` and `codex` typed in an aterm shell run atpkg's
//! twin, a `#!/bin/sh` script (`…/pkg/agents/claude`) that `exec`s the store
//! binary. Measured 2026-09-23 on a private headless aterm with a scratch
//! HOME: once exec'd, the store claude (2.1.280) and codex both keep the
//! store path as argv[0] (`…/store/claude/2026092201/bin/claude`), so they
//! read `claude` / `codex`. But until the `exec` the group's leader is
//! `/bin/sh …/agents/claude` — argv[0] `/bin/sh` — and the exec keeps the
//! pid and the group, so a resolution that landed in that window read `sh`
//! (measured, with a twin that had not exec'd yet) and stood until the screen
//! moved 5 s later; a trust dialog that then sat still stayed `program=sh
//! agent=-`. So a shell interpreter whose first argument is atpkg's OWN shim
//! for an agent — a script named after it, directly in the managed prefix's
//! `agents/` or `bin/` ([`atpkg_shim`]) — is that agent ([`program_from_argv`]).
//! Only that file: a user's own wrapper script named `claude` elsewhere keeps
//! the interpreter's name, so the in-GUI supervisor never attaches to it.
//!
//! WHEN. The status sweep already holds each due session's `tcgetpgrp`
//! answer; it asks for a resolution when the foreground group CHANGES
//! (`SessionTimeline::note_foreground_group`), and again at most every
//! `PROGRAM_RECHECK` while the screen moves — the one program change that
//! keeps its group is an `exec` in place (`cd ~/ay && exec claude`). The
//! sysctl runs on one background thread ([`ProgramResolver`]), never on the
//! event loop.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use crate::session_timeline::SessionTimeline;

/// The longest program token published, in bytes.
const PROGRAM_MAX: usize = 32;

/// The program name argv[0] names: its basename, a login shell's leading `-`
/// dropped (`-zsh` is `zsh`), and a vendor VERSION DIRECTORY read as the tool
/// it holds (`…/claude/versions/2.1.280` is `claude`). Reduced to
/// `[A-Za-z0-9._+-]` and capped at [`PROGRAM_MAX`] bytes; `None` when nothing
/// is left.
pub(crate) fn program_from_argv0(argv0: &str) -> Option<String> {
    let trimmed = argv0.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    let base = parts.next().unwrap_or("");
    let base = match (parts.next(), parts.next()) {
        (Some("versions"), Some(tool)) if base.starts_with(|c: char| c.is_ascii_digit()) => tool,
        _ => base,
    };
    let name: String = base
        .trim_start_matches('-')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        .take(PROGRAM_MAX)
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The interpreters an agent's shim script runs under (`#!/bin/sh`), whose
/// first argument then names the script ([`program_from_argv`]).
const SCRIPT_SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "ksh"];

/// The published program of a process whose argv begins `argv0 [argv1]`:
/// [`program_from_argv0`], except that a shell interpreter
/// ([`SCRIPT_SHELLS`]) running a SCRIPT (`argv1` is not an option) whose
/// name is an agent's (`claude`, `codex` — `aterm_phase::program_of`) AND
/// that `is_shim` says is atpkg's own shim ([`atpkg_shim`] in production) is
/// that agent: atpkg's shim before its `exec` (module header, "THE SHIM
/// WINDOW"). Any other script keeps the interpreter's name (`sh build.sh` is
/// `sh`, and so is `sh ~/bin/claude`, a user's own wrapper), so nothing else
/// changes name. `is_shim` is asked only for an agent-named script.
pub(crate) fn program_from_argv(
    argv0: &str,
    argv1: Option<&str>,
    is_shim: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let name = program_from_argv0(argv0)?;
    if SCRIPT_SHELLS.contains(&name.as_str())
        && let Some(script) = argv1.filter(|a| !a.starts_with('-'))
        && let Some(agent) = program_from_argv0(script)
        && aterm_phase::program_of(&agent).is_some()
        && is_shim(script)
    {
        return Some(agent);
    }
    Some(name)
}

/// Whether `script`'s directory is one of `dirs`, as spelled or resolved
/// (a prefix reached through a symlink, a PATH entry spelled another way).
fn script_in(script: &str, dirs: &[std::path::PathBuf]) -> bool {
    let Some(parent) = std::path::Path::new(script).parent() else {
        return false;
    };
    let resolved = std::fs::canonicalize(parent).ok();
    dirs.iter().any(|dir| {
        dir == parent
            || resolved
                .as_ref()
                .is_some_and(|r| std::fs::canonicalize(dir).is_ok_and(|d| d == *r))
    })
}

/// Whether `script` is atpkg's own shim: a file directly in the configured
/// managed prefix's `agents/` (the twin) or `bin/`
/// (`atpkg::store::resolve_configured`, the prefix every reader outside
/// atpkg's CLI resolves). Reads `aterm.toml` and stats the prefix, so it runs
/// only for an agent-named script, on the resolver thread.
pub(crate) fn atpkg_shim(script: &str) -> bool {
    atpkg::store::resolve_configured()
        .is_some_and(|layout| script_in(script, &[layout.agents_dir(), layout.bin_dir()]))
}

/// argv[0] and argv[1] of `pid`, as the process itself was exec'd with them,
/// or `None` (gone, another user's, or a platform without the call).
pub(crate) fn argv_head_of(pid: i32) -> Option<(String, Option<String>)> {
    if pid <= 0 {
        return None;
    }
    platform_argv_head(pid)
}

/// The published program of `pid`: [`program_from_argv`] of
/// [`argv_head_of`], atpkg's shims told by [`atpkg_shim`].
pub(crate) fn program_of(pid: i32) -> Option<String> {
    let (argv0, argv1) = argv_head_of(pid)?;
    program_from_argv(&argv0, argv1.as_deref(), &atpkg_shim)
}

#[cfg(target_os = "macos")]
fn platform_argv_head(pid: i32) -> Option<(String, Option<String>)> {
    // KERN_PROCARGS2 answers `int argc`, the exec path (NUL-terminated, then
    // NUL padding), then argv[0..argc], each NUL-terminated.
    // `KERN_PROCARGS2` (`<sys/sysctl.h>`), which the workspace's libc does
    // not export.
    const KERN_PROCARGS2: libc::c_int = 49;
    let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query (null buffer) on a fixed three-int MIB.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size < std::mem::size_of::<libc::c_int>() {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` holds `size` writable bytes and `size` says so; the kernel
    // writes at most that many and updates `size` to what it wrote.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    argv_head_from_procargs2(&buf)
}

#[cfg(target_os = "linux")]
fn platform_argv_head(pid: i32) -> Option<(String, Option<String>)> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let mut args = bytes.split(|b| *b == 0);
    let first = args.next().filter(|a| !a.is_empty())?;
    let second = args.next().filter(|a| !a.is_empty());
    let text = |a: &[u8]| String::from_utf8_lossy(a).into_owned();
    Some((text(first), second.map(text)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_argv_head(_pid: i32) -> Option<(String, Option<String>)> {
    None
}

/// argv[0] and (when `argc > 1`) argv[1] out of a `KERN_PROCARGS2` buffer.
/// Pure, so the layout is testable without a process.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn argv_head_from_procargs2(buf: &[u8]) -> Option<(String, Option<String>)> {
    let argv0 = argv0_from_procargs2(buf)?;
    let int = std::mem::size_of::<i32>();
    let argc = i32::from_ne_bytes(buf.get(..int)?.try_into().ok()?);
    let argv1 = (argc > 1)
        .then(|| {
            let rest = &buf[int..];
            let path_end = rest.iter().position(|b| *b == 0)?;
            let rest = &rest[path_end..];
            let start = rest.iter().position(|b| *b != 0)?;
            let rest = &rest[start..];
            let end0 = rest.iter().position(|b| *b == 0)?;
            let rest = rest.get(end0 + 1..)?;
            let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
            let arg = &rest[..end];
            (!arg.is_empty()).then(|| String::from_utf8_lossy(arg).into_owned())
        })
        .flatten();
    Some((argv0, argv1))
}

/// argv[0] out of a `KERN_PROCARGS2` buffer.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn argv0_from_procargs2(buf: &[u8]) -> Option<String> {
    let int = std::mem::size_of::<i32>();
    let argc = i32::from_ne_bytes(buf.get(..int)?.try_into().ok()?);
    if argc < 1 {
        return None;
    }
    let rest = &buf[int..];
    // Skip the exec path, then the NUL padding after it.
    let path_end = rest.iter().position(|b| *b == 0)?;
    let rest = &rest[path_end..];
    let start = rest.iter().position(|b| *b != 0)?;
    let rest = &rest[start..];
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    let argv0 = &rest[..end];
    (!argv0.is_empty()).then(|| String::from_utf8_lossy(argv0).into_owned())
}

/// One resolution request: the session's timeline and the group to name.
struct Job {
    timeline: Arc<Mutex<SessionTimeline>>,
    pgid: i32,
}

/// The background thread program resolutions run on, started on first use.
/// A request whose group has left the foreground by the time it is answered
/// is dropped by [`SessionTimeline::set_program`].
#[derive(Default)]
pub(crate) struct ProgramResolver {
    tx: Option<Sender<Job>>,
}

impl std::fmt::Debug for ProgramResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgramResolver")
            .field("started", &self.tx.is_some())
            .finish()
    }
}

impl ProgramResolver {
    /// Resolve `pgid`'s leader off-thread into `timeline`. If the thread cannot
    /// be started the program stays unknown (`program=-`), never guessed.
    pub(crate) fn request(&mut self, timeline: &Arc<Mutex<SessionTimeline>>, pgid: i32) {
        if pgid <= 0 {
            return;
        }
        if self.tx.is_none() {
            let (tx, rx) = channel::<Job>();
            let spawned = std::thread::Builder::new()
                .name("aterm-program-id".into())
                .spawn(move || {
                    // It takes the session timeline lock the UI thread
                    // contends, so the lock-holder floor applies.
                    crate::qos::set_self(crate::qos::Role::Responsive);
                    while let Ok(job) = rx.recv() {
                        resolve_into(&job.timeline, job.pgid);
                    }
                });
            match spawned {
                Ok(_) => self.tx = Some(tx),
                Err(e) => {
                    aterm_log::warn!("program identity: could not start the resolver: {e}");
                    return;
                }
            }
        }
        if let Some(tx) = &self.tx
            && tx
                .send(Job {
                    timeline: Arc::clone(timeline),
                    pgid,
                })
                .is_err()
        {
            // The thread is gone (it only ends when the channel closes, so
            // this is a panic in the syscall path); start a fresh one next time.
            self.tx = None;
        }
    }
}

/// Resolve `pgid`'s leader and publish it on `timeline` — the resolver
/// thread's body, callable inline where a test wants the answer now. The
/// syscall runs before the (leaf) timeline lock is taken.
pub(crate) fn resolve_into(timeline: &Arc<Mutex<SessionTimeline>>, pgid: i32) {
    let program = program_of(pgid);
    timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_program(pgid, program);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv0_reduces_to_the_program_name() {
        assert_eq!(program_from_argv0("claude").as_deref(), Some("claude"));
        assert_eq!(program_from_argv0("-zsh").as_deref(), Some("zsh"));
        assert_eq!(program_from_argv0("/bin/sleep").as_deref(), Some("sleep"));
        assert_eq!(
            program_from_argv0("/Users//x/.local/share/claude/versions/2.1.280").as_deref(),
            Some("claude"),
            "a vendor version directory names its tool, not its version"
        );
        assert_eq!(
            program_from_argv0("/opt/versions/tool").as_deref(),
            Some("tool"),
            "only a VERSION-shaped basename under versions/ is rewritten"
        );
        assert_eq!(
            program_from_argv0("evil\u{1b}]0;x\u{7}").as_deref(),
            Some("evil0x")
        );
        assert_eq!(program_from_argv0(""), None);
        assert_eq!(program_from_argv0("///"), None);
        assert_eq!(
            program_from_argv0(&"a".repeat(100)).map(|s| s.len()),
            Some(32)
        );
    }

    #[test]
    fn procargs2_layout_yields_argv0_not_the_exec_path() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/Users//x/.local/share/claude/versions/2.1.280\0\0\0\0");
        buf.extend_from_slice(b"claude\0--resume\0PATH=/bin\0");
        assert_eq!(argv0_from_procargs2(&buf).as_deref(), Some("claude"));
        assert_eq!(argv0_from_procargs2(&0i32.to_ne_bytes()), None, "argc 0");
        assert_eq!(argv0_from_procargs2(b"\x01"), None, "short buffer");
    }

    /// A real child: its argv[0] is read from the process table, and a
    /// renamed argv[0] (`exec -a claude`) is what is published — the shape an
    /// agent launched through a shim or a version directory takes.
    #[cfg(unix)]
    #[test]
    fn a_live_process_is_named_by_its_argv0() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = i32::try_from(child.id()).expect("pid");
        let named = program_of(pid);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(named.as_deref(), Some("sleep"));
        assert_eq!(program_of(-1), None);

        // `exec -a` is a bash builtin (not POSIX sh's).
        if !std::path::Path::new("/bin/bash").exists() {
            return;
        }
        let mut child = std::process::Command::new("/bin/bash")
            .args(["-c", "exec -a claude sleep 30"])
            .spawn()
            .expect("spawn sh");
        let pid = i32::try_from(child.id()).expect("pid");
        // `exec -a` replaces the shell in place; wait for the new image.
        let mut named = None;
        for _ in 0..200 {
            named = program_of(pid);
            if named.as_deref() == Some("claude") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(named.as_deref(), Some("claude"));
    }

    /// The shim window, pure: an interpreter running atpkg's shim NAMED after
    /// an agent is the agent; the measured store path names its tool.
    /// NEGATIVE CONTROLS: any other script, an option, a login shell, a
    /// non-shell with an agent-named argument, and an agent-named script that
    /// is NOT atpkg's (a user's own `~/bin/claude` wrapper) keep their own
    /// names.
    #[test]
    fn a_shim_script_named_after_an_agent_is_that_agent() {
        let atpkg = |script: &str| script.contains("/pkg/agents/");
        let p = |a0: &str, a1: Option<&str>| program_from_argv(a0, a1, &atpkg);
        let twin = "/Users//x/Library/Application Support/aterm/pkg/agents/claude";
        assert_eq!(p("/bin/sh", Some(twin)).as_deref(), Some("claude"));
        assert_eq!(
            p("/bin/bash", Some("/opt/pkg/agents/codex")).as_deref(),
            Some("codex")
        );
        assert_eq!(
            p(
                "/Users//x/Library/Application Support/aterm/pkg/store/claude/2026092201/bin/claude",
                None
            )
            .as_deref(),
            Some("claude"),
            "the exec'd store claude, argv[0] as measured"
        );
        assert_eq!(p("/bin/sh", Some("build.sh")).as_deref(), Some("sh"));
        assert_eq!(p("/bin/sh", Some("-c")).as_deref(), Some("sh"));
        assert_eq!(p("-zsh", None).as_deref(), Some("zsh"));
        assert_eq!(p("sleep", Some("claude")).as_deref(), Some("sleep"));
        assert_eq!(p("", Some(twin)), None);
        assert_eq!(
            p("/bin/sh", Some("/Users//x/bin/claude")).as_deref(),
            Some("sh"),
            "a user's own wrapper named claude is not the agent"
        );
    }

    /// [`script_in`]: the script's own directory, as spelled or resolved —
    /// never a deeper one or a sibling.
    #[cfg(unix)]
    #[test]
    fn a_shim_is_a_file_directly_in_a_managed_dir() {
        let dir = std::env::temp_dir().join(format!("aterm-shimdir-{}", std::process::id()));
        let agents = dir.join("pkg/agents");
        std::fs::create_dir_all(&agents).expect("scratch dir");
        std::os::unix::fs::symlink(dir.join("pkg"), dir.join("link")).expect("symlink");
        let dirs = [agents.clone()];
        let at = |p: std::path::PathBuf| script_in(&p.to_string_lossy(), &dirs);
        assert!(at(agents.join("claude")));
        assert!(
            at(dir.join("link/agents/claude")),
            "the prefix through a link"
        );
        assert!(!at(agents.join("sub/claude")), "deeper");
        assert!(!at(dir.join("pkg/claude")), "the prefix itself");
        assert!(!at(dir.join("elsewhere/agents/claude")), "another agents/");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn procargs2_layout_yields_argv1_after_argv0() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/sh\0\0\0");
        buf.extend_from_slice(b"/bin/sh\0/pkg/agents/claude\0--resume\0PATH=/bin\0");
        assert_eq!(
            argv_head_from_procargs2(&buf),
            Some((
                "/bin/sh".to_string(),
                Some("/pkg/agents/claude".to_string())
            ))
        );
        let mut one = 1i32.to_ne_bytes().to_vec();
        one.extend_from_slice(b"/bin/zsh\0\0-zsh\0HOME=/x\0");
        assert_eq!(
            argv_head_from_procargs2(&one),
            Some(("-zsh".to_string(), None)),
            "argc 1: the environment is not argv[1]"
        );
    }

    /// A real shim in its window: `/bin/sh <dir>/agents/claude` that has not
    /// exec'd yet reads `claude` when `<dir>/agents` is the managed one.
    /// NEGATIVE CONTROLS: the same script under another name reads `sh`, and
    /// so does the agent-named one when its directory is not managed.
    #[cfg(unix)]
    #[test]
    fn a_live_shim_before_its_exec_is_named_by_its_script() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-shim-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("agents")).expect("scratch dir");
        let mut named = Vec::new();
        for name in ["agents/claude", "build.sh"] {
            let script = dir.join(name);
            std::fs::write(&script, "#!/bin/sh\nsleep 30\nexit 0\n").expect("write the shim");
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            let mut child = std::process::Command::new(&script)
                .spawn()
                .expect("spawn the shim");
            let pid = i32::try_from(child.id()).expect("pid");
            // The kernel runs `#!` scripts as `/bin/sh <script>` from the
            // first instruction, so the name is readable at once.
            let head = argv_head_of(pid);
            let _ = child.kill();
            let _ = child.wait();
            let (argv0, argv1) = head.expect("the shim's argv");
            let managed = [dir.join("agents")];
            let unmanaged = [dir.join("elsewhere")];
            named.push((
                program_from_argv(&argv0, argv1.as_deref(), &|s| script_in(s, &managed)),
                program_from_argv(&argv0, argv1.as_deref(), &|s| script_in(s, &unmanaged)),
            ));
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(named[0].0.as_deref(), Some("claude"));
        assert_eq!(named[0].1.as_deref(), Some("sh"), "not the managed agents/");
        assert_eq!(named[1].0.as_deref(), Some("sh"));
    }

    #[test]
    fn a_late_answer_for_a_departed_group_is_dropped() {
        let tl = Arc::new(Mutex::new(SessionTimeline::default()));
        assert!(tl.lock().unwrap().note_foreground_group(10));
        tl.lock().unwrap().set_program(10, Some("sleep".into()));
        assert_eq!(tl.lock().unwrap().agent().program.as_deref(), Some("sleep"));
        assert!(!tl.lock().unwrap().note_foreground_group(10), "same group");
        assert!(tl.lock().unwrap().note_foreground_group(11));
        assert_eq!(
            tl.lock().unwrap().agent().program,
            None,
            "the old name goes"
        );
        tl.lock().unwrap().set_program(10, Some("sleep".into()));
        assert_eq!(
            tl.lock().unwrap().agent().program,
            None,
            "late answer dropped"
        );
    }
}
