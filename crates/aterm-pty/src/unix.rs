// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! POSIX backend of the PTY seam: every raw `libc` PTY syscall — `forkpty`,
//! `execve`, `read`, `write`, `ioctl(TIOCSWINSZ)` — moved VERBATIM from the
//! pre-split `lib.rs` (zero semantic change). The shared, portable items
//! ([`SpawnedShell`], [`crate::UTF8_LOCALE`], `build_child_env`) live in
//! `lib.rs`; everything in this module is Unix-only by placement (no inline
//! cfg), and `lib.rs` re-exports it so Unix callers compile untouched.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::ptr;

use crate::{SpawnedShell, build_child_env};

/// Fixed absolute path to the macOS Seatbelt wrapper used by the OS-sandbox wrap
/// (see [`spawn_shell`]'s `sandbox_wrap`). Inlined here (rather than depending on
/// the policy crate) to keep this minimal syscall seam dependency-light; it MUST
/// equal `aterm_containment::SANDBOX_EXEC_PATH` — a test in this crate locks that.
const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// Spawn `$SHELL` in a fresh PTY of `rows`×`cols`, returning the master fd.
///
/// Honors `$ATERM_EXEC`: if set, the shell runs that command first (to paint a
/// known screen) and then `exec`s an interactive shell so the result persists.
/// Defaults to `/bin/sh` when `$SHELL` is unset.
///
/// `env_add` is a set of `(key, value)` environment entries injected into the
/// child before exec (e.g. the OSC 133/633 shell-integration loader vars +
/// nonce); `argv_override`, when `Some`, replaces the shell's argv (e.g. bash's
/// `--rcfile`). Both are GENERIC — this seam knows nothing about shell
/// integration; the frontend computes them. Pass `&[]` / `None` for a bare
/// interactive shell.
///
/// `exec_command`, when `Some(&[prog, args…])`, runs that command DIRECTLY in the
/// PTY instead of a shell (the `-e` convention: when it exits, the PTY closes and
/// the window follows). `prog` is PATH-resolved HERE in the parent (the child must
/// stay async-signal-safe, so no `execvp` PATH search there); `argv[0]` is `prog`
/// as given. It takes precedence over `argv_override` and `$ATERM_EXEC` — there is
/// no interactive shell to integrate with. An unresolved/again-failing `prog` ends
/// the child with `_exit(127)`, closing the window, just like a failed shell exec.
///
/// `cwd`, when `Some`, is the working directory the child `chdir`s into before
/// exec (the `--working-directory` flag); it overrides the default
/// `/`→`$HOME` Finder-launch fallback. A failed `chdir` is non-fatal (the child
/// starts in the inherited directory), matching the existing best-effort `chdir`.
///
/// ## OS sandbox wrap (`sandbox_wrap`, macOS Seatbelt — ATERM_DESIGN §5.6)
///
/// `sandbox_wrap`, when `Some(sbpl)`, wraps the WHOLE resolved program+argv in
/// `/usr/bin/sandbox-exec -p <sbpl>` so the macOS kernel Seatbelt applies the SBPL
/// profile (e.g. `(deny network*)` for `Containment` mode) before the target
/// `exec`s. The wrap is BUILT IN THE PARENT: `sandbox-exec` becomes the exec
/// target (a fixed absolute path — no PATH search, async-signal-safe in the child)
/// and the original program+argv become its trailing arguments, so the login-shell
/// argv[0], `--rcfile`, `$ATERM_EXEC`, and `-e` paths are all preserved verbatim
/// as what sandbox-exec runs. This is **fail-closed**: if `sandbox-exec` is not
/// present at its fixed path, `spawn_shell` returns an error and does NOT spawn —
/// it never silently runs an UNSANDBOXED shell when the caller demanded the
/// sandbox. `None` means no wrap: the spawn is byte-identical to before (used for
/// every non-`Containment` mode, so the default User-mode spawn is unchanged).
///
/// Spawning a child process is a privileged effect (ATERM_DESIGN WS-G), so it
/// requires a `Cap<Spawn>` of at least `Trusted` tier (`aterm-cap`): there is no
/// way to spawn without one.
///
/// ## Fail-closed confinement (ATERM_DESIGN §5.6, exit-before-exec)
///
/// The child applies the resource sandbox BEFORE `execve`. If the sandbox
/// `apply()` returns an error the child does NOT exec — it writes a one-byte
/// failure indicator on the close-on-exec status pipe and `_exit(126)`s, so a
/// confinement failure can never silently hand back a master fd for an
/// UNCONFINED shell. The parent reads the status pipe: a clean EOF (the write
/// end closed by `execve`'s O_CLOEXEC) means the child exec'd confined; any byte
/// means the child failed before exec, and the parent returns an error instead
/// of the master fd.
///
/// # Errors
/// Returns `PermissionDenied` if the capability's tier is too low, the OS error
/// if `forkpty`/`pipe` fails, or `PermissionDenied`/`Other` if the child failed
/// to confine itself (sandbox `apply` error) or to `execve` before exec. On any
/// pre-exec child failure the master fd is closed and NO unconfined shell is
/// returned.
// The arg list is intentionally wide: this is the SINGLE spawn seam, and each
// argument is an independent, security-relevant input (caps, env, argv, cwd, the
// OS-sandbox wrap). Bundling them into a struct would hide that surface, not
// shrink it.
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    shell_override: Option<&str>,
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[String]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
) -> io::Result<i32> {
    // Thin compatibility wrapper: drop the child pid. Callers that need the pid
    // for a graceful, NON-BLOCKING teardown (SIGHUP the controlling-tty session
    // before closing the master — see `spawn_shell_with_pid`) use that instead.
    spawn_shell_with_pid(
        rows,
        cols,
        cap,
        sandbox_cap,
        env_add,
        shell_override,
        shell_args,
        argv_override,
        exec_command,
        cwd,
        sandbox_wrap,
        // The thin wrapper preserves the historical hardened default; the GUI's
        // real spawn path picks the limits by containment mode.
        aterm_sandbox::Limits::shell_default(),
    )
    .map(|s| s.master)
}

/// The UTF-8 locale [`resolve_spawn_locale`] INJECTS into spawned children when the
/// inherited locale is non-UTF-8. On macOS `en_US.UTF-8` is guaranteed; OFF macOS use
/// `C.UTF-8` — the locale-INDEPENDENT UTF-8 codeset present on effectively every Linux
/// (glibc 2.35+ and always on musl) and modern BSD. Naming `en_US.UTF-8` off macOS is
/// unsafe: minimal Debian/Ubuntu, virtually all Docker base images, and musl/Alpine do
/// NOT generate it, so glibc silently falls back to C/POSIX (ASCII — the exact mojibake
/// this override exists to prevent) AND every locale-aware child (perl) then prints
/// `Setting locale failed`. Kept separate from [`UTF8_LOCALE`], which is the macOS-only
/// pbcopy/pbpaste pin; on macOS both resolve to the same value so they cannot drift.
const SPAWN_UTF8_LOCALE: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

/// Whether a locale string selects a UTF-8 character encoding.
///
/// A POSIX locale is `language[_TERRITORY][.codeset][@modifier]`; the *codeset*
/// (the part after the last `.`, with any trailing `@modifier` stripped) decides
/// the encoding. The match is case-insensitive and ignores `-`, so `.UTF-8`,
/// `.UTF8`, `.utf-8`, `.utf8`, and `.UTF-8@euro` all qualify, while `C`, `POSIX`,
/// a bare `en_US` (no codeset), and `.ISO8859-1` do not.
fn is_utf8_locale(loc: &str) -> bool {
    let Some(dot) = loc.rfind('.') else {
        return false;
    };
    // `dot` is a byte index returned by `rfind('.')`, so `dot < loc.len()` and,
    // `.` being one byte, `dot + 1` is `<= loc.len()` and on a char boundary:
    // the add cannot wrap and the range is always valid, so neither the
    // `saturating_add` clamp nor the `unwrap_or("")` fallback ever fires —
    // behavior-identical while discharging the Trust slice/overflow obligations.
    let codeset = loc
        .get(dot.saturating_add(1)..)
        .unwrap_or("")
        .split('@')
        .next()
        .unwrap_or("");
    let norm: String = codeset
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect();
    norm == "utf8"
}

/// Resolve the locale overrides aterm must inject so the spawned child always runs
/// under a UTF-8 `LC_CTYPE`.
///
/// `LC_CTYPE` is the POSIX category that decides character encoding; locale-aware
/// programs (emacs, vim, python, tmux, perl, …) consult it to choose whether
/// terminal I/O is UTF-8. aterm's parser is UTF-8-only, so if the child runs under
/// a non-UTF-8 `LC_CTYPE` those programs re-encode multibyte text (e.g. pasted
/// box-drawing `┌─┐`) into the ASCII codeset and emit a literal `?` per character.
/// The terminal must therefore GUARANTEE a UTF-8 `LC_CTYPE` regardless of what
/// locale fragments it inherited.
///
/// `lc_all`/`lc_ctype`/`lang` are the inherited values: `None` = unset; `Some("")`
/// = set-but-empty, which POSIX treats as unset for category resolution (it falls
/// through to the next level). The *effective* encoding category follows POSIX
/// precedence **`LC_ALL` > `LC_CTYPE` > `LANG`**.
///
/// Returns the `(key, value)` pairs to APPEND to `env_add` (applied by
/// [`build_child_env`], which overrides an inherited key or appends a new one):
/// - **EMPTY** when the effective encoding is already UTF-8 — the user's locale is
///   left completely untouched (the common case; keeps every existing spawn test green).
/// - Otherwise `LC_CTYPE=`[`SPAWN_UTF8_LOCALE`] — the minimal override: it fixes only
///   the encoding category and dominates `LANG`. That value is `en_US.UTF-8` on macOS
///   (guaranteed present) and `C.UTF-8` off macOS (the locale-INDEPENDENT UTF-8 codeset
///   present on effectively every Linux/musl/BSD, where `en_US.UTF-8` is NOT). We
///   deliberately do NOT guess a territory locale (e.g. `fr_FR.UTF-8`) that may be
///   absent and would silently fall back to `C`, reintroducing the bug.
/// - …PLUS `LC_ALL=""` when a non-empty `LC_ALL` is the dominating inherited value:
///   `LC_ALL` would otherwise override the injected `LC_CTYPE` (it sits above it in
///   precedence), so we NEUTRALIZE it via POSIX empty-string fall-through. This is
///   surgical — the user's `LANG`/other `LC_*` still drive collation/messages/etc.;
///   only the encoding category is forced to UTF-8.
///
/// Pure in its inputs (like [`build_child_env`]) so it is unit-tested without
/// mutating the process-global environment, and called in the PARENT before
/// `forkpty` where allocation / env reads are safe. The property "the child's
/// effective `LC_CTYPE` is UTF-8 for every inherited locale shape" is proven by the
/// `SpawnLocale` Tier-0 `ty` model (`aterm_spec::derive::spawn_locale_model`) and
/// bound to this real function by the `spawn_locale_*` conformance tests below.
#[must_use]
pub fn resolve_spawn_locale(
    lc_all: Option<&str>,
    lc_ctype: Option<&str>,
    lang: Option<&str>,
) -> Vec<(String, String)> {
    // POSIX: an empty value is treated as unset for category resolution.
    fn set(o: Option<&str>) -> Option<&str> {
        o.filter(|s| !s.is_empty())
    }
    // Effective encoding category under precedence LC_ALL > LC_CTYPE > LANG.
    let effective = set(lc_all).or(set(lc_ctype)).or(set(lang));
    // Already UTF-8 (incl. a UTF-8 dominating LC_ALL): change nothing.
    if effective.is_some_and(is_utf8_locale) {
        return Vec::new();
    }
    let mut overrides = vec![("LC_CTYPE".to_string(), SPAWN_UTF8_LOCALE.to_string())];
    // A set, non-empty LC_ALL dominates LC_CTYPE; here it is necessarily non-UTF-8
    // (else `effective` above would have been UTF-8 and we'd have returned). Empty
    // it so POSIX falls through to the LC_CTYPE we just injected.
    if set(lc_all).is_some() {
        overrides.push(("LC_ALL".to_string(), String::new()));
    }
    overrides
}

/// The termios a NULL-termios `openpty`/`forkpty` gives the slave — the kernel's
/// compiled-in defaults — probed ONCE per process via a throwaway PTY pair and
/// cached. Probing (instead of hardcoding `TTYDEF_*`) guarantees "identical to
/// the NULL path except our documented deltas" by construction. `None` when the
/// probe fails; the spawn then passes NULL exactly as before.
fn kernel_default_termios() -> Option<libc::termios> {
    static DEFAULTS: std::sync::OnceLock<Option<libc::termios>> = std::sync::OnceLock::new();
    *DEFAULTS.get_or_init(|| {
        let mut m: libc::c_int = -1;
        let mut s: libc::c_int = -1;
        // SAFETY: valid out-params; null name/termios/winsize is the documented
        // "kernel defaults" form of `openpty`. Both fds are closed before return,
        // `tcgetattr` only fills the zeroed out-param.
        unsafe {
            if libc::openpty(
                &mut m,
                &mut s,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            ) != 0
            {
                return None;
            }
            let mut t: libc::termios = std::mem::zeroed();
            let ok = libc::tcgetattr(s, &mut t) == 0;
            libc::close(s);
            libc::close(m);
            ok.then_some(t)
        }
    })
}

/// Build the slave termios handed to `forkpty`: the probed kernel defaults with
/// exactly two deltas (everything else — cc chars, lflags, iflags, oflags — is
/// bit-identical):
///
///  * `IUTF8` ON. The kernel default leaves it off (probed macOS iflag
///    `0x2b02`), so canonical-mode ERASE deletes one BYTE of a multibyte UTF-8
///    char. We already force a UTF-8 locale into the child (`SPAWN_UTF8_LOCALE`);
///    the tty line discipline should agree. Terminal.app/iTerm2/kitty all set it.
///  * ispeed/ospeed `B230400` instead of the default `B9600`. PTY speed is
///    nominal (no UART), but xnu's `ttsetwater` sizes the slave output-queue
///    high-water from ospeed. MEASURED on this M5 Max (2026-07-19 probe): the
///    master-read chunk stays hard-clamped at exactly 1024 B/read at ospeeds
///    9600/38400/230400/1e6 (32 MiB → 32768 reads each), so this is expected
///    ~neutral here — kept because it is free, matches other emulators, and the
///    clamp may differ on other xnu/Linux versions. A/B-revert:
///    `ATERM_PTY_NULL_TERMIOS=1`.
///
/// `bench_no_opost` (BENCH-ONLY, env `ATERM_PTY_BENCH_NO_OPOST=1`) additionally
/// clears `OPOST`: the probe showed kernel output post-processing (ONLCR `\n`
/// scan) costs ~6% of raw PTY throughput (209.9 vs ~197 MB/s). It CHANGES
/// cooked-mode display semantics (bare `\n` stairsteps, no `\r` insertion), so
/// it must never be on for interactive use — default OFF, measurement only.
fn build_spawn_termios(mut t: libc::termios, bench_no_opost: bool) -> libc::termios {
    t.c_iflag |= libc::IUTF8;
    // SAFETY: `cfsetspeed` only writes the speed fields (and, on Linux, CBAUD
    // bits) of the valid, stack-owned `t`.
    unsafe {
        libc::cfsetspeed(&mut t, libc::B230400);
    }
    if bench_no_opost {
        t.c_oflag &= !libc::OPOST;
    }
    t
}

/// The termios passed to `forkpty` — `None` means NULL (kernel defaults), which
/// is the historical spawn path, byte-identical behavior. `ATERM_PTY_NULL_TERMIOS=1`
/// forces that historical path (the A/B revert switch for the
/// [`build_spawn_termios`] deltas). Runs in the PARENT before `forkpty` (env
/// reads + the one-time probe allocate; the post-fork child stays
/// async-signal-safe and untouched).
fn spawn_termios() -> Option<libc::termios> {
    if std::env::var_os("ATERM_PTY_NULL_TERMIOS").is_some_and(|v| v == "1") {
        return None;
    }
    let bench_no_opost = std::env::var_os("ATERM_PTY_BENCH_NO_OPOST").is_some_and(|v| v == "1");
    kernel_default_termios().map(|t| build_spawn_termios(t, bench_no_opost))
}

/// Like [`spawn_shell`] but also returns the child pid (see [`SpawnedShell`]).
/// Identical spawn/sandbox/exec behavior — `spawn_shell` is this minus the pid.
///
/// SPEC: the parent-prebuild + child branch of this `forkpty` seam is the real
/// implementation of the external `ForkExec.tla` model (TRUST_NATIVE_TLA Phase 2,
/// PTY-spawn SAFETY family, WS-G). The spec's ordered child program-counter walk
/// `Fork → Setrlimit → Chdir → CloseMaster → Exec` is exactly the child branch
/// below (`forkpty` at the `pid == 0` branch: `Limits::apply` = `Setrlimit`, `chdir`
/// = `Chdir`, `close(master)` = `CloseMaster`, `execve` = `Exec`), and the parent
/// pre-builds `envp`/argv BEFORE `forkpty` (the spec's `envPrebuilt = ~Buggy`) so
/// `OnlySafeBeforeExec` / `MasterClosedBeforeExec` / `SafeImpliesEnvPrebuilt` hold.
///
/// NO Tier-1 conformance is attached (honest): the modeled trajectory lives in the
/// real CHILD after `fork`, which `execve`s or `_exit`s — it can never be driven
/// in-process to observe the `pc` walk as projectable state. The binding is
/// structural (anchors close obligations 1/3/4); the BEHAVIORAL guarantees are
/// proven in the abstract (Tier-0 `ty check` of `ForkExec.tla`) and defended by the
/// crate's real fork/exec unit tests (fail-closed-on-sandbox-failure, master-fd not
/// leaked). `UnsafeEnvOp` is `#[spec_unmodeled]` — it exists ONLY in the spec's
/// `Buggy` branch (the pre-fix child's setenv/alloc in the window); the fixed code
/// has NO such step, so there is nothing to bind.
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): each fork_exec action projects the
// real child program-counter walk onto the spec's `<<pc, masterClosed, unsafeOpRan,
// envPrebuilt>>`. The witness is `aterm_pty::child_spawn::project_pc` — the structural
// projection of the child's ordered step list (BeforeFork→…→Execed) that the fork/exec
// unit tests drive. fork_exec is NOT in-process Tier-1 (the post-fork child cannot be
// driven from the test harness — the gate's ISOLATION note), so the projection is named
// for L2 (Trust requires a non-empty projection NAME, not its execution); the behavioral
// binding is the crate's real fork/exec unit tests.
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Fork",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Setrlimit",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Chdir",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "CloseMaster",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "fork_exec",
        action = "Exec",
        project = "aterm_pty::child_spawn::project_pc"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::spec_unmodeled(
        machine = "fork_exec",
        action = "UnsafeEnvOp",
        reason = "Modeled DEFECT only: UnsafeEnvOp fires solely in ForkExec.tla's Buggy branch \
                  (the pre-fix child running setenv/var_os/current_dir/CString/format!/Vec — \
                  async-signal-UNSAFE work in the fork..exec window). The fixed child runs NONE \
                  of these (all env/argv/envp is pre-built in the parent before forkpty), so \
                  there is no shipping code to bind; the action exists to let ty PROVE the \
                  defect is excluded (OnlySafeBeforeExec) at Buggy=TRUE."
    )
)]
// Skip: extraction classifies this body `TreatedAsAssumption(AddressOfField)`
// (a `&raw`-of-field shape the extractor cannot yet model) — the DEFAULT lane
// records exactly that assumption row, but the explicit-full gate lane ABORTS
// the whole crate on it. The explicit skip is the same epistemic state
// (unverified-by-capability-gap, machine-visible), spelled through the honored
// opt-out channel. The fork..exec window's safety is separately machine-checked
// by the ForkExec.tla refinement anchors above (OnlySafeBeforeExec proven).
// Droppable when AddressOfField extraction lands.
#[cfg_attr(trust_verify, trust::skip)]
#[allow(clippy::too_many_arguments)]
pub fn spawn_shell_with_pid(
    rows: u16,
    cols: u16,
    cap: &aterm_cap::Cap<aterm_cap::effects::Spawn>,
    sandbox_cap: &aterm_cap::Cap<aterm_sandbox::Sandbox>,
    env_add: &[(String, String)],
    // Config `shell` / `--shell`: the program to run instead of `$SHELL`. An
    // absolute path or a bare name (used verbatim as the exec target on Unix —
    // `execve` needs a path, so a bare name relies on it being absolute or the
    // caller's discovery having resolved it). `None` → `$SHELL` (the default).
    shell_override: Option<&str>,
    // Config `shell_args`: extra argv after argv[0]. When set, the shell runs as
    // a NON-login interactive shell with exactly these args (a login `-basename`
    // argv[0] and explicit `-l` would conflict). `None` → the default login argv.
    shell_args: Option<&[String]>,
    argv_override: Option<&[String]>,
    exec_command: Option<&[String]>,
    cwd: Option<&str>,
    sandbox_wrap: Option<&str>,
    // The `rlimit` set applied in the child before exec. The caller chooses it by
    // containment mode: hardened ([`aterm_sandbox::Limits::shell_default`]) for
    // Safety/Containment, permissive ([`aterm_sandbox::Limits::inherit`]) for the
    // daily-driver User/Master modes so normal programs aren't constrained more than
    // the launching login shell.
    limits: aterm_sandbox::Limits,
) -> io::Result<SpawnedShell> {
    aterm_cap::require(cap, aterm_cap::Tier::Trusted)
        .map_err(|e| io::Error::new(io::ErrorKind::PermissionDenied, e.to_string()))?;

    // EVERYTHING that allocates or reads the environment happens HERE, in the
    // PARENT, BEFORE forkpty. The frontend is multi-threaded (GPU/Metal + socket
    // threads are live), and POSIX permits ONLY async-signal-safe calls between
    // fork and exec — so the child below must not allocate, take the std env
    // lock, or call `setenv`. We pre-build the C arrays and hand them to
    // `execve`; a lock a vanished thread held would otherwise deadlock (or, with
    // the macOS Obj-C runtime, hard-abort) the child.
    let shell = shell_override
        .filter(|s| !s.is_empty())
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("SHELL"))
        .unwrap_or_else(|| "/bin/sh".into());
    // The fallback is a compile-time `c"..."` literal (interior-NUL-free by
    // construction), not a runtime `CString::new(..).unwrap()`: same bytes, no
    // panic path — behavior-identical while discharging the Trust unwrap
    // panic-boundary obligations.
    let cshell =
        CString::new(shell.as_os_str().as_bytes()).unwrap_or_else(|_| c"/bin/sh".to_owned());

    // envp = the inherited environment with every deny-listed key removed, then
    // `env_add` applied on top (overriding or appending). Built by `build_child_env`
    // (pure in its inputs) so the deny-list wiring is unit-tested deterministically,
    // without mutating the process-global environment. `env_store` owns the C
    // strings `envp` points into.
    let env_pairs = build_child_env(std::env::vars_os(), env_add);
    let env_store: Vec<CString> = env_pairs
        .iter()
        .filter_map(|(k, v)| {
            let mut kv = k.clone();
            kv.push("=");
            kv.push(v);
            CString::new(kv.as_bytes()).ok()
        })
        .collect();
    let mut envp: Vec<*const libc::c_char> = env_store.iter().map(|c| c.as_ptr()).collect();
    envp.push(ptr::null());

    // exec target + argv. `-e prog args…` (`exec_command`) runs the command
    // DIRECTLY and takes precedence over every shell path. Otherwise the program is
    // `$SHELL` and argv is: an explicit override (bash `--rcfile …`) wins; else
    // `$ATERM_EXEC` runs a command then execs the shell; else a LOGIN interactive
    // shell whose argv[0] is "-"+basename (the macOS convention → sources
    // .zprofile / .bash_profile / path_helper). `argv_store` + `exec_target` own
    // the C strings the child's `execve` reads.
    let (exec_target, argv_store): (CString, Vec<CString>) =
        if let Some(cmd) = exec_command.filter(|c| !c.is_empty()) {
            let argv: Vec<CString> = cmd
                .iter()
                .filter_map(|a| CString::new(a.as_bytes()).ok())
                .collect();
            (resolve_program(&cmd[0]), argv)
        } else if let Some(ov) = argv_override {
            let argv = ov
                .iter()
                .filter_map(|a| CString::new(a.as_bytes()).ok())
                .collect();
            (cshell.clone(), argv)
        } else if let Some(args) = shell_args.filter(|a| !a.is_empty()) {
            // Config `shell_args`: a non-login interactive shell, argv[0] = the
            // shell basename (not the login "-" form — explicit args own the
            // login/interactive choice), then the user's args verbatim.
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            let mut argv = vec![CString::new(base.as_bytes()).unwrap_or_else(|_| cshell.clone())];
            argv.extend(args.iter().filter_map(|a| CString::new(a.as_bytes()).ok()));
            (cshell.clone(), argv)
        } else if let Some(cmd) = std::env::var_os("ATERM_EXEC") {
            let script = format!(
                "{}; exec {}",
                cmd.to_string_lossy(),
                shell.to_string_lossy()
            );
            let argv = vec![
                cshell.clone(),
                CString::new("-c").unwrap(),
                CString::new(script).unwrap_or_else(|_| CString::new("true").unwrap()),
            ];
            (cshell.clone(), argv)
        } else {
            let base = std::path::Path::new(&shell)
                .file_name()
                .unwrap_or(shell.as_os_str());
            let mut argv0 = std::ffi::OsString::from("-");
            argv0.push(base);
            let argv = vec![CString::new(argv0.as_bytes()).unwrap_or_else(|_| cshell.clone())];
            (cshell.clone(), argv)
        };

    // OS-sandbox wrap (macOS Seatbelt, ATERM_DESIGN §5.6). When the caller demands
    // a sandbox (`Some(sbpl)` — Containment mode denies network), wrap the resolved
    // program+argv in `/usr/bin/sandbox-exec -p <sbpl>` so the kernel applies the
    // profile before the target execs. We FAIL CLOSED in the PARENT (before any
    // fork) if the wrapper binary is absent: a caller that demanded the sandbox
    // must NEVER get an unsandboxed shell. The wrapped argv is:
    //   sandbox-exec, "-p", <sbpl>, <program-path>, <original argv[1..]>
    // i.e. the original argv with argv[0] replaced by the resolved program PATH
    // (sandbox-exec execs its first positional and sets that path as the child's
    // argv[0]). This preserves every real argument (`--rcfile FILE`, `-c SCRIPT`,
    // a `-e` command's args); only the cosmetic leading-dash login marker on a
    // BARE interactive shell is dropped (a Containment shell is a non-login
    // interactive shell — an accepted, documented tradeoff for the hostile mode).
    // `exec_target`/`argv_store` from above are shadowed by the wrapped versions so
    // the rest of the seam (the C-array build, the child's execve) is unchanged.
    let (exec_target, argv_store): (CString, Vec<CString>) = if let Some(sbpl) = sandbox_wrap {
        // FAIL CLOSED in the PARENT, before any fork, if the wrapper is missing or
        // the argv can't be built — never spawn an unsandboxed shell when a sandbox
        // was demanded. The presence check + argv build is the pure, testable
        // `build_sandbox_wrap`.
        build_sandbox_wrap(SANDBOX_EXEC_PATH, sbpl, &exec_target, &argv_store)?
    } else {
        // No wrap requested → byte-identical to the pre-sandbox spawn.
        (exec_target, argv_store)
    };

    let mut argv: Vec<*const libc::c_char> = argv_store.iter().map(|c| c.as_ptr()).collect();
    argv.push(ptr::null());

    // chdir target: an explicit `--working-directory` (`cwd`) wins; else, when
    // launched from `/` (a Finder/launchd .app start), begin in $HOME instead of
    // the filesystem root. Resolved up front — the child only calls `chdir`.
    let chdir_c: Option<CString> = if let Some(dir) = cwd {
        CString::new(dir.as_bytes()).ok()
    } else if std::env::current_dir().ok().as_deref() == Some(std::path::Path::new("/")) {
        std::env::var_os("HOME").and_then(|h| CString::new(h.as_bytes()).ok())
    } else {
        None
    };

    // Exec-status pipe: a close-on-exec pipe whose write end the child holds. A
    // successful `execve` closes that end (O_CLOEXEC) and the parent reads EOF (0
    // bytes) = "child exec'd confined". A pre-exec failure (sandbox apply error,
    // or execve itself failing) makes the child WRITE a one-byte reason then
    // `_exit`, and the parent reads that byte = "child failed before exec" and
    // returns an error rather than a master fd for an unconfined shell.
    let mut status_fds = [0i32; 2];
    // SAFETY: `status_fds` is a valid 2-element buffer. (`pipe2` with O_CLOEXEC is
    // not available on macOS, so we set FD_CLOEXEC explicitly below.)
    let rc = unsafe { libc::pipe(status_fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let (status_rd, status_wr) = (status_fds[0], status_fds[1]);
    // Mark BOTH ends close-on-exec: the write end's close-on-exec close is the
    // SUCCESS signal (parent reads EOF after the child execs), and the read end
    // must not leak into the shell. Set in the PARENT, before fork (still safe to
    // allocate / call fcntl here). A failure to set CLOEXEC would break the
    // success/failure distinction, so treat it as a hard error.
    // SAFETY: both fds are valid; `fcntl(F_SETFD, FD_CLOEXEC)` only sets a flag.
    let cloexec_ok = unsafe {
        libc::fcntl(status_rd, libc::F_SETFD, libc::FD_CLOEXEC) != -1
            && libc::fcntl(status_wr, libc::F_SETFD, libc::FD_CLOEXEC) != -1
    };
    if !cloexec_ok {
        let err = io::Error::last_os_error();
        // SAFETY: closing the two pipe fds we just opened.
        unsafe {
            libc::close(status_rd);
            libc::close(status_wr);
        }
        return Err(err);
    }

    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // Explicit slave termios (kernel defaults + IUTF8 + B230400 — see
    // `build_spawn_termios`); `None` = the historical NULL path. Built here in
    // the parent so `forkpty` applies it atomically at slave-open time (no
    // post-fork tcsetattr race with the shell's own termios reads).
    let mut termp = spawn_termios();
    let mut master: libc::c_int = -1;
    // SAFETY: `forkpty` is called with a valid out-param for the master fd, null
    // for the (unused) slave-name buffer, the optional stack-owned termios (or
    // null = kernel defaults), and a valid `winsize`. It returns the child pid
    // in the parent (and 0 in the child), per POSIX.
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            ptr::null_mut(),
            termp
                .as_mut()
                .map_or(ptr::null_mut(), |t| ptr::addr_of_mut!(*t)),
            ptr::addr_of!(ws).cast_mut(),
        )
    };
    if pid < 0 {
        let err = io::Error::last_os_error();
        // SAFETY: closing the two pipe fds we just opened (fork failed).
        unsafe {
            libc::close(status_rd);
            libc::close(status_wr);
        }
        return Err(err);
    }
    if pid == 0 {
        // CHILD — async-signal-safe ONLY. Everything was pre-built in the parent
        // above; nothing here allocates, locks, or reads std env.
        // (0) the read end is the parent's; drop it in the child so only the
        //     write end (closed by exec on success) carries the status.
        // SAFETY: `status_rd` is the inherited read-end fd; `close` is a-s-safe.
        unsafe {
            libc::close(status_rd);
        }
        // (0b) NORMALIZE SIGNALS — hand the shell a POSIX-clean slate. `execve`
        //     auto-resets CAUGHT signals to SIG_DFL, but IGNORED dispositions AND the
        //     signal MASK both SURVIVE exec. Rust's std installs SIGPIPE=SIG_IGN
        //     process-wide, and aterm-gui blocks SIGUSR1 before spawning; without this
        //     reset every shell and ALL its descendants would inherit SIGPIPE ignored
        //     (spurious "Broken pipe" on `cmd | head`, pagers/`git log` not dying on
        //     SIGPIPE) and SIGUSR1 blocked. Resetting the standard signals to SIG_DFL
        //     also repairs an inherited SIGCHLD=SIG_IGN, which would otherwise stop the
        //     shell from reaping its own children (`wait` → ECHILD). SIGKILL/SIGSTOP
        //     cannot be reset (sigaction → EINVAL); skip them.
        // SAFETY: sigaction / sigemptyset / sigprocmask are async-signal-safe; the
        // sigaction/sigset values are stack-local and fully initialized by zeroed() +
        // sigemptyset before use; no allocation, no locks.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(std::ptr::addr_of_mut!(sa.sa_mask));
            sa.sa_flags = 0;
            let mut sig: libc::c_int = 1;
            while sig < 32 {
                if sig != libc::SIGKILL && sig != libc::SIGSTOP {
                    libc::sigaction(sig, std::ptr::addr_of!(sa), std::ptr::null_mut());
                }
                sig += 1;
            }
            let mut empty: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(std::ptr::addr_of_mut!(empty));
            libc::sigprocmask(
                libc::SIG_SETMASK,
                std::ptr::addr_of!(empty),
                std::ptr::null_mut(),
            );
        }
        // (1) confine resource use (WS-G auto-sandbox). FAIL-CLOSED (§5.6): if
        //     the sandbox cannot be installed, do NOT exec an unconfined shell —
        //     signal the parent and exit before exec. With a valid cap `apply`
        //     does not allocate, and `setrlimit` is async-signal-safe.
        if limits.apply(sandbox_cap).is_err() {
            // SAFETY: write a single async-signal-safe failure byte then exit.
            // `write`/`_exit` are async-signal-safe; the byte distinguishes a
            // sandbox failure (b'S') for the parent's diagnostic.
            unsafe {
                let b: u8 = b'S';
                libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
                libc::_exit(126);
            }
        }
        // (2) chdir to $HOME when started from `/`.
        if let Some(dir) = &chdir_c {
            // SAFETY: `dir` is a valid NUL-terminated path; `chdir` is async-signal-safe.
            unsafe {
                libc::chdir(dir.as_ptr());
            }
        }
        // (3) close the inherited master fd: the slave is already this child's
        //     controlling tty (forkpty's login_tty), so the master must not leak
        //     into the shell or any process it spawns.
        // SAFETY: `master` is the forkpty master fd; `close` is async-signal-safe.
        unsafe {
            libc::close(master);
        }
        // (4) exec. `execve` (not `execvp`) takes the pre-built `envp` and does no
        //     PATH-search allocation; the target is an absolute path ($SHELL, or a
        //     `-e` program already PATH-resolved in the parent).
        //     On success `execve` does not return and the O_CLOEXEC `status_wr`
        //     is closed by the kernel → parent reads EOF (confined-and-exec'd).
        //     On failure, signal the parent (b'E') and exit before any shell runs.
        // SAFETY: exec_target/argv/envp are null-terminated arrays of live C
        // strings that outlive the call; `write`/`_exit` are async-signal-safe.
        unsafe {
            libc::execve(exec_target.as_ptr(), argv.as_ptr(), envp.as_ptr());
            let b: u8 = b'E';
            libc::write(status_wr, std::ptr::addr_of!(b).cast::<libc::c_void>(), 1);
            libc::_exit(127);
        }
    }
    // PARENT. Mark the master close-on-exec FIRST: the master must not leak into
    // shells spawned by LATER sessions. Each session keeps its master open for
    // its whole lifetime (the SinkWriter Arc owns it), so without FD_CLOEXEC a
    // subsequent `forkpty`'s child would inherit every prior session's master
    // straight through `execve` — an ungated cross-session input-injection /
    // output-exfiltration channel that bypasses the WriteInput/EdgeToken gate.
    // Setting it now (post-fork) affects only FUTURE execs, so the current child
    // — which already closed its own master copy in step (3) — is unaffected;
    // and FD_CLOEXEC does not affect this parent's own read/write of the fd.
    // SAFETY: `master` is the parent's valid forkpty master fd (pid > 0 here);
    // `fcntl(F_SETFD)` is a simple fd-flag set.
    if unsafe { libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        let err = io::Error::last_os_error();
        // Fail-closed (mirrors the status-pipe handling): tear everything down —
        // close the master + both status-pipe ends, then SIGKILL and reap the
        // child so no leaky session is ever handed back to the caller.
        // SAFETY: all fds/pid are valid here; these calls are self-contained.
        unsafe {
            libc::close(master);
            libc::close(status_wr);
            libc::close(status_rd);
            libc::kill(pid, libc::SIGKILL);
            let mut wstatus: libc::c_int = 0;
            libc::waitpid(pid, &mut wstatus, 0);
        }
        return Err(err);
    }
    // Close our copy of the write end so the read sees EOF once the only
    // remaining write end (the child's) is gone (exec-closed or after the child
    // exits). Then read the status: 0 bytes (EOF) = success; any byte = the child
    // failed BEFORE exec, so there is no confined shell to hand back.
    // SAFETY: `status_wr` is the parent's copy of the write end.
    unsafe {
        libc::close(status_wr);
    }
    let mut indicator = [0u8; 1];
    // EINTR-retrying read of the single status byte (or EOF).
    let n = loop {
        // SAFETY: `status_rd` is a valid read fd; `indicator` is a 1-byte buffer.
        let r = unsafe { libc::read(status_rd, indicator.as_mut_ptr().cast::<libc::c_void>(), 1) };
        if r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        break r;
    };
    // SAFETY: done with the read end.
    unsafe {
        libc::close(status_rd);
    }
    if n > 0 {
        // Child reported a pre-exec failure. Close the master (no unconfined
        // shell escapes) and reap the child so it is not left as a zombie.
        // SAFETY: `master` is the parent's forkpty master fd.
        unsafe {
            libc::close(master);
            let mut wstatus: libc::c_int = 0;
            libc::waitpid(pid, &mut wstatus, 0);
        }
        let (kind, what) = match indicator[0] {
            b'S' => (
                io::ErrorKind::PermissionDenied,
                "sandbox confinement failed in child (fail-closed: shell not exec'd, _exit(126))",
            ),
            _ => (
                io::ErrorKind::Other,
                "child failed to exec the shell before exec (_exit(127))",
            ),
        };
        return Err(io::Error::new(kind, what));
    }
    Ok(SpawnedShell { master, pid })
}

/// HANG UP a spawned shell's controlling-tty session by sending `SIGHUP` to the
/// child's process group (`pid` from [`SpawnedShell`] is its session-leader pid
/// == pgid). The child — and its jobs — receive SIGHUP and exit; the PTY slave
/// then closes, so a reader thread blocked in `read(master)` gets EOF and ends on
/// its own. This is the NON-BLOCKING half of teardown: it never touches the tty
/// lock the way `close(master)` does, so the caller can run it from the UI thread
/// and only close the master afterwards (off-thread / at process exit). A no-op
/// for a non-positive pid (a pgid of <= 1 would target init / every process — we
/// refuse it). Best-effort: a child that already exited makes `killpg` fail
/// harmlessly (ESRCH), which is fine — the reader still sees EOF.
pub fn hangup(pid: i32) {
    if pid <= 1 {
        return;
    }
    // SAFETY: `killpg` with a signal merely posts SIGHUP to the process group;
    // `pid` is the session-leader pid we got from `forkpty`, so the group is the
    // child's own job tree. Any error (already-reaped child) is ignored.
    unsafe {
        libc::killpg(pid, libc::SIGHUP);
    }
}

/// Reap an exited child WITHOUT ever blocking unboundedly. Runs on the detached
/// teardown thread AFTER [`hangup`] (the UI thread has already moved on). A
/// well-behaved child exits on SIGHUP within milliseconds and is reaped on the first
/// poll. The hazard this guards against: a child that TRAPS or ignores SIGHUP (e.g.
/// `trap '' HUP`, or one wedged in uninterruptible D-state) would leave a plain
/// blocking `waitpid(…, 0)` parked here FOREVER — one leaked thread (and the
/// fd/process slot it pins) per such mid-run close. So poll `WNOHANG`: escalate to an
/// unignorable SIGKILL after a short grace, and after a hard deadline give up and
/// return, leaving the kernel to reap the orphan at process exit. Keeps the child
/// from lingering as a zombie in the common case. Best-effort; a no-op for a
/// non-positive pid.
///
/// NOT-OUR-CHILD SHELLS (the overlap handoff): a seamlessly ADOPTED session's
/// shell is a child of the PREVIOUS aterm process — after the overlap swap it
/// reparents to launchd, so `waitpid` here answers `ECHILD` on the FIRST tick.
/// Treating that as "already gone" (the old behaviour) skipped the SIGKILL
/// escalation entirely: a HUP-trapping shell closed via Cmd-W after an update
/// was never force-killed. `ECHILD` now switches to a signal-0 liveness poll
/// with the SAME grace/escalation schedule (launchd reaps the corpse, so no
/// zombie risk on this path — only the escalation matters).
pub fn reap(pid: i32) {
    if pid <= 1 {
        return;
    }
    // ~2 s budget: poll every 10 ms (200 ticks). A SIGHUP-ignoring holdout is
    // SIGKILLed at ~250 ms, so the common case returns on the first poll and the
    // pathological case is still bounded.
    const POLL: std::time::Duration = std::time::Duration::from_millis(10);
    const KILL_AT: u32 = 25;
    const DEADLINE: u32 = 200;
    let mut status: libc::c_int = 0;
    for tick in 0..DEADLINE {
        // SAFETY: `WNOHANG` `waitpid`; `&mut status` is a valid out-param.
        // Returns the pid when reaped, 0 if still running as our child, -1
        // (`ECHILD`) when it is not our child — either already reaped, or an
        // ADOPTED shell parented to launchd.
        let r = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if r == pid {
            return; // reaped
        }
        let mut not_ours = false;
        if r == -1 {
            not_ours = true;
            // Not our child. IDENTITY CHECK before anything else: an adopted
            // shell is a SESSION LEADER (forkpty = setsid), so its pgid == its
            // pid. A recycled pid (the shell died and the kernel reissued the
            // number) is overwhelmingly NOT a fresh group leader — treating
            // `getpgid` mismatch (or `ESRCH`) as "gone" makes the escalation
            // below unable to SIGKILL an innocent bystander's process group.
            // SAFETY: getpgid is a read-only probe.
            if unsafe { libc::getpgid(pid) } != pid {
                return; // dead (ESRCH) or a recycled non-leader pid
            }
        }
        if tick == KILL_AT {
            // Still alive past the grace ⇒ it ignored SIGHUP. SIGKILL the group.
            // Re-verify the not-our-child identity IMMEDIATELY before the kill
            // (the per-tick probe above may be up to one tick stale).
            // SAFETY: read-only probe + best-effort signal post to the child's
            // own process group.
            if !not_ours || unsafe { libc::getpgid(pid) } == pid {
                unsafe {
                    libc::killpg(pid, libc::SIGKILL);
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

/// Build the `sandbox-exec`-wrapped `(exec_target, argv)` for an OS-sandboxed
/// spawn, FAILING CLOSED if the wrapper at `wrapper_path` is missing/not
/// executable. Pure (its only side effect is the `access(X_OK)` probe of
/// `wrapper_path`), so the fail-closed and argv-shape behavior is unit-testable
/// without forking.
///
/// On success the returned exec target is `wrapper_path` and the argv is:
///   ["sandbox-exec", "-p", <sbpl>, <program-path>, <orig argv[1..]>]
/// i.e. the original argv with argv[0] replaced by the resolved program PATH
/// (`prog`), because `sandbox-exec` execs its first positional and sets that path
/// as the child's argv[0]. Every real argument after argv[0] is preserved; only a
/// cosmetic login-dash argv[0] on a bare shell is dropped (documented on
/// [`spawn_shell`]).
///
/// # Errors
/// `NotFound` if `wrapper_path` is missing/not executable (fail-closed — the
/// caller must NOT spawn unsandboxed); `Other`/`InvalidInput` if `wrapper_path`
/// or `sbpl` cannot be turned into a C string (interior NUL).
fn build_sandbox_wrap(
    wrapper_path: &str,
    sbpl: &str,
    prog: &CString,
    orig_argv: &[CString],
) -> io::Result<(CString, Vec<CString>)> {
    let wrapper = CString::new(wrapper_path.as_bytes())
        .map_err(|_| io::Error::other("sandbox-exec path not representable"))?;
    // `access(X_OK)` in the PARENT (the child does no PATH search). A missing
    // wrapper means the policy-demanded sandbox cannot be applied → refuse.
    // SAFETY: `wrapper` is a valid NUL-terminated absolute path.
    let present = unsafe { libc::access(wrapper.as_ptr(), libc::X_OK) } == 0;
    if !present {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "OS sandbox demanded but {wrapper_path} is missing/not executable — refusing \
                 to spawn an unsandboxed shell (fail-closed, ATERM_DESIGN §5.6)"
            ),
        ));
    }
    let sbpl_c = CString::new(sbpl.as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "SBPL profile has interior NUL")
    })?;
    // saturating_add: a real argv cannot approach usize::MAX; the capacity
    // is a hint either way. Identical on every real input.
    // Clamp the pre-size HINT (advisory; the Vec grows on demand) so the bulk
    // allocation carries a provable bound for the L0 gate. A real argv is far
    // below the cap — identical contents for every input.
    let mut wrapped: Vec<CString> = Vec::with_capacity(orig_argv.len().saturating_add(3).min(4096));
    wrapped.push(CString::new("sandbox-exec").unwrap_or_else(|_| wrapper.clone()));
    // `CStr` literal (not `CString::new(..).unwrap()`): the unwrap's panic
    // lives in an absent std body and the literal is NUL-terminated at compile
    // time — same bytes, no panic path.
    wrapped.push(CString::from(c"-p"));
    wrapped.push(sbpl_c);
    wrapped.push(prog.clone());
    wrapped.extend(orig_argv.iter().skip(1).cloned());
    Ok((wrapper, wrapped))
}

/// PATH-resolve a `-e` program name to an absolute path, IN THE PARENT (the child
/// must stay async-signal-safe, so it cannot do its own `execvp` PATH search). A
/// name containing `/` is used verbatim (an explicit path). Otherwise each `$PATH`
/// entry is probed for an executable regular file. Falls back to the name verbatim
/// when nothing matches, so `execve` fails cleanly (`_exit(127)`) instead of this
/// resolver masking a not-found command.
// Skip: `CString::new` panics only on allocation (interior NUL returns
// Err) — the audited-alloc class; the argv strings are bounded by the
// config. Droppable when the CString totality entry lands.
#[cfg_attr(trust_verify, trust::skip)]
fn resolve_program(name: &str) -> CString {
    // The interior-NUL fallback is a compile-time `c"..."` literal (NUL-free by
    // construction), not a runtime `CString::new(..).unwrap()`: same bytes, no
    // panic path — behavior-identical while discharging the Trust unwrap
    // panic-boundary obligations.
    let verbatim = || CString::new(name.as_bytes()).unwrap_or_else(|_| c"/nonexistent".to_owned());
    if name.is_empty() || name.contains('/') {
        return verbatim();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate = dir.join(name);
            if let Ok(c) = CString::new(candidate.as_os_str().as_bytes()) {
                // Executable (X_OK) AND a regular file — something we can exec.
                // SAFETY: `c` is a valid NUL-terminated path string.
                let executable = unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0;
                if executable && candidate.is_file() {
                    return c;
                }
            }
        }
    }
    verbatim()
}

/// The decision a single `write(2)` return drives in the `write_all` drain loop.
/// Extracted as a pure value so the EINTR-retry / short-write / peer-closed branch
/// logic is unit-testable WITHOUT provoking a real (timing-dependent) `EINTR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStep {
    /// A signal interrupted the write before any byte moved (`EINTR`): retry.
    Retry,
    /// A real error, or the peer closed (`r == 0`): stop draining.
    Stop,
    /// `n` bytes were written: advance the slice cursor by `n` and continue.
    Advance(usize),
}

/// Classify a `write(2)` result for the `write_all` loop. `r` is the raw return;
/// `is_eintr` is whether `errno` was `EINTR` (only consulted when `r < 0`, exactly
/// as the loop does — the caller reads `errno` only on the error branch). Pure: no
/// syscalls, no `errno` read of its own, so it can be tested with synthetic inputs.
///
/// This is a behavior-preserving extraction of the original inline branch ladder;
/// the runtime decisions are byte-identical:
///   r < 0 && EINTR      -> Retry
///   r < 0 && other      -> Stop
///   r == 0 (peer closed) -> Stop
///   r > 0               -> Advance(r)
fn classify_write_result(r: isize, is_eintr: bool) -> WriteStep {
    if r < 0 {
        if is_eintr {
            WriteStep::Retry
        } else {
            WriteStep::Stop
        }
    } else if r == 0 {
        WriteStep::Stop
    } else {
        WriteStep::Advance(r as usize)
    }
}

/// Write all of `bytes` to the PTY master, retrying short writes AND `EINTR`
/// (a signal interrupting the write must not silently drop the rest of the
/// buffer — that would lose terminal input). Stops only on a real error or a
/// zero/negative non-`EINTR` return (peer closed).
///
/// SPEC: this drain loop is the real implementation of the external `WriteAll.tla`
/// model (TRUST_NATIVE_TLA Phase 2, I/O DURABILITY family). Its `off` cursor
/// (`data = &data[n..]`) is monotone non-decreasing and only the FULL-buffer exit
/// (`data.is_empty()`) reports completion — exactly the spec's `NoSilentDrop`
/// (`done ⇒ off = Size`): a short write or `EINTR` ([`WriteStep::Advance`] with a
/// partial `n`, or [`WriteStep::Retry`]) keeps looping rather than claiming success
/// with a dropped tail. A progress write that reaches the end is the spec's
/// `Progress`; a short/EINTR step that does NOT finish is `Interrupted`. Tier-1
/// conformance drives this real loop over a slow reader and validates the `<<off,
/// done>>` trajectory against `WriteAll.tla` (`tests/conformance_writeall.rs`).
// PROJECTION (TRUST_VACUITY_GATE §2.2 / finding 2): both drain-loop actions project the
// real `write_all` cursor onto the spec's `<<off, done>>` — `off = bytes.len() -
// data.len()` (the monotone consumed prefix), `done = data.is_empty()`. The witness is
// `aterm_pty::write_all_project`; the L2 obligation requires the projection NAME be
// present (Trust does not execute it — that is the aterm-side conformance binding).
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "write_all",
        action = "Progress",
        project = "aterm_pty::write_all_project"
    )
)]
#[cfg_attr(
    any(test, feature = "spec-anchors"),
    aterm_spec::refines(
        machine = "write_all",
        action = "Interrupted",
        project = "aterm_pty::write_all_project"
    )
)]
pub fn write_all(master: i32, bytes: &[u8]) {
    let mut data = bytes;
    while !data.is_empty() {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `data` is a
        // valid slice of `data.len()` bytes.
        let r = unsafe { libc::write(master, data.as_ptr() as *const libc::c_void, data.len()) };
        // `errno` is only meaningful when `r < 0`; mirror the original loop, which
        // read `last_os_error()` solely on the error branch.
        let is_eintr = r < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted;
        match classify_write_result(r, is_eintr) {
            WriteStep::Retry => continue,
            WriteStep::Stop => break,
            // `Advance(n)` carries `n = r as usize` with `r > 0`, and `write(2)`
            // never reports more bytes written than the `data.len()` it was asked
            // for, so `n <= data.len()` and the range is always valid: the
            // `unwrap_or(&[])` fallback never fires — behavior-identical while
            // discharging the Trust slice-bounds obligation.
            WriteStep::Advance(n) => data = data.get(n..).unwrap_or(&[]),
        }
    }
}

/// Write `bytes` to the PTY master with a single `write(2)` (retrying only `EINTR`),
/// returning the number of bytes the kernel ACCEPTED (which may be fewer than
/// `bytes.len()`). A true count is what lets a sink apply real end-to-end
/// backpressure instead of silently dropping the tail — the routing-fabric
/// `SinkWriter` (`aterm-session`) is built on this.
///
/// Errors: [`io::ErrorKind::WouldBlock`] when `master` is non-blocking (see
/// [`set_nonblocking`]) and the input buffer is full; other errors when the slave is
/// gone. A `0` return (peer closed mid-write) is reported as `Ok(0)`.
// #[inline] so the MIR crosses the crate boundary: callers' Trust gates
// (aterm-session's drain loop) bundle and VERIFY this body instead of
// assuming an absent callee. Semantics unchanged.
#[inline]
pub fn write_some(master: i32, bytes: &[u8]) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    loop {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `bytes` is a valid
        // slice of `bytes.len()` bytes.
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(r as usize);
    }
}

/// Like [`write_some`], but reports ONLY the byte count the kernel accepted and
/// NEVER yields an [`io::Error`]: every error collapses to `0`, `EINTR` is
/// retried internally. Built for the sink drain loop (`aterm-session`): returning
/// `usize` keeps any `io::Error` from crossing the call boundary into the loop,
/// so the loop has no opaque error value to drop — eliminating the `io::Error`
/// drop-glue gap that forced the loop's old skip'd `discard_write_error` helper.
/// Mapping vs [`write_some`]: `Ok(n) -> n`, `Ok(0) -> 0`, `Err(_) -> 0` —
/// behavior-identical for the drainer, which treats every non-positive result as
/// session-dead.
///
/// INVARIANT (load-bearing for `drain_loop`'s panic-freedom proof): the ONLY
/// `io::Error` this body may ever construct is `io::Error::last_os_error()` (the
/// `Os(errno)` variant), and it is born AND dropped inside this body. That drop
/// is trivially total — no boxed `Custom` payload, so no user `Drop` runs. Do NOT
/// introduce `io::Error::new`/`io::Error::other`/a `?`-propagated error here: a
/// `Custom` variant runs arbitrary user `Drop` (may panic) and would silently
/// regress the drain loop from PROVED back to a coverage gap.
// #[inline] so the MIR crosses the crate boundary (the `write_some`/`dup_fd`
// precedent): aterm-session's drain loop bundles and VERIFIES this body rather
// than assuming an absent callee.
#[inline]
pub fn write_some_count(master: i32, bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    loop {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `bytes` is a valid
        // slice of `bytes.len()` bytes (identical to `write_some`).
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            // `last_os_error()` is the `Os(errno)` variant, constructed AND dropped
            // right here — no boxed `Custom` payload, so the drop is total. Only
            // `EINTR` retries; every other error collapses to `0` (session-dead).
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return 0;
        }
        return r as usize;
    }
}

/// [`write_some`] with BLOCKING semantics restored on an `O_NONBLOCK` master: on
/// `WouldBlock` it parks in `poll(POLLOUT)` and retries. The direct-read gather
/// flips the master's file description non-blocking (`O_NONBLOCK` is
/// per-DESCRIPTION, so a `dup` cannot isolate it) — every writer that previously
/// relied on the kernel parking a write into a full tty input queue (reply
/// writer, paste, control verbs, the spill drainer) keeps that exact behavior by
/// going through this instead of a bare [`write_some`]. `POLLERR`/`POLLHUP`
/// report as writable inside [`poll_writable`] ON PURPOSE: the retried write then
/// surfaces the real error. `EINTR` retries (in both the write and the poll);
/// other errors propagate.
// #[inline] so the MIR crosses the crate boundary (the `write_some` precedent):
// aterm-session's frame writers bundle and VERIFY this body. Its only error
// values are `Os(errno)` (born in `write_some`/`poll_writable`, returned — never
// dropped here); no panic path.
#[inline]
pub fn write_some_blocking(master: i32, bytes: &[u8]) -> io::Result<usize> {
    loop {
        match write_some(master, bytes) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Park until the tty input queue has room (or the peer errors —
                // reported writable, so the retry surfaces it).
                poll_writable(master, -1)?;
            }
            other => return other,
        }
    }
}

/// [`write_some_count`] with BLOCKING semantics restored on an `O_NONBLOCK`
/// master (the [`write_some_blocking`] twin for the sink's spill drain loop):
/// `EAGAIN` parks in `poll(POLLOUT)` and retries instead of collapsing to `0` —
/// without this the drainer would misread a full-but-alive tty input queue (the
/// wedged foreground it EXISTS to absorb) as session-dead and drop the spill.
/// All other errors still collapse to `0`; `EINTR` retries internally.
///
/// INVARIANT (the `write_some_count` drop discipline): every `io::Error` touched
/// here is the `Os(errno)` variant, born in this body or in [`poll_writable`],
/// and dropped HERE — a trivially total drop with no boxed `Custom` payload.
// #[inline] so the MIR crosses the crate boundary (the `write_some_count`
// precedent): aterm-session's drain loop bundles and VERIFIES this body.
#[inline]
pub fn write_some_count_blocking(master: i32, bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    loop {
        // SAFETY: `master` is a PTY master fd from `spawn_shell`; `bytes` is a valid
        // slice of `bytes.len()` bytes (identical to `write_some`).
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r >= 0 {
            return r as usize;
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                // Full-but-alive input queue: park for room, then retry. A broken
                // poll collapses to 0 (session-dead), like any other hard error.
                if poll_writable(master, -1).is_err() {
                    return 0;
                }
            }
            _ => return 0,
        }
    }
}

/// Classification of a single non-parking `write(2)`, for
/// [`SinkWriter::write_frame_nonparking`]. Distinguishes `WouldBlock` (an O_NONBLOCK
/// race) from a fatal error WITHOUT the CALLER ever holding — hence dropping — an
/// `io::Error`. The only `io::Error` constructed is the `Os(errno)` from
/// `last_os_error`, born inside [`write_some_nonparking`] and dropped THERE on the
/// EINTR / would-block branches (trivially total — no boxed `Custom` payload), or moved
/// into `Fatal` for the caller to RETURN. This keeps the caller's non-parking write loop
/// free of any opaque `io::Error` drop (the `write_some_count` idiom, generalized to a
/// path that must still surface `WouldBlock` and the real error).
#[cfg(unix)]
#[derive(Debug)]
pub enum NonParkWrite {
    /// `n > 0` bytes accepted; advance the cursor by `n`.
    Wrote(usize),
    /// Peer closed mid-write (`r == 0`) or nothing to write: stop.
    Closed,
    /// The fd would block (`EAGAIN`/`EWOULDBLOCK`): spill the tail.
    WouldBlock,
    /// A real error (the `Os(errno)` variant): the caller RETURNS it, never drops it.
    Fatal(io::Error),
}

/// One non-parking `write(2)`, classified as [`NonParkWrite`] — behavior-identical to
/// the prior inline `match write_some(..) { Ok(0)=>.., Ok(n)=>.., Err(WouldBlock)=>..,
/// Err(e)=>Err(e) }` ladder, but the `WouldBlock` decision no longer forces the caller
/// to drop an `io::Error`. `EINTR` retries internally.
// #[inline] so the MIR crosses the crate boundary (the `write_some` precedent):
// aterm-session's `write_frame_nonparking` bundles and VERIFIES this body.
#[cfg(unix)]
#[inline]
pub fn write_some_nonparking(master: i32, bytes: &[u8]) -> NonParkWrite {
    if bytes.is_empty() {
        return NonParkWrite::Closed;
    }
    loop {
        // SAFETY: identical to `write_some` — `master` is a PTY master fd; `bytes` is a
        // valid slice of `bytes.len()` bytes.
        let r = unsafe { libc::write(master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if r < 0 {
            // `last_os_error()` is the `Os(errno)` variant, born here and dropped here on
            // the EINTR / would-block branches (total drop, no `Custom` payload) or moved
            // into `Fatal` below.
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return NonParkWrite::WouldBlock,
                _ => return NonParkWrite::Fatal(e),
            }
        }
        if r == 0 {
            return NonParkWrite::Closed;
        }
        return NonParkWrite::Wrote(r as usize);
    }
}

/// Poll the PTY master for writability, blocking up to `timeout_ms` (`0` =
/// check-and-return). `POLLERR`/`POLLHUP`/`POLLNVAL` report as "writable" ON PURPOSE:
/// the subsequent write then surfaces the real error to its caller instead of the
/// condition being silently classified as "not ready" and the bytes spilled forever.
/// `EINTR` retries; other `poll` errors propagate. The basis of the sink's
/// non-parking keystroke egress (`aterm-session`): a UI-thread writer checks this
/// before committing to a `write(2)` that a wedged foreground program (full tty input
/// buffer) would otherwise park the whole event loop on.
// #[inline] so the MIR crosses the crate boundary (the `write_some`/`dup_fd`
// precedent): aterm-session's non-parking write path (`write_frame_nonparking`)
// bundles and VERIFIES this body rather than assuming an absent callee. Its only
// error value is the `Os(errno)` `io::Error` (born+dropped on the EINTR branch,
// discharged by the Os-variant drop audit) or returned; no panic path.
#[inline]
pub fn poll_writable(fd: i32, timeout_ms: i32) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        // SAFETY: `p` is a valid pollfd for the duration of the call; nfds == 1.
        let r = unsafe { libc::poll(&mut p, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(r > 0);
    }
}

/// `dup(2)` an fd into an [`std::os::fd::OwnedFd`] the caller owns outright — same
/// open file description (shared offset/flags), independent lifetime. Lets a detached
/// helper thread (the sink's spill drainer) keep writing safely without pinning the
/// original owner: even if the original fd number is closed and recycled, the dup
/// still names the PTY, so no write can land on a stranger's fd.
// #[inline] so the MIR crosses the crate boundary (write_some precedent):
// aterm-session's spill path bundles and VERIFIES this body.
#[inline]
pub fn dup_fd(fd: i32) -> io::Result<std::os::fd::OwnedFd> {
    // SAFETY: `dup` on a caller-supplied fd; a negative return is checked before the
    // from_raw_fd, so ownership is only assumed for a real, freshly-dup'd fd.
    let d = unsafe { libc::dup(fd) };
    if d < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `d` is a fresh fd we exclusively own (just created by dup above).
    Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(d) })
}

/// Toggle `O_NONBLOCK` on the PTY master so [`write_some`] returns
/// [`io::ErrorKind::WouldBlock`] (instead of blocking) when the input buffer is full
/// — the basis for per-edge backpressure in the routing fabric. Idempotent; reads
/// the current flags first so it never clobbers unrelated `fcntl` state.
pub fn set_nonblocking(master: i32, nonblocking: bool) -> io::Result<()> {
    // SAFETY: `master` is a valid fd; `F_GETFL`/`F_SETFL` only read/modify flags.
    let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if next == flags {
        return Ok(());
    }
    // SAFETY: `master` is a valid fd; `F_SETFL` sets the flags word we derived.
    let rc = unsafe { libc::fcntl(master, libc::F_SETFL, next) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Toggle `FD_CLOEXEC` on `fd` — the DESCRIPTOR flag (`F_GETFD`/`F_SETFD`), distinct
/// from the STATUS flags [`set_nonblocking`] touches. The parent marks every PTY master
/// `FD_CLOEXEC` right after `forkpty` (so a later session's child can't inherit a prior
/// master — the cross-session isolation gate). Proof-carrying DSU's seamless re-exec
/// (RFC Rung 1b) must CLEAR it (`on = false`) on each master it hands to the new binary,
/// so the master SURVIVES `execve` and the running shell is not killed. Idempotent;
/// reads the current flags first so it never clobbers unrelated `fcntl` state.
///
/// SAFETY-CRITICAL: only clear `FD_CLOEXEC` on a master you are ABOUT to hand off and
/// then re-adopt (or close) in the new process — a cleared master that is neither
/// re-adopted nor closed leaks a live, ungated PTY channel across the exec.
pub fn set_cloexec(fd: i32, on: bool) -> io::Result<()> {
    // SAFETY: `fd` is a valid fd; `F_GETFD` only reads the descriptor flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let next = if on {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if next == flags {
        return Ok(());
    }
    // SAFETY: `fd` is a valid fd; `F_SETFD` sets the descriptor-flags word we derived.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, next) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Whether `fd` is a tty (`TIOCGWINSZ` succeeds). A SANITY BACKSTOP for the seamless
/// re-exec's INCOMING side — NOT the authenticity gate. It rejects the obviously-wrong
/// (a pipe, a socket, a regular file, a closed/bad fd), but it does NOT prove `fd` is a
/// PTY MASTER: a pty SLAVE and even inherited `0`/`1`/`2` when aterm is launched from a
/// terminal are ttys and pass. The REAL fail-closed defense against a spoofed
/// `ATERM_SEAMLESS_FDS` is the single-use nonce stamp in the `0700` dir (only WE can
/// mint it) plus adopting ONLY the exact fd numbers listed in the authenticated
/// handoff map WE wrote — never "any fd that looks like a tty". This check is the
/// last-line "and it's still a tty" assertion after that authentication.
#[must_use]
pub fn fd_is_tty(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    // SAFETY: `libc::winsize` is a plain struct of integer fields (rows/cols/pixels);
    // an all-zeros bit pattern is a valid, fully-initialized value.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` may be any integer; `ioctl` fails cleanly (rc<0) on a bad/non-tty fd
    // and `ws` is a valid winsize out-param on success.
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    rc == 0
}

/// Read up to `buf.len()` bytes from the PTY master into `buf`. Returns the number
/// of bytes read (`0` = EOF, `< 0` = error, per `read(2)`).
pub fn read(master: i32, buf: &mut [u8]) -> isize {
    // SAFETY: `master` is a valid fd; `buf` is a valid mutable slice of
    // `buf.len()` bytes.
    unsafe { libc::read(master, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) }
}

/// A wake pipe for CANCELLING a parked PTY reader: `(read_end, write_end)`. Both are
/// close-on-exec (they must NOT survive a seamless re-exec) and both ends are
/// non-blocking. The write end must be non-blocking because [`wake`] runs on the
/// event loop during handoff parking; a redundant wake against a full pipe may
/// never turn into input latency. Returns `None` on failure — the caller then degrades to a plain
/// un-cancellable [`read`], no worse than before. The owner unblocks a reader parked
/// in [`read_or_wake`] by writing the write end via [`wake`] (MEM-L2).
pub fn make_wake_pipe() -> Option<(i32, i32)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is a valid 2-int out-array for pipe(2).
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    let (rd, wr) = (fds[0], fds[1]);
    // SAFETY: rd/wr are freshly-created pipe fds; these fcntls only read/set flags.
    let configured = unsafe {
        let rd_flags = libc::fcntl(rd, libc::F_GETFL);
        let wr_flags = libc::fcntl(wr, libc::F_GETFL);
        libc::fcntl(rd, libc::F_SETFD, libc::FD_CLOEXEC) != -1
            && libc::fcntl(wr, libc::F_SETFD, libc::FD_CLOEXEC) != -1
            && rd_flags >= 0
            && wr_flags >= 0
            && libc::fcntl(rd, libc::F_SETFL, rd_flags | libc::O_NONBLOCK) != -1
            && libc::fcntl(wr, libc::F_SETFL, wr_flags | libc::O_NONBLOCK) != -1
    };
    if !configured {
        close_fd(rd);
        close_fd(wr);
        return None;
    }
    Some((rd, wr))
}

/// Outcome of [`read_or_wake`].
pub enum ReadOutcome {
    /// `n` bytes (`n > 0`) were read into the caller's buffer.
    Data(usize),
    /// The master reached EOF / error — the child's slave closed (the shell exited).
    Eof,
    /// The wake pipe fired: the owning `Session` dropped and asked the reader to stop
    /// (tab/pane close) even though the master itself has NOT EOF'd — an orphaned child
    /// in another pgroup may still hold the slave open. The reader must exit and drop
    /// its refs so the master fd + Terminal + writer threads are released (MEM-L2).
    Wake,
    /// The wake-pipe-LESS fallback (`wake_rd < 0`, because `make_wake_pipe` failed under
    /// fd exhaustion) polled the master with a bounded timeout and it elapsed with no
    /// master activity. The caller must re-check its own (fd-free) stop flag and, if the
    /// session is still live, call again. This is what lets a reader that never got a
    /// wake pipe still be cancelled — instead of parking forever on a master an orphaned
    /// different-pgroup child keeps open, which would silently re-open the MEM-L2 leak
    /// exactly under the heavy-fd load that broke pipe creation. Never produced on the
    /// wake-pipe path (which blocks indefinitely with zero polling).
    Idle,
}

/// Timeout (ms) for the wake-pipe-LESS fallback poll in [`read_or_wake`]. ONLY the
/// degraded path (no wake pipe — `make_wake_pipe` failed under fd exhaustion) wakes on
/// this cadence; the normal wake-pipe path blocks indefinitely with zero polling. Small
/// enough that a dropped-but-orphan-pinned session is reclaimed promptly, large enough
/// that an otherwise-idle degraded reader costs only ~4 wakeups/sec.
const WAKELESS_POLL_TIMEOUT_MS: libc::c_int = 250;

/// Block until EITHER the master is readable (or EOF) OR `wake_rd` fires, then act.
/// Unlike a bare `read(master)`, this returns [`ReadOutcome::Wake`] when the owning
/// `Session` drops and writes its wake pipe — so a reader parked on a master that an
/// orphaned, different-pgroup child keeps open (setsid / double-fork / `disown`) is no
/// longer pinned forever.
///
/// `wake_rd < 0` (no wake pipe could be made — fd exhaustion) does NOT degrade to a bare
/// blocking `read`: that would re-pin the reader forever on an orphan-held master and
/// silently re-open MEM-L2 precisely under the heavy-fd load that broke pipe creation.
/// Instead it polls the master ALONE (allocating no new fd) with a bounded timeout and
/// returns [`ReadOutcome::Idle`] on expiry, so the caller can honor an fd-free stop flag.
pub fn read_or_wake(master: i32, buf: &mut [u8], wake_rd: i32) -> ReadOutcome {
    if wake_rd < 0 {
        let mut fds = [libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `fds` is a valid 1-element pollfd array for the duration of the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 1, WAKELESS_POLL_TIMEOUT_MS) };
        if r < 0 {
            let e = io::Error::last_os_error();
            // EINTR → hand control back so the caller re-checks its stop flag and retries;
            // any other poll error tears the reader down rather than letting it spin.
            return if e.kind() == io::ErrorKind::Interrupted {
                ReadOutcome::Idle
            } else {
                ReadOutcome::Eof
            };
        }
        if r == 0 {
            return ReadOutcome::Idle; // timed out, master quiet — caller re-checks stop
        }
        // POLLHUP/ERR/NVAL fall through to `read`, which surfaces the 0/-1 as Eof.
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
            // O_NONBLOCK master (the direct-read gather): a read after POLLIN can
            // still EAGAIN spuriously — that is Idle, NOT Eof (an Eof here would
            // tear the session down on a kernel wakeup race). EINTR likewise.
            if n < 0
                && matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                )
            {
                return ReadOutcome::Idle;
            }
            return ReadOutcome::Eof;
        }
        return ReadOutcome::Idle; // spurious wakeup — caller re-checks stop and retries
    }
    let mut fds = [
        libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_rd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a valid 2-element pollfd array for the duration of the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // A broken poll tears the reader down rather than letting it spin.
            return ReadOutcome::Eof;
        }
        // Wake takes priority — the owner wants us gone regardless of pending output.
        if fds[1].revents != 0 {
            return ReadOutcome::Wake;
        }
        // POLLHUP/ERR/NVAL fall through to `read`, which surfaces the 0/-1 as Eof.
        let ready = libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
        if fds[0].revents & ready != 0 {
            let n = read(master, buf);
            if n > 0 {
                return ReadOutcome::Data(n as usize);
            }
            // O_NONBLOCK master (the direct-read gather): a read after POLLIN can
            // still EAGAIN spuriously — poll again rather than reporting a false
            // Eof that would tear the session down. EINTR likewise.
            if n < 0
                && matches!(
                    io::Error::last_os_error().kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                )
            {
                continue;
            }
            return ReadOutcome::Eof;
        }
        // Spurious wakeup with no revents — poll again.
    }
}

/// Top up `buf` past an initial [`read_or_wake`] read. macOS caps every master
/// `read` at ~1 KiB (the xnu tty output-queue high-water mark), so a
/// poll→read→process loop degenerates to lockstep with the writer at ~1 KiB per
/// cycle — every downstream cost (term-lock, taps, wake) is paid per KiB and the
/// drain ceils far below what the kernel can move. Gathering many kernel chunks
/// into one batch amortizes all of it. Two regimes:
///   * `filled` below one outq (~1 KiB) ⇒ interactive output: return on the FIRST
///     quiet poll, so a lone keystroke echo pays one extra zero-timeout poll and
///     nothing else;
///   * `filled` at/above one outq ⇒ the writer is saturating the queue: bridge its
///     microsecond refill gaps with up to [`DRAIN_SPIN_MAX`] short spins instead of
///     sleeping — a sleeping drain hands the whole gap back to the scheduler.
///
/// EOF/errors mid-drain just stop the top-up: the gathered bytes get processed and
/// the NEXT `read_or_wake` surfaces the Eof.
pub fn drain_more(master: i32, buf: &mut [u8], mut filled: usize) -> usize {
    /// One kernel tty output queue's worth — at/above this the writer saturated it.
    const SATURATED: usize = 1024;
    /// Spin-retry budget bridging the writer's refill gaps (~0.5–1 µs each).
    const DRAIN_SPIN_MAX: u32 = 16;
    let mut spins = 0u32;
    while filled < buf.len() {
        let mut fds = [libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `fds` is a valid 1-element pollfd array for the duration of the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) };
        if r > 0 && fds[0].revents & libc::POLLIN != 0 {
            let n = read(master, &mut buf[filled..]);
            if n <= 0 {
                break; // EOF/error: keep what we have; the next outer poll reports it
            }
            filled += n as usize;
            spins = 0;
        } else {
            // Quiet (or POLLHUP/EINTR — the outer read_or_wake handles those).
            if filled < SATURATED || spins >= DRAIN_SPIN_MAX {
                break;
            }
            spins += 1;
            for _ in 0..64 {
                std::hint::spin_loop();
            }
        }
    }
    filled
}

/// Revert knob (`ATERM_PTY_BRIDGE_SPIN=1`): the legacy paced-probe bridge
/// (256 `spin_loop` pauses between re-reads, deliver once the probe budget is
/// spent). Kept because the bridge shape is a measured-perf hot spot; measured
/// on M5 Max, the pause block costs ~2 µs (`spin_loop` lowers to `ISB` on
/// Apple Silicon), 4× its design assumption — the reason the legacy bridge
/// lost ~100 MB/s of drain rate.
fn bridge_legacy_spin() -> bool {
    static LEGACY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEGACY.get_or_init(|| std::env::var_os("ATERM_PTY_BRIDGE_SPIN").is_some_and(|v| v == "1"))
}

/// Default idle-refill patience. Measured sweep (quiet M5 Max, 500 MB flood +
/// CPR round-trip probe): 0 ⇒ 231 MB/s / +0 µs; 50 ⇒ 266 / +79 µs p50;
/// 250 ⇒ 265 / +327 µs; 1000 ⇒ 254 / +1.27 ms (the bb0ac4c fps-regression
/// class). 50 µs recovers the full cutoff-churn loss at an invisible query
/// tax — and IMPROVES p99 (679 vs 1385 µs: fewer, fuller deliveries).
const IDLE_POLL_DEFAULT_US: u32 = 50;

/// Idle-cutoff hysteresis (`ATERM_PTY_IDLE_POLL_US=<µs>`): how long a dry,
/// parser-idle gap may wait for a refill before delivering. An immediate
/// cutoff (0) protects the fps/request-response class (the ghostty bb0ac4c
/// regression) but its small-batch churn costs ~33-43 MB/s of cat-flood
/// throughput; [`IDLE_POLL_DEFAULT_US`] is the measured sweet spot. `0`
/// restores the immediate cutoff. Read once; unparsable values mean the
/// default. Clamped to [`IDLE_POLL_CLAMP_US`]: a wait beyond the batch
/// budget is meaningless, and on the wake-pipe-less fallback path an
/// unbounded select would stall teardown for the full value.
fn idle_poll_us() -> u32 {
    /// Upper clamp — just past [`BATCH_BUDGET`]'s 3 ms.
    const IDLE_POLL_CLAMP_US: u32 = 5_000;
    static IDLE_POLL_US: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *IDLE_POLL_US.get_or_init(|| {
        std::env::var("ATERM_PTY_IDLE_POLL_US")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(IDLE_POLL_DEFAULT_US)
            .min(IDLE_POLL_CLAMP_US)
    })
}

/// Outcome of one parked bridge poll: the master refilled (keep gathering into
/// the SAME batch) or delivery is due (quiet timeout, wake signal, HUP, error).
enum BridgeWait {
    Refill,
    Deliver,
}

/// Park for the writer's next refill: one bounded poll on `[master, wake_rd]`.
/// A pending wake byte is deliberately NOT consumed — [`read_or_wake`] owns the
/// wake protocol and must observe it after the batch is delivered.
fn bridge_poll(master: i32, wake_rd: i32, timeout_ms: i32) -> BridgeWait {
    let mut fds = [
        libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_rd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let nfds: libc::nfds_t = if wake_rd >= 0 { 2 } else { 1 };
    // SAFETY: polling fds this gather owns, with a bounded timeout.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), nfds, timeout_ms) };
    if r <= 0 {
        return BridgeWait::Deliver; // timeout (burst over), EINTR, or error
    }
    if nfds == 2 && fds[1].revents != 0 {
        return BridgeWait::Deliver; // teardown wake — deliver what we have
    }
    if fds[0].revents & libc::POLLIN != 0 {
        return BridgeWait::Refill;
    }
    BridgeWait::Deliver // HUP/ERR without data: the next read_or_wake decides
}

/// Park the parser-idle cutoff for the writer's next refill: one bounded
/// `select(2)` on `[master, wake_rd]` — select because `poll(2)` is
/// ms-granular and the refill gaps this bridges are µs-scale. A pending wake
/// byte is deliberately NOT consumed — [`read_or_wake`] owns the wake
/// protocol (same rule as [`bridge_poll`]). Timeout, error, or EINTR all
/// mean deliver: the hysteresis wait must never outlive its budget.
fn idle_refill_wait(master: i32, wake_rd: i32, wait_us: u32) -> BridgeWait {
    // select cannot represent fds >= FD_SETSIZE — deliver, same as knob-off.
    if master >= libc::FD_SETSIZE as i32 || wake_rd >= libc::FD_SETSIZE as i32 {
        return BridgeWait::Deliver;
    }
    // SAFETY: an all-zero fd_set is the empty set FD_ZERO produces.
    let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
    // SAFETY: both fds were bounds-checked against FD_SETSIZE above.
    unsafe {
        libc::FD_SET(master, &mut set);
        if wake_rd >= 0 {
            libc::FD_SET(wake_rd, &mut set);
        }
    }
    let mut tv = libc::timeval {
        tv_sec: (wait_us / 1_000_000) as _,
        tv_usec: (wait_us % 1_000_000) as _,
    };
    // SAFETY: selecting on fds this gather owns, with a bounded timeout.
    let r = unsafe {
        libc::select(
            master.max(wake_rd) + 1,
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut tv,
        )
    };
    if r <= 0 {
        return BridgeWait::Deliver; // timeout (burst over), EINTR, or error
    }
    // SAFETY: reading a set select just filled; fds are < FD_SETSIZE.
    if wake_rd >= 0 && unsafe { libc::FD_ISSET(wake_rd, &set) } {
        return BridgeWait::Deliver; // teardown wake — byte left for read_or_wake
    }
    // SAFETY: as above.
    if unsafe { libc::FD_ISSET(master, &set) } {
        return BridgeWait::Refill;
    }
    BridgeWait::Deliver
}

/// [`drain_more`] for an `O_NONBLOCK` master: drain `read(2)` DIRECTLY to
/// `EAGAIN`, then bridge the writer's µs refill gaps with up to [`NB_SPIN_MAX`]
/// IMMEDIATE re-reads — the ~0.4 µs failed `read` IS the pause (a paced
/// 256×`spin_loop` block measures ~2 µs on Apple Silicon, so pacing overshoots
/// the ~1-2 µs refill by whole gap-lengths; measured ladder: paced 195 MB/s vs
/// ghostty-shape ~295 target). When a gap outlives the probe burst, ONE parked
/// [`BRIDGE_POLL_MS`] poll on `[master, wake]` CONTINUES the same batch on
/// refill rather than delivering a partial. Delivery triggers:
///   * first quiet with `filled` under one outq (~1 KiB) ⇒ interactive echo, NOW;
///   * the parse stage idle at gap-check time (`parse_in_flight == 0`, sampled
///     once per dry gap before parking) ⇒ holding the batch adds pure output
///     latency, and a mid-burst query's reply may be sitting in it;
///   * bridge poll quiet for a full timeout (burst over), wake-pipe signal
///     (teardown; the byte is LEFT for [`read_or_wake`]), [`BATCH_BUDGET`]
///     spent (checked per dry gap, so the effective bound is the budget plus
///     one bridge poll, ~4 ms — same shape as ghostty's), batch full, EOF, or
///     hard error (surfaced by the next [`read_or_wake`]).
///
/// CALLER CONTRACT: the master MUST be non-blocking ([`set_nonblocking`]) — on a
/// blocking fd the direct `read` would park the gather mid-batch. Callers keep
/// [`drain_more`] as the fallback when the fcntl fails. `wake_rd < 0` (fd
/// exhaustion) drops the wake fd from the bridge poll; `parse_in_flight = None`
/// (no parse stage attached) bridges unconditionally.
///
/// The parser-idle cutoff carries a measured hysteresis ([`idle_poll_us`],
/// `ATERM_PTY_IDLE_POLL_US`, default [`IDLE_POLL_DEFAULT_US`]): a dry idle
/// gap waits up to that many µs for a refill before delivering — the
/// immediate cutoff's small-batch churn costs ~33-43 MB/s of flood
/// throughput; `0` restores immediate delivery.
pub fn drain_more_nonblocking(
    master: i32,
    buf: &mut [u8],
    filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
) -> usize {
    drain_more_nonblocking_with_idle_wait(
        master,
        buf,
        filled,
        wake_rd,
        parse_in_flight,
        idle_poll_us(),
    )
}

/// [`drain_more_nonblocking`] body with the idle-cutoff hysteresis as an
/// explicit parameter: the [`idle_poll_us`] OnceLock caches the env once per
/// process, so tests needing different knob values drive this directly.
fn drain_more_nonblocking_with_idle_wait(
    master: i32,
    buf: &mut [u8],
    filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    idle_wait_us: u32,
) -> usize {
    drain_more_nonblocking_with_idle_wait_after_gap(
        master,
        buf,
        filled,
        wake_rd,
        parse_in_flight,
        idle_wait_us,
        || {},
    )
}

/// Shared gather body. `after_idle_gap` is a deterministic test seam invoked
/// after the immediate probes are exhausted and immediately before an armed
/// parser-idle wait; the shipping wrapper supplies an inlined no-op.
fn drain_more_nonblocking_with_idle_wait_after_gap(
    master: i32,
    buf: &mut [u8],
    mut filled: usize,
    wake_rd: i32,
    parse_in_flight: Option<&std::sync::atomic::AtomicUsize>,
    idle_wait_us: u32,
    mut after_idle_gap: impl FnMut(),
) -> usize {
    /// One kernel tty output queue's worth — at/above this the writer saturated it.
    const SATURATED: usize = 1024;
    /// Immediate re-read budget per refill gap (ghostty's bridge length; 8-16
    /// catches >90% of gaps).
    const NB_SPIN_MAX: u32 = 16;
    /// Legacy paced-probe pause length (revert knob only).
    const NB_SPIN_ITERS: u32 = 256;
    /// One parked poll per dry gap; quiet for the full timeout ⇒ burst over.
    const BRIDGE_POLL_MS: i32 = 1;
    /// Per-batch latency budget (ghostty's); checked per dry gap, so the
    /// effective bound is this plus one [`BRIDGE_POLL_MS`] park.
    const BATCH_BUDGET: std::time::Duration = std::time::Duration::from_millis(3);
    let legacy = bridge_legacy_spin();
    let start = std::time::Instant::now();
    let mut spins = 0u32;
    while filled < buf.len() {
        let n = read(master, &mut buf[filled..]);
        if n > 0 {
            filled += n as usize;
            spins = 0; // fresh probe budget per refill gap
            continue;
        }
        if n == 0 {
            break; // EOF: keep what we have; the next read_or_wake reports it
        }
        match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => {
                // Quiet. Interactive (< one outq) delivers NOW; over-budget
                // batches deliver regardless of bridge luck.
                if filled < SATURATED || start.elapsed() >= BATCH_BUDGET {
                    break;
                }
                if spins < NB_SPIN_MAX {
                    spins += 1;
                    if legacy {
                        for _ in 0..NB_SPIN_ITERS {
                            std::hint::spin_loop();
                        }
                    }
                    continue;
                }
                if legacy {
                    break; // legacy bridge delivered once the probe budget ran dry
                }
                if parse_in_flight
                    .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed) == 0)
                {
                    if idle_wait_us == 0 {
                        break; // parser idle: bridging would add pure output latency
                    }
                    // Hysteresis: a µs-bounded wait for the refill instead of
                    // delivering a churn-sized batch (BATCH_BUDGET still caps
                    // the whole gather — it is re-checked per dry gap).
                    after_idle_gap();
                    match idle_refill_wait(master, wake_rd, idle_wait_us) {
                        BridgeWait::Refill => {
                            spins = 0;
                            continue;
                        }
                        BridgeWait::Deliver => break,
                    }
                }
                match bridge_poll(master, wake_rd, BRIDGE_POLL_MS) {
                    BridgeWait::Refill => spins = 0,
                    BridgeWait::Deliver => break,
                }
            }
            _ => break, // hard error: the next read_or_wake surfaces it
        }
    }
    filled
}

/// Signal a reader parked in [`read_or_wake`] to stop: write one byte to the wake
/// pipe's write end. Best-effort — a full or closed pipe is harmless (one queued byte
/// is enough to make `poll` return).
pub fn wake(wake_wr: i32) {
    let b = [1u8];
    // SAFETY: `wake_wr` is the pipe write end; a 1-byte write is atomic (never partial).
    unsafe {
        libc::write(wake_wr, b.as_ptr() as *const libc::c_void, 1);
    }
}

/// Close a raw fd (a wake-pipe end). Best-effort; ignores a negative sentinel.
pub fn close_fd(fd: i32) {
    if fd >= 0 {
        // SAFETY: closing an fd the caller owns.
        unsafe {
            libc::close(fd);
        }
    }
}

/// Focus-linked shell QoS boost — Unix no-op stub (identical signature, so
/// the GUI's focus-change call site compiles unchanged). On Unix the kernel
/// PTY is a zero-process byte queue with no starvable middlemen, and the
/// scheduler's own interactivity wake-up boosts already protect the shell's
/// echo; the real implementation (ConPTY conhost + shell root,
/// `SetPriorityClass` + power-throttling) lives in `src/windows/mod.rs`.
pub fn set_focus_boost(_master: i32, _on: bool) {}

/// Resize the PTY to `rows`×`cols` (`TIOCSWINSZ`).
pub fn resize(master: i32, rows: u16, cols: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `master` is a valid PTY master fd; `&ws` is a valid `winsize` for
    // the `TIOCSWINSZ` ioctl.
    unsafe {
        libc::ioctl(master, libc::TIOCSWINSZ, &ws);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_pipe_is_cloexec_and_nonblocking_on_both_ends() {
        let (rd, wr) = make_wake_pipe().expect("wake pipe");
        // SAFETY: read-only fcntl queries on two live fds owned by this test.
        unsafe {
            for fd in [rd, wr] {
                assert_ne!(libc::fcntl(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
                assert_ne!(libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0);
            }
        }
        close_fd(rd);
        close_fd(wr);
    }

    #[test]
    fn saturated_wake_pipe_never_blocks_the_caller() {
        let (rd, wr) = make_wake_pipe().expect("wake pipe");
        let fill = [0x55u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            let wrote = unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) };
            if wrote < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock,
                    "pipe saturation must report EAGAIN"
                );
                break;
            }
        }
        let started = std::time::Instant::now();
        wake(wr);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(20),
            "a redundant wake against a full pipe must return promptly"
        );
        close_fd(rd);
        close_fd(wr);
    }

    /// MEM-L2: a reader parked in `read_or_wake` on a master that STAYS OPEN with no
    /// data and never EOFs (the orphaned-child-holds-the-slave scenario) must still
    /// unblock the instant the owner fires the wake pipe — otherwise `Session::drop`
    /// could never reclaim the reader (and the Terminal + master fd it pins).
    #[test]
    fn read_or_wake_unblocks_on_wake_even_when_master_never_eofs() {
        use std::sync::mpsc;
        use std::time::Duration;
        // A pipe stands in for a master held open with no data: its write end lives for
        // the whole test, so the read end never sees data OR EOF.
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (master_rd, master_wr) = (m[0], m[1]);
        let (wake_rd, wake_wr) = make_wake_pipe().expect("wake pipe");

        let (tx, rx) = mpsc::channel();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 16];
            let out = read_or_wake(master_rd, &mut buf, wake_rd);
            tx.send(matches!(out, ReadOutcome::Wake)).unwrap();
            close_fd(wake_rd);
        });

        // Neither fd is ready ⇒ the reader must stay parked (not busy-return).
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "reader must block while master has no data and the wake pipe is empty"
        );

        // Fire the wake: the reader must return `Wake` promptly even though `master_rd`
        // never saw data or EOF (`master_wr` is still open).
        wake(wake_wr);
        let got_wake = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reader must unblock promptly once the wake pipe fires");
        assert!(got_wake, "the wake pipe must yield ReadOutcome::Wake");

        h.join().unwrap();
        close_fd(wake_wr);
        // SAFETY: closing the two pipe fds this test owns.
        unsafe {
            libc::close(master_rd);
            libc::close(master_wr);
        }
    }

    /// Direct-read drain (O_NONBLOCK gather): an interactive-sized burst (< one
    /// outq) must be delivered on the FIRST quiet read — no spin bridge, no
    /// parking — and gather exactly the bytes written.
    #[test]
    fn drain_more_nonblocking_delivers_interactive_burst_on_first_quiet_read() {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(unsafe { libc::write(wr, b"echo".as_ptr().cast(), 4) }, 4);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1]); // stand-in for the read_or_wake first chunk
        assert_eq!(n, 1);
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking(rd, &mut buf, 1, -1, None);
        assert_eq!(filled, 4, "must gather the whole burst");
        assert_eq!(&buf[..4], b"echo");
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(50),
            "interactive drain must return on the first quiet read, not spin/park"
        );
        close_fd(rd);
        close_fd(wr);
    }

    /// Direct-read drain: a saturated burst (>= one outq) is gathered fully into
    /// one batch, and EOF mid-drain just stops the top-up (kept bytes intact).
    #[test]
    fn drain_more_nonblocking_gathers_saturated_burst_and_survives_eof() {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0xA5u8; 8192];
        // SAFETY: bounded write to this test's live pipe end (8 KiB fits the
        // default pipe buffer).
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        close_fd(wr); // EOF after the burst: the drain must keep what it read
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let filled = drain_more_nonblocking(rd, &mut buf, n as usize, -1, None);
        assert_eq!(
            filled,
            payload.len(),
            "must gather the full saturated burst"
        );
        assert!(buf[..filled].iter().all(|&b| b == 0xA5));
        close_fd(rd);
    }

    /// Bridge teardown contract: a wake byte pending during the bridge poll
    /// makes the drain deliver what it has WITHOUT consuming the byte —
    /// `read_or_wake` owns the wake protocol and must still observe it.
    #[test]
    fn drain_more_nonblocking_bridge_yields_to_wake_and_leaves_the_byte() {
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0x5Au8; 4096];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        let mut wk = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(wk.as_mut_ptr()) }, 0, "wake pipe");
        let (wake_rd, wake_wr) = (wk[0], wk[1]);
        wake(wake_wr); // teardown byte queued BEFORE the drain bridges
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let filled = drain_more_nonblocking(rd, &mut buf, n as usize, wake_rd, None);
        assert_eq!(filled, payload.len(), "keeps the drained burst");
        let mut b = [0u8; 4];
        // SAFETY: bounded read from this test's live wake pipe end.
        let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
        assert_eq!(wn, 1, "bridge must NOT consume the wake byte");
        close_fd(rd);
        close_fd(wr);
        close_fd(wake_rd);
        close_fd(wake_wr);
    }

    /// Refill continuation — THE flood-throughput mechanism (181→226 MB/s):
    /// with the parse stage BUSY (`in_flight > 0`), a refill landing during the
    /// bridge poll must be gathered into the SAME batch, not delivered as a
    /// partial. Also pins the idle-cutoff's sign: inverting `== 0` would
    /// deliver at the first dry gap and this test would see only chunk 1.
    /// Scheduling-flake damped by retrying: a broken bridge NEVER continues,
    /// so any passing attempt proves the mechanism.
    #[test]
    fn drain_more_nonblocking_busy_parser_bridges_refill_into_same_batch() {
        use std::sync::atomic::AtomicUsize;
        let mut ok = false;
        for _ in 0..3 {
            let mut m = [0i32; 2];
            // SAFETY: valid 2-int out-array for pipe(2).
            assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
            let (rd, wr) = (m[0], m[1]);
            set_nonblocking(rd, true).expect("nonblock read end");
            let chunk = [0xC3u8; 2048];
            // SAFETY: bounded write to this test's live pipe end.
            assert_eq!(
                unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                chunk.len() as isize
            );
            let writer = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_micros(300));
                // SAFETY: bounded write to the pipe end this thread owns.
                unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) };
                close_fd(wr);
            });
            let busy = AtomicUsize::new(1);
            let mut buf = [0u8; 65_536];
            let n = read(rd, &mut buf[..1024]);
            assert!(n > 0);
            let filled = drain_more_nonblocking(rd, &mut buf, n as usize, -1, Some(&busy));
            writer.join().unwrap();
            close_fd(rd);
            if filled == 2 * chunk.len() {
                ok = true;
                break;
            }
        }
        assert!(
            ok,
            "a busy-parser bridge must continue the batch across a refill gap"
        );
    }

    /// Parser-idle cutoff (the ghostty fps-fire class): with `in_flight == 0`
    /// and the stream dry, the drain must deliver after the probe burst —
    /// never park in the 1 ms bridge poll. A regressed cutoff parks every
    /// attempt (≥1 ms); the µs-path passing once proves the cutoff. Runs both
    /// the public fn (env unset ⇒ the measured `IDLE_POLL_DEFAULT_US` = 50 µs
    /// wait, still far under this test's 1 ms park bound) and the inner body
    /// with an explicit 0 (the immediate-deliver revert path).
    #[test]
    fn drain_more_nonblocking_idle_parser_delivers_without_parking() {
        use std::sync::atomic::AtomicUsize;
        for explicit_zero in [false, true] {
            let mut ok = false;
            for _ in 0..3 {
                let mut m = [0i32; 2];
                // SAFETY: valid 2-int out-array for pipe(2).
                assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
                let (rd, wr) = (m[0], m[1]);
                set_nonblocking(rd, true).expect("nonblock read end");
                let chunk = [0x7Eu8; 4096];
                // SAFETY: bounded write to this test's live pipe end.
                assert_eq!(
                    unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                    chunk.len() as isize
                );
                let idle = AtomicUsize::new(0);
                let mut buf = [0u8; 65_536];
                let n = read(rd, &mut buf[..1024]);
                assert!(n > 0);
                let t0 = std::time::Instant::now();
                let filled = if explicit_zero {
                    drain_more_nonblocking_with_idle_wait(
                        rd,
                        &mut buf,
                        n as usize,
                        -1,
                        Some(&idle),
                        0,
                    )
                } else {
                    drain_more_nonblocking(rd, &mut buf, n as usize, -1, Some(&idle))
                };
                let el = t0.elapsed();
                close_fd(rd);
                close_fd(wr);
                assert_eq!(filled, chunk.len(), "keeps the drained burst");
                // The public path folds in the CONFIGURED idle wait (env
                // knob or the default, clamped) so an exported
                // ATERM_PTY_IDLE_POLL_US cannot fail this test spuriously.
                let bound = if explicit_zero {
                    std::time::Duration::from_millis(1)
                } else {
                    std::time::Duration::from_millis(1)
                        + std::time::Duration::from_micros(u64::from(idle_poll_us()))
                };
                if el < bound {
                    ok = true;
                    break;
                }
            }
            assert!(
                ok,
                "an idle-parser dry gap must deliver within its idle wait, not \
                 park the 1ms bridge poll (explicit_zero={explicit_zero}, \
                 configured idle wait {}µs)",
                idle_poll_us()
            );
        }
    }

    /// Pins the SHIPPED default from below: [`IDLE_POLL_DEFAULT_US`] must be
    /// armed (> 0) and must actually bridge a refill that lands AFTER the
    /// immediate probe window, inside the default wait. The refill is injected
    /// synchronously at that exact seam: a sub-50µs cross-thread deadline would
    /// test scheduler luck under load rather than the gather protocol.
    #[test]
    fn drain_more_nonblocking_default_idle_wait_is_armed_and_bridges() {
        use std::sync::atomic::AtomicUsize;
        const {
            assert!(
                IDLE_POLL_DEFAULT_US > 0,
                "the measured idle-refill default must stay armed"
            );
        }
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        const CHUNK_LEN: usize = 2048;
        let chunk = [0xD6u8; CHUNK_LEN];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let idle = AtomicUsize::new(0);
        // Bound the gather to exactly the initial chunk plus the injected
        // refill. Sibling fork/PTY tests can briefly inherit raw descriptors,
        // so an EOF-driven test races their lifetime; reaching capacity after
        // exactly two chunks terminates deterministically.
        let mut buf = [0u8; 2 * CHUNK_LEN];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let mut injected = false;
        let filled = drain_more_nonblocking_with_idle_wait_after_gap(
            rd,
            &mut buf,
            n as usize,
            -1,
            Some(&idle),
            IDLE_POLL_DEFAULT_US,
            || {
                assert!(!injected, "the refill must be injected exactly once");
                // SAFETY: bounded write to this test's live pipe end.
                assert_eq!(
                    unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                    chunk.len() as isize
                );
                injected = true;
            },
        );
        close_fd(wr);
        close_fd(rd);
        assert!(
            injected,
            "the armed default must reach the parser-idle wait after its probes"
        );
        assert_eq!(
            filled,
            2 * chunk.len(),
            "the default idle wait must bridge the post-probe refill into the same batch"
        );
    }

    /// Hysteresis knob (`ATERM_PTY_IDLE_POLL_US` > 0): an idle parser at a dry
    /// gap WAITS for the writer's refill instead of delivering, so the refill
    /// lands in the SAME batch. 100 ms is deliberately huge so scheduling can
    /// never expire the wait before the +2 ms refill; the writer's close (EOF)
    /// ends the batch. Retry damps scheduling flakes: a knob-off cutoff NEVER
    /// continues the batch, so any passing attempt proves the hysteresis.
    #[test]
    fn drain_more_nonblocking_idle_wait_bridges_refill_into_same_batch() {
        use std::sync::atomic::AtomicUsize;
        let mut ok = false;
        for _ in 0..3 {
            let mut m = [0i32; 2];
            // SAFETY: valid 2-int out-array for pipe(2).
            assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
            let (rd, wr) = (m[0], m[1]);
            set_nonblocking(rd, true).expect("nonblock read end");
            let chunk = [0xB4u8; 2048];
            // SAFETY: bounded write to this test's live pipe end.
            assert_eq!(
                unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
                chunk.len() as isize
            );
            let writer = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(2));
                // SAFETY: bounded write to the pipe end this thread owns.
                unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) };
                close_fd(wr);
            });
            let idle = AtomicUsize::new(0);
            let mut buf = [0u8; 65_536];
            let n = read(rd, &mut buf[..1024]);
            assert!(n > 0);
            let filled = drain_more_nonblocking_with_idle_wait(
                rd,
                &mut buf,
                n as usize,
                -1,
                Some(&idle),
                100_000,
            );
            writer.join().unwrap();
            close_fd(rd);
            if filled == 2 * chunk.len() {
                ok = true;
                break;
            }
        }
        assert!(
            ok,
            "an armed idle wait must gather the refill into the same batch"
        );
    }

    /// Hysteresis + teardown: a wake byte pending when the idle wait parks
    /// must deliver promptly WITHOUT consuming the byte — [`read_or_wake`]
    /// owns the wake protocol (same contract as the busy-parser bridge poll).
    #[test]
    fn drain_more_nonblocking_idle_wait_yields_to_wake_and_leaves_the_byte() {
        use std::sync::atomic::AtomicUsize;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let payload = [0x9Cu8; 4096];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        let mut wk = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(wk.as_mut_ptr()) }, 0, "wake pipe");
        let (wake_rd, wake_wr) = (wk[0], wk[1]);
        wake(wake_wr); // teardown byte queued BEFORE the idle wait parks
        let idle = AtomicUsize::new(0);
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking_with_idle_wait(
            rd,
            &mut buf,
            n as usize,
            wake_rd,
            Some(&idle),
            100_000,
        );
        assert_eq!(filled, payload.len(), "keeps the drained burst");
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(50),
            "a queued wake must deliver promptly, not park the full idle wait"
        );
        let mut b = [0u8; 4];
        // Non-blocking: a CONSUMED wake byte must fail the assert below, not
        // hang this read forever.
        set_nonblocking(wake_rd, true).expect("nonblock wake read end");
        // SAFETY: bounded read from this test's live wake pipe end.
        let wn = unsafe { libc::read(wake_rd, b.as_mut_ptr().cast(), 4) };
        assert_eq!(wn, 1, "idle wait must NOT consume the wake byte");
        close_fd(rd);
        close_fd(wr);
        close_fd(wake_rd);
        close_fd(wake_wr);
    }

    /// Producer death mid-bridge: the write end closing while the drain is
    /// parked in the bridge poll must deliver promptly with what it has
    /// (via the POLLHUP Deliver arm, or the read()==0 EOF break one syscall
    /// later — both protocol-safe), never stall against a dead producer.
    #[test]
    fn drain_more_nonblocking_delivers_promptly_on_writer_hup() {
        use std::sync::atomic::AtomicUsize;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(rd, true).expect("nonblock read end");
        let chunk = [0x11u8; 2048];
        // SAFETY: bounded write to this test's live pipe end.
        assert_eq!(
            unsafe { libc::write(wr, chunk.as_ptr().cast(), chunk.len()) },
            chunk.len() as isize
        );
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_micros(300));
            close_fd(wr); // HUP lands while the drain bridges the dry gap
        });
        let busy = AtomicUsize::new(1); // busy parser: the bridge parks
        let mut buf = [0u8; 65_536];
        let n = read(rd, &mut buf[..1024]);
        assert!(n > 0);
        let t0 = std::time::Instant::now();
        let filled = drain_more_nonblocking(rd, &mut buf, n as usize, -1, Some(&busy));
        closer.join().unwrap();
        assert_eq!(filled, chunk.len(), "keeps what was drained before HUP");
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(50),
            "a dead producer must deliver promptly, not stall the batch"
        );
        close_fd(rd);
    }

    /// Blocking-emulated write on an O_NONBLOCK description: a full queue must
    /// PARK (in poll) until the reader drains, then complete — never surface
    /// `WouldBlock` (the legacy blocking-master contract every frame writer keeps).
    #[test]
    fn write_some_blocking_parks_through_a_full_queue_instead_of_erroring() {
        use std::time::Duration;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(wr, true).expect("nonblock write end");
        // Fill the pipe to EAGAIN.
        let fill = [0u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            if unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) } < 0 {
                assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
                break;
            }
        }
        // Drain the whole pipe from a helper thread after a beat, so the parked
        // writer has room for the retry.
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut sink_buf = [0u8; 65_536];
            set_nonblocking(rd, true).expect("nonblock read end");
            loop {
                let n = read(rd, &mut sink_buf);
                if n <= 0 {
                    break;
                }
            }
            rd // keep rd open until after the writer completes
        });
        let wrote = write_some_blocking(wr, b"parked-frame").expect("must not surface WouldBlock");
        assert!(
            wrote > 0,
            "the retried write must land bytes after the drain"
        );
        let rd = h.join().unwrap();
        close_fd(rd);
        close_fd(wr);
    }

    /// The drain-loop twin: same park-through-EAGAIN contract, count-only shape —
    /// a full-but-alive queue must NOT collapse to 0 (0 = session-dead drops the spill).
    #[test]
    fn write_some_count_blocking_parks_instead_of_reporting_dead() {
        use std::time::Duration;
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (rd, wr) = (m[0], m[1]);
        set_nonblocking(wr, true).expect("nonblock write end");
        let fill = [0u8; 4096];
        loop {
            // SAFETY: bounded write to this test's live non-blocking pipe end.
            if unsafe { libc::write(wr, fill.as_ptr().cast(), fill.len()) } < 0 {
                break;
            }
        }
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let mut sink_buf = [0u8; 65_536];
            set_nonblocking(rd, true).expect("nonblock read end");
            loop {
                let n = read(rd, &mut sink_buf);
                if n <= 0 {
                    break;
                }
            }
            rd
        });
        let wrote = write_some_count_blocking(wr, b"spill-chunk");
        assert!(
            wrote > 0,
            "a full-but-alive queue must park+retry, not report dead"
        );
        let rd = h.join().unwrap();
        close_fd(rd);
        close_fd(wr);
    }

    /// MEM-L2 fallback: when NO wake pipe exists (`wake_rd < 0`, the `make_wake_pipe`
    /// failed-under-fd-exhaustion path), a reader on a master that stays open with no data
    /// and never EOFs must NOT park forever — it must time out and return `Idle` so the
    /// owning session's fd-free stop flag can still tear it down. A bare blocking `read`
    /// here is exactly the silent MEM-L2 re-open this guards against.
    #[test]
    fn read_or_wake_wakeless_fallback_times_out_instead_of_parking() {
        use std::time::{Duration, Instant};
        // A pipe read end whose write end lives for the whole test stands in for an
        // orphan-held master: never readable, never EOF.
        let mut m = [0i32; 2];
        // SAFETY: valid 2-int out-array for pipe(2).
        assert_eq!(unsafe { libc::pipe(m.as_mut_ptr()) }, 0, "pipe");
        let (master_rd, master_wr) = (m[0], m[1]);

        let mut buf = [0u8; 16];
        let t0 = Instant::now();
        let out = read_or_wake(master_rd, &mut buf, -1);
        let elapsed = t0.elapsed();

        assert!(
            matches!(out, ReadOutcome::Idle),
            "wakeless fallback must return Idle on a quiet master, never block or read"
        );
        // It returned via the bounded poll timeout, not by parking indefinitely.
        assert!(
            elapsed < Duration::from_secs(2),
            "fallback must return within the poll timeout, not hang"
        );

        // SAFETY: closing the two pipe fds this test owns.
        unsafe {
            libc::close(master_rd);
            libc::close(master_wr);
        }
    }

    /// PROOF-CARRYING DSU (RFC Rung 1b) — the FD-handoff mechanism, proven with REAL
    /// syscalls: a PTY master survives `execve` IFF `FD_CLOEXEC` is cleared. Opens a
    /// real pty pair, and forks+execs a child that reports whether the inherited fd is
    /// open — the exact "the shell's master survives the seamless re-exec" contract.
    #[test]
    fn cloexec_controls_master_survival_across_exec() {
        // A real pty pair (no shell).
        let (mut master, mut slave) = (0i32, 0i32);
        // SAFETY: valid out-params; openpty fills master/slave on success.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty failed");

        // fd_is_tty is a SANITY backstop, not a master discriminator: it accepts any
        // tty (master AND slave) and rejects the obviously-wrong (pipe / bad fd).
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let tty_master = fd_is_tty(master);
        let tty_slave = fd_is_tty(slave); // a slave IS a tty — documents the non-guarantee
        let tty_bad = fd_is_tty(-1);
        let tty_pipe = fd_is_tty(pipe[0]);

        // A tiny helper: fork+exec `/bin/test -e /dev/fd/<master>` and return whether
        // the fd was OPEN in the exec'd child (i.e. survived the exec). NOTE: libtest
        // runs tests on a THREAD POOL, so this process is multi-threaded at fork time.
        // That is safe here because the child touches ONLY async-signal-safe calls
        // (`execv`, `_exit`) before the image is replaced — it never allocates, locks,
        // or calls back into std. The parent reaps the child.
        let survived = |m: i32| -> bool {
            let path = std::ffi::CString::new("/bin/test").unwrap();
            let arg0 = std::ffi::CString::new("test").unwrap();
            let arg1 = std::ffi::CString::new("-e").unwrap();
            let arg2 = std::ffi::CString::new(format!("/dev/fd/{m}")).unwrap();
            let pid = unsafe { libc::fork() };
            assert!(pid >= 0, "fork failed");
            if pid == 0 {
                let argv = [
                    arg0.as_ptr(),
                    arg1.as_ptr(),
                    arg2.as_ptr(),
                    std::ptr::null(),
                ];
                unsafe {
                    libc::execv(path.as_ptr(), argv.as_ptr());
                    libc::_exit(127); // execv only returns on failure
                }
            }
            let mut status = 0i32;
            unsafe { libc::waitpid(pid, &mut status, 0) };
            // WIFEXITED && WEXITSTATUS == 0  ⇒ `test -e` found the fd open.
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0
        };

        // CLOEXEC set (the isolation default the parent applies after forkpty) ⇒ the
        // master is CLOSED at exec (a naive re-exec would orphan the shell). Cleared ⇒
        // it SURVIVES (the seamless handoff's contract).
        set_cloexec(master, true).unwrap();
        let survives_when_cloexec = survived(master);
        set_cloexec(master, false).unwrap();
        let survives_when_cleared = survived(master);

        // The surviving master is still a live, usable pty (write master → read slave).
        set_nonblocking(slave, true).unwrap();
        write_all(master, b"ping\n");
        let mut buf = [0u8; 8];
        let mut got = 0isize;
        for _ in 0..1000 {
            got = read(slave, &mut buf);
            if got > 0 {
                break;
            }
        }

        // Close every fd BEFORE asserting, so a failed assertion can't leak them.
        for fd in [master, slave, pipe[0], pipe[1]] {
            unsafe { libc::close(fd) };
        }

        assert!(tty_master, "the master is a tty");
        assert!(
            tty_slave,
            "a pty SLAVE is also a tty (fd_is_tty is not master-only)"
        );
        assert!(!tty_bad, "a bad fd is not a tty");
        assert!(!tty_pipe, "a pipe end is not a tty");
        assert!(
            !survives_when_cloexec,
            "a CLOEXEC master does NOT survive exec"
        );
        assert!(
            survives_when_cleared,
            "a cleared-CLOEXEC master SURVIVES exec (seamless handoff)"
        );
        assert!(got > 0, "the re-adoptable master still drives its slave");
    }

    // Validates the `write_all` + `read` syscall wrappers on a real fd (a plain
    // pipe), so the seam's IO is exercised without spawning a shell (no flake, no
    // leftover process). `spawn_shell`/`resize` are exercised end-to-end by the
    // GUI that depends on this crate.
    #[test]
    fn write_all_then_read_roundtrips_on_a_pipe() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);

        write_all(wr, b"hello-pty-seam");
        let mut buf = [0u8; 64];
        let n = read(rd, &mut buf);
        assert!(n > 0, "read returned {n}");
        assert_eq!(&buf[..n as usize], b"hello-pty-seam");

        // SAFETY: closing the two fds we just opened.
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    // SEC-2: a confinement failure in the child must FAIL CLOSED. We force the
    // sandbox `apply` to fail by handing it an UNTRUSTED `Cap<Sandbox>` (its gate
    // requires Trusted+), so the child takes the `_exit(126)` path BEFORE exec and
    // the parent returns an error instead of a master fd for an unconfined shell.
    #[test]
    fn sandbox_apply_failure_in_child_fails_closed_no_unconfined_shell() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        // A valid spawn cap (passes the PARENT gate) but a too-weak sandbox cap
        // (fails the CHILD's `apply` gate) — exactly the silent-unconfined hole.
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let weak_sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Untrusted);

        let result = spawn_shell(
            24,
            80,
            &spawn_cap,
            &weak_sandbox,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        );
        let err = result
            .expect_err("a sandbox confinement failure must surface as an error, NOT a master fd");
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "child sandbox failure must be reported as PermissionDenied, got: {err}",
        );
        assert!(
            err.to_string().contains("fail-closed"),
            "error should describe the fail-closed confinement: {err}",
        );
    }

    // The success path still works: with a properly-tiered sandbox cap a real
    // `$SHELL` spawns and the parent gets a live master fd. Reading from it (the
    // shell's first prompt/banner, or at least the PTY echo) proves a process is
    // attached; then we close the master to tear the child down.
    #[test]
    fn normal_shell_spawns_with_a_trusted_sandbox_cap() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        // Run a deterministic command then exit, so the test does not hang on an
        // interactive prompt: ATERM_EXEC makes the child run it, then exec $SHELL.
        // Using a bare `echo` + immediate close is enough to prove a live master.
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        )
        .expect("a normal shell must spawn with a Trusted sandbox cap");
        assert!(master >= 0, "master fd must be valid, got {master}");
        // Best-effort: write a harmless newline and read whatever echoes back, to
        // confirm the fd is a live PTY master, not a dangling descriptor.
        write_all(master, b"\n");
        let mut buf = [0u8; 64];
        let _ = read(master, &mut buf); // may be 0 if the child raced exit; fd is still valid
        // SAFETY: closing the master tears down the child's controlling tty.
        unsafe {
            libc::close(master);
        }
    }

    #[test]
    fn spawned_master_is_close_on_exec() {
        // REGRESSION: the forkpty master was returned WITHOUT FD_CLOEXEC, so a
        // later session's child shell would inherit (through execve) every prior
        // session's still-open master — an ungated cross-session input-injection
        // / output-exfiltration channel. The parent must mark the master
        // close-on-exec before handing it back.
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        )
        .expect("a normal shell must spawn with a Trusted sandbox cap");
        assert!(master >= 0, "master fd must be valid, got {master}");
        // SAFETY: querying fd flags on a valid fd.
        let flags = unsafe { libc::fcntl(master, libc::F_GETFD) };
        assert!(flags >= 0, "F_GETFD must succeed on the master fd");
        assert!(
            flags & libc::FD_CLOEXEC != 0,
            "PTY master must be FD_CLOEXEC so it never leaks into a later session's shell"
        );
        // SAFETY: closing the master tears down the child's controlling tty.
        unsafe {
            libc::close(master);
        }
    }

    // ---- spawn termios (kernel defaults + IUTF8/B230400 deltas) ----

    #[test]
    fn kernel_default_termios_probe_yields_cooked_defaults() {
        let d = kernel_default_termios().expect("throwaway-pty termios probe must succeed");
        // Sanity that we captured the kernel's COOKED defaults, not garbage: a
        // fresh NULL-termios pty is canonical + echoing + output-processing.
        assert_ne!(d.c_lflag & libc::ICANON, 0, "default pty must be canonical");
        assert_ne!(d.c_lflag & libc::ECHO, 0, "default pty must echo");
        assert_ne!(
            d.c_oflag & libc::OPOST,
            0,
            "default pty must post-process output"
        );
    }

    #[test]
    fn spawn_termios_deltas_are_exactly_iutf8_and_speed() {
        let d = kernel_default_termios().expect("probe");
        let t = build_spawn_termios(d, false);
        // The two documented deltas...
        assert_ne!(
            t.c_iflag & libc::IUTF8,
            0,
            "IUTF8 must be set for the UTF-8 child locale"
        );
        // SAFETY: cfget*speed only read the valid termios.
        unsafe {
            assert_eq!(libc::cfgetospeed(&t), libc::B230400);
            assert_eq!(libc::cfgetispeed(&t), libc::B230400);
        }
        // ...and NOTHING else: flag-for-flag identical to the kernel defaults.
        assert_eq!(t.c_iflag & !libc::IUTF8, d.c_iflag & !libc::IUTF8);
        assert_eq!(t.c_oflag, d.c_oflag);
        assert_eq!(t.c_lflag, d.c_lflag);
        assert_eq!(t.c_cc, d.c_cc);
        // On macOS speed lives in dedicated c_ispeed/c_ospeed fields, so c_cflag
        // must be untouched too (Linux stores CBAUD bits inside c_cflag — skip).
        #[cfg(target_os = "macos")]
        assert_eq!(t.c_cflag, d.c_cflag);
    }

    #[test]
    fn bench_no_opost_gate_clears_only_opost() {
        let d = kernel_default_termios().expect("probe");
        let bench = build_spawn_termios(d, true);
        let normal = build_spawn_termios(d, false);
        assert_eq!(
            bench.c_oflag & libc::OPOST,
            0,
            "bench gate must clear OPOST"
        );
        assert_eq!(
            bench.c_oflag | libc::OPOST,
            normal.c_oflag | libc::OPOST,
            "bench gate must change no other oflag"
        );
        assert_eq!(bench.c_iflag, normal.c_iflag);
        assert_eq!(bench.c_lflag, normal.c_lflag);
        assert_eq!(bench.c_cc, normal.c_cc);
    }

    #[test]
    fn spawned_pty_carries_iutf8_and_b230400() {
        // END-TO-END: the explicit termios must actually reach the spawned PTY.
        // Run `sleep` (touches no termios, unlike a shell) and tcgetattr the
        // master — on both BSD and Linux ptys the master reflects the slave's
        // termios, so the two deltas must be visible.
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<String> = vec!["/bin/sleep".into(), "5".into()];
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None, // argv_override
            Some(&exec),
            None,
            None,
        )
        .expect("sleep must spawn");
        assert!(master >= 0);
        // SAFETY: tcgetattr on a valid master fd fills the zeroed out-param.
        let t = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(master, &mut t), 0, "tcgetattr(master)");
            t
        };
        assert_ne!(t.c_iflag & libc::IUTF8, 0, "spawned pty must carry IUTF8");
        // SAFETY: cfgetospeed only reads the termios.
        assert_eq!(unsafe { libc::cfgetospeed(&t) }, libc::B230400);
        // SAFETY: close the master (sleep gets SIGHUP via slave close at exit).
        unsafe {
            libc::close(master);
        }
    }

    // ---- write_all branch logic (pure, via the extracted classifier) ----
    //
    // The EINTR-retry / short-write / peer-closed branch ladder of `write_all` is
    // a behavior-preserving extraction into `classify_write_result`. Testing the
    // pure classifier covers the EXACT decision the loop drives, WITHOUT having to
    // provoke a real (timing-dependent, flaky) `EINTR`.

    #[test]
    fn classify_write_eintr_negative_retries() {
        // r < 0 with EINTR => retry the write (do not drop the rest of the buffer).
        assert_eq!(classify_write_result(-1, true), WriteStep::Retry);
    }

    #[test]
    fn classify_write_noneintr_error_stops() {
        // r < 0 with any other errno (EIO, EBADF, EPIPE, …) => stop: master is gone.
        assert_eq!(classify_write_result(-1, false), WriteStep::Stop);
    }

    #[test]
    fn classify_write_zero_is_peer_closed_stop() {
        // r == 0 => peer closed; stop draining (errno is irrelevant here).
        assert_eq!(classify_write_result(0, false), WriteStep::Stop);
        assert_eq!(classify_write_result(0, true), WriteStep::Stop);
    }

    #[test]
    fn classify_write_partial_advances_by_exact_count() {
        // r > 0 => advance the cursor by EXACTLY r bytes (short-write handling).
        assert_eq!(classify_write_result(1, false), WriteStep::Advance(1));
        assert_eq!(classify_write_result(4096, false), WriteStep::Advance(4096));
    }

    // ---- locale resolution: the child always runs under a UTF-8 LC_CTYPE ----
    //
    // REGRESSION (the emacs `?` bug): the old GUI guard injected a UTF-8 locale ONLY
    // when LANG/LC_ALL/LC_CTYPE were ALL unset, so a present-but-non-UTF-8 locale
    // (LANG=C, bare en_US, LC_ALL=C, a stray non-UTF-8 LC_CTYPE) reached the child and
    // programs like emacs re-encoded pasted box-drawing UTF-8 to ASCII `?`. These
    // pin `resolve_spawn_locale` (which `build_child_env` then composes onto the env).

    #[test]
    fn is_utf8_locale_classifies_codeset() {
        // UTF-8 codesets in every spelling/case, with and without an @modifier.
        for ok in [
            "en_US.UTF-8",
            "en_US.UTF8",
            "en_US.utf-8",
            "en_US.utf8",
            "de_DE.UTF-8@euro",
        ] {
            assert!(is_utf8_locale(ok), "{ok} should be UTF-8");
        }
        // No codeset, or a non-UTF-8 one, is NOT UTF-8.
        for no in [
            "C",
            "POSIX",
            "en_US",
            "en_US.ISO8859-1",
            "",
            "fr_FR.ISO8859-15@euro",
        ] {
            assert!(!is_utf8_locale(no), "{no} should NOT be UTF-8");
        }
    }

    #[test]
    fn resolve_spawn_locale_edge_cases() {
        let kv = |k: &str, v: &str| (k.to_string(), v.to_string());
        // The injected value is platform-aware (C.UTF-8 off macOS; see SPAWN_UTF8_LOCALE).
        let ctype = || vec![kv("LC_CTYPE", SPAWN_UTF8_LOCALE)];
        let ctype_and_neutralize = || vec![kv("LC_CTYPE", SPAWN_UTF8_LOCALE), kv("LC_ALL", "")];

        // All unset (Finder/.app launch): inject the encoding category.
        assert_eq!(resolve_spawn_locale(None, None, None), ctype());
        // present-but-non-UTF-8 LANG — the emacs `?` repro.
        assert_eq!(resolve_spawn_locale(None, None, Some("C")), ctype());
        assert_eq!(resolve_spawn_locale(None, None, Some("POSIX")), ctype());
        // bare LANG (no codeset).
        assert_eq!(resolve_spawn_locale(None, None, Some("en_US")), ctype());
        // a non-UTF-8 LC_CTYPE shadowing a UTF-8 LANG: override LC_CTYPE; LANG untouched.
        assert_eq!(
            resolve_spawn_locale(None, Some("en_US.ISO8859-1"), Some("en_US.UTF-8")),
            ctype()
        );
        // LC_ALL=C dominating a UTF-8 LANG: override LC_CTYPE AND neutralize LC_ALL,
        // else the LC_CTYPE override would be dead (LC_ALL > LC_CTYPE).
        assert_eq!(
            resolve_spawn_locale(Some("C"), None, Some("en_US.UTF-8")),
            ctype_and_neutralize()
        );
        // both LC_ALL and LC_CTYPE non-UTF-8 at once: LC_ALL wins, same outcome.
        assert_eq!(
            resolve_spawn_locale(Some("C"), Some("en_US.ISO8859-1"), None),
            ctype_and_neutralize()
        );

        // Already UTF-8 anywhere in the effective slot: change NOTHING (no clobber).
        assert!(resolve_spawn_locale(None, None, Some("en_US.UTF-8")).is_empty());
        assert!(resolve_spawn_locale(Some("en_US.UTF-8"), None, None).is_empty());
        assert!(resolve_spawn_locale(None, Some("fr_FR.UTF-8"), Some("C")).is_empty());
        // A UTF-8 LC_ALL must NOT be touched even though a lower slot is non-UTF-8.
        assert!(resolve_spawn_locale(Some("en_US.UTF-8"), Some("C"), Some("C")).is_empty());
        // set-but-empty falls through (POSIX): empty LC_ALL/LC_CTYPE + UTF-8 LANG -> nothing.
        assert!(resolve_spawn_locale(Some(""), Some(""), Some("en_US.UTF-8")).is_empty());
        // UTF-8 spelling variants are all recognized (no needless override).
        for v in ["en_US.UTF8", "en_US.utf-8", "de_DE.UTF-8@euro"] {
            assert!(
                resolve_spawn_locale(None, None, Some(v)).is_empty(),
                "{v} is UTF-8"
            );
        }
    }

    /// CONFORMANCE: drive the REAL `resolve_spawn_locale` + `build_child_env` over
    /// every inherited-locale shape and assert the child's effective `LC_CTYPE` is
    /// UTF-8. The UTF-8/precedence oracle here is written INDEPENDENTLY of
    /// `is_utf8_locale` (a `.ends_with` check vs the production codeset parse) so a
    /// shared predicate bug cannot make the assertion vacuous.
    #[test]
    fn spawn_locale_conformance_child_always_utf8_ctype() {
        use std::ffi::OsString;

        #[derive(Clone, Copy)]
        enum Cls {
            Unset,
            Empty,
            NonUtf8,
            Utf8,
        }
        // Representative concrete value per class (None = the var is unset).
        let val = |c: Cls| match c {
            Cls::Unset => None,
            Cls::Empty => Some(""),
            Cls::NonUtf8 => Some("C"),
            Cls::Utf8 => Some("en_US.UTF-8"),
        };
        // Independent codeset check (different impl than `is_utf8_locale`).
        let looks_utf8 = |s: &str| {
            let lo = s.to_ascii_lowercase();
            lo.ends_with(".utf-8") || lo.ends_with(".utf8")
        };
        // Effective LC_CTYPE of a composed child env (POSIX precedence, empty==unset).
        let child_ctype_utf8 = |env: &[(OsString, OsString)]| -> bool {
            let get = |k: &str| {
                env.iter()
                    .find(|(ek, _)| ek.to_str() == Some(k))
                    .map(|(_, v)| v.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
            };
            match get("LC_ALL")
                .or_else(|| get("LC_CTYPE"))
                .or_else(|| get("LANG"))
            {
                None => false, // "C" default
                Some(loc) => looks_utf8(&loc),
            }
        };

        let classes = [Cls::Unset, Cls::Empty, Cls::NonUtf8, Cls::Utf8];
        let mut checked = 0u32;
        for &a in &classes {
            for &c in &classes {
                for &l in &classes {
                    // Build the inherited env the child would have started from.
                    let mut inherited: Vec<(OsString, OsString)> =
                        vec![(OsString::from("PATH"), OsString::from("/usr/bin"))];
                    for (k, cl) in [("LC_ALL", a), ("LC_CTYPE", c), ("LANG", l)] {
                        if let Some(v) = val(cl) {
                            inherited.push((OsString::from(k), OsString::from(v)));
                        }
                    }

                    let overrides = resolve_spawn_locale(val(a), val(c), val(l));

                    // INDEPENDENT "was the inherited effective locale already UTF-8?"
                    let ne = |cl: Cls| val(cl).filter(|s| !s.is_empty());
                    let orig_utf8 = ne(a).or(ne(c)).or(ne(l)).map(looks_utf8).unwrap_or(false);

                    // No-clobber & always-fix: overrides are empty IFF already UTF-8.
                    assert_eq!(
                        overrides.is_empty(),
                        orig_utf8,
                        "overrides emptiness must track already-UTF-8 for (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?})",
                        val(a),
                        val(c),
                        val(l)
                    );

                    // A dominating non-UTF-8 LC_ALL must be neutralized, else the
                    // LC_CTYPE override would be powerless.
                    if matches!(a, Cls::NonUtf8) {
                        assert!(
                            overrides.iter().any(|(k, v)| k == "LC_ALL" && v.is_empty()),
                            "non-UTF-8 LC_ALL must be neutralized; got {overrides:?}"
                        );
                    }

                    // THE INVARIANT: the child the terminal spawns is UTF-8.
                    let child = build_child_env(inherited.into_iter(), &overrides);
                    assert!(
                        child_ctype_utf8(&child),
                        "child LC_CTYPE NOT UTF-8 for inherited (LC_ALL,LC_CTYPE,LANG)=({:?},{:?},{:?}); env={:?}",
                        val(a),
                        val(c),
                        val(l),
                        child
                    );
                    checked += 1;
                }
            }
        }
        assert_eq!(
            checked, 64,
            "all 4x4x4 inherited-locale shapes must be exercised"
        );
    }

    // ---- read() syscall wrapper: EOF and bad-fd error contract ----

    // EOF: when the write end of a pipe is closed and the buffer is drained, a
    // `read` of the read end returns exactly 0 (not negative, not a partial-read
    // surprise). This is the `0 = EOF` half of the documented `read` contract.
    #[test]
    fn read_returns_zero_on_eof_after_write_end_closed() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);
        // Close the only write end with no data pending => the next read sees EOF.
        // SAFETY: `wr` is the pipe write end we just opened.
        unsafe {
            libc::close(wr);
        }
        let mut buf = [0u8; 16];
        let n = read(rd, &mut buf);
        assert_eq!(n, 0, "read at EOF must return 0, got {n}");
        // SAFETY: closing the read end we opened.
        unsafe {
            libc::close(rd);
        }
    }

    // Error: a `read` of an invalid descriptor must return a negative value (the
    // `< 0 = error` half of the contract), with `errno == EBADF`. We use fd -1,
    // which is never a valid descriptor, so this is hermetic and deterministic and
    // never touches a real, possibly-open fd. (We assert the raw errno, not
    // `ErrorKind`, because libstd categorizes EBADF as `Uncategorized` here — the
    // stable contract is the negative return + the POSIX errno, not the kind.)
    #[test]
    fn read_returns_negative_with_ebadf_on_invalid_fd() {
        let mut buf = [0u8; 16];
        let n = read(-1, &mut buf);
        assert!(n < 0, "read on a bad fd must be negative, got {n}");
        let err = io::Error::last_os_error();
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EBADF),
            "read on a bad fd must set errno=EBADF, got {err}",
        );
        // And EBADF is NOT EINTR, so the read loop would STOP (not spin-retry) on it
        // — the very decision the classifier encodes.
        assert_ne!(err.kind(), io::ErrorKind::Interrupted);
    }

    // ---- write_all drains a buffer larger than one pipe write (partial writes) ----

    // A pipe's kernel buffer is finite (typically 16–64 KiB), so a single
    // `write(2)` of a buffer larger than the pipe capacity CANNOT move all the
    // bytes at once: the kernel returns a short count and `write_all` must loop to
    // drain the remainder. A dedicated reader thread keeps draining so the writer
    // never blocks forever; we assert the bytes arrive byte-for-byte, in order,
    // for the full payload. This exercises the real `Advance(n)` short-write path
    // of `write_all` on a live fd (not just the pure classifier).
    #[test]
    fn write_all_drains_payload_larger_than_one_pipe_write() {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid 2-element buffer for `pipe`.
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);

        // 1 MiB — far larger than any pipe buffer, so >=1 short write is forced.
        // A deterministic, position-dependent pattern catches reorder/drop bugs.
        let n_bytes = 1usize << 20;
        let payload: Vec<u8> = (0..n_bytes).map(|i| (i % 251) as u8).collect();

        // Drain thread: read the read end to completion (until EOF) and return what
        // it saw. It must run concurrently with the writer or the pipe deadlocks.
        let reader = std::thread::spawn(move || {
            // Clamp the pre-size HINT (advisory; the Vec grows on demand) so
            // the bulk allocation carries a provable bound for the L0 gate.
            let mut got = Vec::with_capacity(n_bytes.min(1 << 20));
            let mut chunk = [0u8; 8192];
            loop {
                let r = read(rd, &mut chunk);
                if r <= 0 {
                    break; // 0 = EOF (writer closed), <0 = error
                }
                got.extend_from_slice(&chunk[..r as usize]);
            }
            // SAFETY: closing the read end this thread owns.
            unsafe {
                libc::close(rd);
            }
            got
        });

        write_all(wr, &payload);
        // Close the write end so the reader observes EOF and the thread joins.
        // SAFETY: `wr` is the write end this thread owns after `write_all`.
        unsafe {
            libc::close(wr);
        }

        let got = reader.join().expect("reader thread panicked");
        assert_eq!(got.len(), payload.len(), "drained byte count mismatch");
        assert!(
            got == payload,
            "drained bytes differ from the payload byte-for-byte"
        );
    }

    // ---- fail-closed spawn: under-tier capability is denied WITHOUT forking ----

    // An under-tier `Cap<Spawn>` (Untrusted, below the required Trusted) must be
    // rejected by the PARENT gate BEFORE any `forkpty` — there must be no way to
    // spawn a child with an insufficient capability. We assert PermissionDenied;
    // the absence of a leaked child is implicit (no fork happened, so there is
    // nothing to reap), and the error originates from `aterm_cap::require`, not
    // from a child status byte.
    #[test]
    fn under_tier_spawn_cap_is_denied_before_forking() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        // Untrusted spawn cap: below the Trusted floor `spawn_shell` requires.
        let weak_spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Untrusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        let result = spawn_shell(
            24,
            80,
            &weak_spawn,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            None,
            None,
            None,
        );
        let err = result.expect_err("an under-tier spawn cap must be denied, not spawn a shell");
        assert_eq!(
            err.kind(),
            io::ErrorKind::PermissionDenied,
            "under-tier spawn must be PermissionDenied, got: {err}",
        );
    }

    // ---- fail-closed spawn: a child that cannot exec takes the _exit(127) path ----

    // The exec-failure path through the REAL production code: a `-e` command naming
    // a nonexistent absolute program forces the child's `execve` to fail, so the
    // child writes the b'E' status byte and `_exit(127)`s. The parent reads that
    // byte off the status pipe, reaps the child internally, and surfaces an
    // `io::Error` (ErrorKind::Other) describing the pre-exec exec failure — never a
    // master fd. This drives a real `forkpty` + the full status-pipe protocol.
    //
    // NOTE on "$SHELL in the child": `spawn_shell` resolves the exec target in the
    // PARENT (it must, to stay async-signal-safe in the child), so a bogus `$SHELL`
    // can only be injected by mutating the parent's env — which is a data race
    // against the multi-threaded test harness under edition 2024. We therefore
    // drive the SAME child exec-failure path hermetically via a bogus `exec_command`
    // (no env mutation). The raw 127 exit code is consumed by `spawn_shell`'s own
    // `waitpid` reap, so it is not observable here; the contract that exit code 127
    // is what a bogus `execve` yields is locked by the sibling test below.
    #[test]
    fn bogus_exec_command_takes_child_exec_failure_path() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        // An absolute path that cannot exist => `resolve_program` returns it
        // verbatim => the child's `execve` fails => b'E' + _exit(127).
        let bogus = vec![String::from("/nonexistent/aterm-pty-no-such-prog-xyz")];
        let result = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&bogus),
            None,
            None,
        );
        let err =
            result.expect_err("a child that cannot exec must surface an error, not a master fd");
        assert_eq!(
            err.kind(),
            io::ErrorKind::Other,
            "exec failure before exec must be reported as Other, got: {err}",
        );
        assert!(
            err.to_string().contains("127"),
            "error should describe the _exit(127) exec failure: {err}",
        );
    }

    // Contract lock for the exit code the design depends on: a child that writes a
    // status byte and `_exit(127)`s after a failed `execve` is reaped by the parent
    // with the WEXITSTATUS == 127 the spawn protocol claims. This mirrors the exact
    // child syscall shape of `spawn_shell` (status pipe + write byte + _exit), using
    // a real `forkpty`, and ASSERTS the raw exit code — which `spawn_shell` itself
    // consumes during its internal reap, so it cannot be observed through that API.
    // It is a contract test of the OS primitive, NOT a re-implementation of product
    // logic: it locks "bogus execve => _exit(127), reapable" so a future change to
    // the child's exit code would be caught here.
    #[test]
    // Linux-only: this contract test forks the PROCESS with `forkpty` and runs the
    // child to `execve`/`_exit` INSIDE the libtest harness. On Linux the harness's
    // threaded runtime does not survive a raw fork — strace shows the child
    // deterministically `exit_group(1)`ing before its `execve` ever runs (true for
    // both `libc::execve` and a raw `SYS_execve`), so the child's exit code is the
    // harness's, not the test's. This is a harness↔fork incompatibility, NOT a
    // product defect: the SAME execve-failure → 127 contract on the real spawn path
    // is verified by `bogus_exec_command_takes_child_exec_failure_path` (which drives
    // `spawn_shell` and passes), and the live GUI spawn works. macOS's harness
    // tolerates the fork, so the raw-primitive lock still runs there.
    #[cfg_attr(
        target_os = "linux",
        ignore = "forkpty inside the libtest harness can't run the child to exec on Linux; \
                  the execve→127 contract is covered by bogus_exec_command_takes_child_exec_failure_path"
    )]
    fn child_exec_failure_exit_code_is_127_and_reapable() {
        let mut master: libc::c_int = -1;
        let ws = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid out-param for the master fd, null for the unused name/termios
        // buffers, and a valid winsize; returns the child pid (parent) or 0 (child).
        let pid = unsafe {
            libc::forkpty(
                &mut master,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::addr_of!(ws).cast_mut(),
            )
        };
        assert!(pid >= 0, "forkpty failed: {}", io::Error::last_os_error());
        if pid == 0 {
            // CHILD — async-signal-safe only: attempt to exec a nonexistent program
            // (mirroring the child's `execve`), then take the _exit(127) failure
            // path exactly as `spawn_shell`'s child does.
            // SAFETY: a NUL-terminated absolute path; on `execve` failure we _exit.
            unsafe {
                let prog = b"/nonexistent/aterm-pty-no-such-prog-xyz\0";
                let argv: [*const libc::c_char; 2] =
                    [prog.as_ptr().cast::<libc::c_char>(), ptr::null()];
                let envp: [*const libc::c_char; 1] = [ptr::null()];
                libc::execve(
                    prog.as_ptr().cast::<libc::c_char>(),
                    argv.as_ptr(),
                    envp.as_ptr(),
                );
                libc::_exit(127);
            }
        }
        // PARENT: reap the child and assert the exit code.
        // SAFETY: `master` is the forkpty master; closing it tears the child's tty.
        unsafe {
            libc::close(master);
        }
        let mut wstatus: libc::c_int = 0;
        // SAFETY: reaping the child we just forked; `wstatus` is a valid out-param.
        let w = unsafe { libc::waitpid(pid, &mut wstatus, 0) };
        assert_eq!(w, pid, "waitpid did not reap our child");
        assert!(
            libc::WIFEXITED(wstatus),
            "child did not exit normally: {wstatus}"
        );
        assert_eq!(
            libc::WEXITSTATUS(wstatus),
            127,
            "a failed execve child must _exit(127)",
        );
    }

    // ---- OS-sandbox wrap (sandbox_wrap) ----

    // The seam's inlined wrapper path MUST be the SAME bytes as the policy crate's
    // canonical SANDBOX_EXEC_PATH. They are kept in lockstep by hand (the seam
    // stays dependency-light), so this test fails loudly if either drifts.
    #[test]
    fn inlined_sandbox_exec_path_matches_policy_crate() {
        assert_eq!(SANDBOX_EXEC_PATH, aterm_containment::SANDBOX_EXEC_PATH);
        assert_eq!(SANDBOX_EXEC_PATH, "/usr/bin/sandbox-exec");
    }

    // FAIL-CLOSED: when the wrapper binary is absent at the given path,
    // build_sandbox_wrap returns NotFound — the caller (`spawn_shell`) propagates
    // it and NEVER forks, so a policy-demanded sandbox that can't be applied
    // refuses to spawn rather than silently running an unsandboxed shell. We point
    // it at a guaranteed-nonexistent path to drive this without disturbing the real
    // /usr/bin/sandbox-exec.
    #[test]
    fn build_sandbox_wrap_fails_closed_when_wrapper_missing() {
        let prog = CString::new("/bin/zsh").unwrap();
        let argv = vec![CString::new("-zsh").unwrap()];
        let err = build_sandbox_wrap(
            "/nonexistent/aterm-no-such-sandbox-exec",
            aterm_containment::NETWORK_DENY_PROFILE,
            &prog,
            &argv,
        )
        .expect_err("a missing wrapper must fail closed, not silently skip the sandbox");
        assert_eq!(
            err.kind(),
            io::ErrorKind::NotFound,
            "fail-closed kind: {err}"
        );
        assert!(
            err.to_string().contains("fail-closed"),
            "error must describe the fail-closed refusal: {err}",
        );
    }

    // The wrapped argv has the exact shape the kernel needs: sandbox-exec, -p,
    // <profile>, <program-path>, then the original args AFTER argv[0]. The login
    // argv[0] ("-zsh") is replaced by the program PATH; "--rcfile FILE" style real
    // args are carried through verbatim. Uses the REAL /usr/bin/sandbox-exec path
    // (present on macOS) so the access() probe passes.
    #[cfg(target_os = "macos")]
    #[test]
    fn build_sandbox_wrap_produces_correct_argv_shape() {
        let prog = CString::new("/bin/zsh").unwrap();
        // Original argv: a login-shell argv[0] plus a real flag+value pair.
        let argv = vec![
            CString::new("-zsh").unwrap(),
            CString::new("--rcfile").unwrap(),
            CString::new("/tmp/rc").unwrap(),
        ];
        let (target, wrapped) = build_sandbox_wrap(
            SANDBOX_EXEC_PATH,
            aterm_containment::NETWORK_DENY_PROFILE,
            &prog,
            &argv,
        )
        .expect("wrapper present → build succeeds");
        assert_eq!(target.to_str().unwrap(), SANDBOX_EXEC_PATH);
        let got: Vec<&str> = wrapped.iter().map(|c| c.to_str().unwrap()).collect();
        assert_eq!(
            got,
            vec![
                "sandbox-exec",
                "-p",
                aterm_containment::NETWORK_DENY_PROFILE,
                "/bin/zsh", // argv[0] replaced by the program PATH
                "--rcfile", // real args carried through verbatim …
                "/tmp/rc",  // …
            ],
            "wrapped argv shape must be sandbox-exec -p <sbpl> <prog> <orig argv[1..]>",
        );
    }

    // Default (no-wrap) spawn is byte-identical: passing `sandbox_wrap = None` must
    // NOT change the exec target — it stays `$SHELL`, never `sandbox-exec`. We
    // assert this through the SAME `-e` echo path used elsewhere: with no wrap, a
    // `-e /bin/echo MARKER` runs `/bin/echo` directly (argv[0] == the program), so
    // the PTY shows exactly "MARKER" with no sandbox-exec banner/argv mutation.
    #[test]
    fn no_wrap_spawn_runs_program_directly_unchanged() {
        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let cmd = vec![
            String::from("/bin/echo"),
            String::from("ATERM-NOWRAP-MARKER"),
        ];
        // sandbox_wrap = None → no wrap, byte-identical spawn.
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&cmd),
            None,
            None,
        )
        .expect("unwrapped -e command must spawn");
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        for _ in 0..50 {
            let n = read(master, &mut buf);
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
            if out
                .windows(b"ATERM-NOWRAP-MARKER".len())
                .any(|w| w == b"ATERM-NOWRAP-MARKER")
            {
                break;
            }
        }
        // SAFETY: tear down the child.
        unsafe {
            libc::close(master);
        }
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains("ATERM-NOWRAP-MARKER"),
            "echo output not seen: {s:?}"
        );
        assert!(
            !s.contains("sandbox-exec"),
            "no-wrap spawn must NOT involve sandbox-exec: {s:?}",
        );
    }

    // The wrap path is well-formed AND actually applies Seatbelt: wrap a `-e`
    // command in the real `(deny network*)` profile and run `/usr/bin/nc` against a
    // live loopback listener bound in this parent. WITHOUT the wrap nc connects;
    // WITH the wrap the kernel denies network so nc cannot connect — observed via
    // the child's exit code (the wrapped sandbox-exec→nc child fails). This drives
    // the REAL `spawn_shell` wrap-argv construction end to end, not just a direct
    // sandbox-exec call.
    #[cfg(target_os = "macos")]
    #[test]
    fn wrapped_spawn_enforces_network_deny_via_seatbelt() {
        use std::io::Write;
        use std::net::TcpListener;

        // SAFETY: single-threaded test, trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

        // Loopback listener in the parent + a draining accept thread.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let accepter = std::thread::spawn(move || {
            for _ in 0..2 {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.write_all(b"x");
                    }
                    Err(_) => break,
                }
            }
        });
        let port_s = port.to_string();

        // Control: unwrapped `-e nc` to the listener CONNECTS (so the probe works).
        // We can't read nc's exit code through spawn_shell's API, so prove the
        // control via a direct connect from the parent instead, then focus the
        // wrapped assertion on the seam producing a sandbox-exec'd child that the
        // kernel network-denies (nc fails → its PTY closes quickly with no data
        // that looks like a successful connect).
        let probe = std::net::TcpStream::connect(("127.0.0.1", port));
        assert!(
            probe.is_ok(),
            "loopback listener must be connectable (probe)"
        );
        drop(probe);

        // Wrapped `-e nc` under (deny network*). The wrap is built by spawn_shell:
        // sandbox-exec -p <profile> /usr/bin/nc <args>. The connect is denied.
        let nc = vec![
            String::from("/usr/bin/nc"),
            String::from("-G"),
            String::from("1"),
            String::from("-w"),
            String::from("1"),
            String::from("-z"),
            String::from("127.0.0.1"),
            port_s.clone(),
        ];
        let profile = aterm_containment::NETWORK_DENY_PROFILE;
        let master = spawn_shell(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None, // shell_override
            None, // shell_args
            None,
            Some(&nc),
            None,
            Some(profile),
        )
        .expect("wrapped -e nc must spawn (sandbox-exec applies the profile)");
        // Drain to EOF (the child exits fast: nc's connect is denied). The success
        // banner "succeeded!" must NOT appear — a denied connect never prints it.
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        for _ in 0..200 {
            let n = read(master, &mut buf);
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        // SAFETY: tear down the child.
        unsafe {
            libc::close(master);
        }
        let _ = std::net::TcpStream::connect(("127.0.0.1", port)); // unblock accepter
        let _ = accepter.join();
        let s = String::from_utf8_lossy(&out);
        assert!(
            !s.contains("succeeded"),
            "DENY FAILED: wrapped nc reported a successful connect under (deny network*): {s:?}",
        );
    }
}

/// Tier-1 trace conformance: bind the REAL `write_all` drain loop to the external
/// `WriteAll.tla` design spec (TRUST_NATIVE_TLA Phase 2, I/O DURABILITY family).
///
/// `WriteAll.tla` is model-checked in the abstract by aterm-spec-models'
/// `model_check.rs` (Tier-0: proves the loop reports completion ONLY when `off =
/// Size`, and catches the dropped-tail bug at `Buggy=TRUE`), but nothing tied it to
/// the code that runs. This test closes that gap two ways:
///
///   1. END-TO-END over a REAL pipe: drive `write_all` with a payload far larger
///      than the pipe buffer and a reader that drains it, proving the real loop
///      delivers EVERY byte (the spec's `NoSilentDrop` / `off = Size` exit) even
///      across the genuine short writes a small pipe forces.
///   2. PER-TRANSITION against `ty`: replay the loop's offset trajectory using the
///      REAL [`classify_write_result`] decision over synthetic `write(2)` returns
///      (`Advance(n)` advances `off`; `Retry`/`Stop` do not), and `ty trace
///      validate --spec` each `(off,done) -> (off',done')` step against `WriteAll`'s
///      `Next`. Because `WriteAll` is multi-transition (off climbs over several
///      steps) and `ty` strictly checks only `Init` + the FIRST transition, we pin
///      `Init` to each step's predecessor via a PARAMETERIZED variant of the
///      COMMITTED spec (mechanical `Init`/`CONSTANT` rewrite — every action and
///      invariant body is the committed text verbatim, so it cannot drift). A
///      NEGATIVE control (claim `done` while `off < Size` — the dropped tail) MUST
///      be ty-REJECTED, so a pass is never vacuous.
///
/// `ty` is located by the same fixed canonical path search. VERIFICATION GATE
/// (honesty ratchet, batteries-on, see [`aterm_spec::verify`]): verification is always
/// required — an absent Trust `ty` FAILS the test with a build hint (`cargo build
/// --release -p tla-cli` in $HOME/trust/first-party/ty).
#[cfg(test)]
mod writeall_conformance {
    use super::{WriteStep, classify_write_result, write_all};
    use aterm_spec::verify::ty_escalation;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The bounded buffer length the conformance trajectory uses (matches the cfg
    /// `Size` we pin below). Small so the ty runs are cheap.
    const SIZE: i64 = 5;

    /// The committed `WriteAll.tla`, with `Init` PARAMETERIZED (`off = off_init /\
    /// done = done_init`) so any predecessor state can be the strict first step. The
    /// rewrite touches ONLY the `CONSTANTS` line (adds `off_init, done_init`) and the
    /// `Init ==` line; every `Progress`/`Interrupted`/`Next`/invariant line is the
    /// committed text verbatim, so the actions cannot drift from the checked spec.
    fn parameterized_spec() -> String {
        let committed =
            std::fs::read_to_string(spec_path("WriteAll.tla")).expect("read WriteAll.tla");
        let mut out = String::new();
        for line in committed.lines() {
            let t = line.trim_start();
            if t.starts_with("CONSTANTS Size, Buggy") {
                out.push_str("CONSTANTS Size, Buggy, off_init, done_init\n");
            } else if t.starts_with("Init ==") {
                out.push_str("Init == off = off_init /\\ done = done_init\n");
            } else if t.starts_with("/\\ off = 0") || t.starts_with("/\\ done = FALSE") {
                // The two old Init conjunct lines — dropped (subsumed by the rewrite).
                continue;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    fn spec_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("aterm-spec-models/specs")
            .join(name)
    }

    fn transition_trace(prev: (i64, bool), next: (i64, bool), action: &str) -> String {
        let st = |off: i64, done: bool| {
            format!(
                "{{\"off\":{{\"type\":\"int\",\"value\":{off}}},\"done\":{{\"type\":\"bool\",\"value\":{done}}}}}"
            )
        };
        format!(
            "{{\"version\":\"1\",\"module\":\"WriteAll\",\"variables\":[\"off\",\"done\"],\"steps\":[\
             {{\"index\":0,\"state\":{}}},\
             {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"{}\"}}}}\
             ]}}",
            st(prev.0, prev.1),
            st(next.0, next.1),
            action
        )
    }

    fn validate(
        ty: &Path,
        dir: &Path,
        spec: &str,
        prev: (i64, bool),
        next: (i64, bool),
        action: &str,
    ) -> (bool, String) {
        let spec_f = dir.join("WriteAll.tla");
        let cfg_f = dir.join("WriteAll.cfg");
        let trace_f = dir.join("t.json");
        std::fs::write(&spec_f, spec).expect("write spec");
        std::fs::write(
            &cfg_f,
            format!(
                "CONSTANT Size = {SIZE}\nCONSTANT Buggy = FALSE\n\
                 CONSTANT off_init = {}\nCONSTANT done_init = {}\n\
                 SPECIFICATION Spec\nCHECK_DEADLOCK FALSE\n",
                prev.0,
                if prev.1 { "TRUE" } else { "FALSE" } // TLA+ booleans are UPPERCASE
            ),
        )
        .expect("write cfg");
        std::fs::write(&trace_f, transition_trace(prev, next, action)).expect("write trace");
        let out = Command::new(ty)
            .arg("trace")
            .arg("validate")
            .arg(&trace_f)
            .arg("--spec")
            .arg(&spec_f)
            .arg("--config")
            .arg(&cfg_f)
            .output()
            .expect("run ty trace validate");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), combined)
    }

    /// Replay the REAL `write_all` loop's `off`/`done` trajectory over a scripted
    /// sequence of `write(2)` returns, using the REAL [`classify_write_result`]
    /// decision and the REAL `data = &data[n..]` advance. Returns the sequence of
    /// `(off, done, action_label)` transitions the loop takes. `done` is set on the
    /// step that exhausts the buffer (`data.is_empty()`), exactly as `write_all`'s
    /// `while !data.is_empty()` exit reports completion.
    fn replay(returns: &[(isize, bool)]) -> Vec<(i64, bool, &'static str)> {
        let total = SIZE;
        let mut off: i64 = 0;
        let mut steps = Vec::new();
        for &(r, is_eintr) in returns {
            if off >= total {
                break; // loop already exited (done)
            }
            match classify_write_result(r, is_eintr) {
                WriteStep::Retry => {
                    // EINTR: off unchanged, not done — the spec's `Interrupted` with k=0.
                    steps.push((off, false, "Interrupted"));
                }
                WriteStep::Stop => break, // peer closed / real error — loop ends, not "done"
                WriteStep::Advance(n) => {
                    off += n as i64;
                    let done = off >= total;
                    // A step that finishes the buffer is `Progress`; a short write that
                    // does NOT finish is `Interrupted` (advance-but-not-complete).
                    let label = if done { "Progress" } else { "Interrupted" };
                    steps.push((off, done, label));
                }
            }
        }
        steps
    }

    #[test]
    fn real_write_all_drains_a_large_payload_over_a_pipe() {
        // END-TO-END: a payload far larger than a pipe buffer + a draining reader.
        // The real `write_all` MUST deliver every byte — `NoSilentDrop` end-to-end.
        let mut fds = [0i32; 2];
        // SAFETY: valid 2-element out-array for pipe(2).
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe() failed");
        let (rd, wr) = (fds[0], fds[1]);
        let payload: Vec<u8> = (0..(1 << 20)).map(|i| (i % 251) as u8).collect(); // 1 MiB > pipe buf
        let expect = payload.clone();
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: rd is a valid pipe read end; buf is a valid 4096-byte buffer.
                let n = unsafe { libc::read(rd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n as usize]);
            }
            // SAFETY: own pipe read end; close once drained.
            unsafe { libc::close(rd) };
            got
        });
        write_all(wr, &payload);
        // SAFETY: own pipe write end; closing signals EOF to the reader.
        unsafe { libc::close(wr) };
        let got = reader.join().expect("reader thread");
        assert_eq!(
            got.len(),
            expect.len(),
            "write_all dropped bytes — NoSilentDrop violated"
        );
        assert_eq!(got, expect, "write_all delivered corrupted/reordered bytes");
    }

    #[test]
    fn real_write_all_offset_trajectory_conforms_to_writeall_spec() {
        // TIERED (VERIFY-1): a committed hand-written `.tla` — external-tool
        // obligation; runs only where the Trust toolchain is installed.
        let Some(ty) = ty_escalation("WriteAll conformance") else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("aterm-writeall-conf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mk tempdir");
        let spec = parameterized_spec();

        // A scripted run of `write(2)` returns the real loop classifies+advances:
        //   short write (+2), EINTR retry (0 advance), short write (+1), full tail (+2).
        // Total = SIZE (5). This exercises BOTH spec actions: `Interrupted` (short
        // writes and the EINTR) and `Progress` (the final completing write).
        let returns = [(2isize, false), (-1, true), (1, false), (2, false)];
        let steps = replay(&returns);
        assert_eq!(
            steps.last().map(|s| (s.0, s.1)),
            Some((SIZE, true)),
            "loop must finish at off=Size, done"
        );

        // POSITIVE: each real transition strictly conforms to WriteAll's `Next`.
        let mut prev = (0i64, false);
        let mut validated = 0usize;
        for &(off, done, action) in &steps {
            let next = (off, done);
            let (ok, out) = validate(&ty, &dir, &spec, prev, next, action);
            assert!(
                ok,
                "real {action} transition {prev:?} -> {next:?} must conform to WriteAll\n--- ty ---\n{out}"
            );
            prev = next;
            validated += 1;
        }

        // NEGATIVE CONTROL — the dropped-tail bug: a `Progress` step that claims
        // `done` while `off < Size`. `NoSilentDrop` forbids it; ty MUST reject.
        let (bad_ok, o) = validate(&ty, &dir, &spec, (1, false), (3, true), "Progress");
        assert!(
            !bad_ok,
            "NEGATIVE CONTROL (done with off=3 < Size=5 — dropped tail) MUST be rejected\n--- ty ---\n{o}"
        );

        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "WriteAll Tier-1 conformance: {validated} real loop transitions (short writes + EINTR + \
             completing write) strictly validated against committed WriteAll.tla; dropped-tail \
             negative control rejected."
        );
    }
}
