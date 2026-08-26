// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![deny(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]
// Trust verification tool attribute (`#[trust::skip]`) registration, mirroring
// aterm-log / aterm-types. Active only under the `trust_verify` cfg.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Shell integration injection for aterm.
//!
//! Embeds shell integration scripts (zsh, bash, fish, PowerShell) in the
//! Rust binary and provides a cross-platform injection mechanism that
//! auto-loads them at shell startup without requiring user configuration.
//!
//! # Injection Strategies
//!
//! Each shell has its own auto-loading mechanism:
//!
//! | Shell | Mechanism | How |
//! |-------|-----------|-----|
//! | zsh   | ZDOTDIR override | Wrapper `.zshenv` sources user config then ours |
//! | bash  | `--rcfile` | Wrapper rcfile sources profiles then ours |
//! | fish  | `XDG_DATA_DIRS` | Vendor conf.d auto-loading |
//! | pwsh/powershell | `-NoExit -Command` | Argv override dot-sources our `.ps1` after profiles |
//! | wsl   | `WSLENV` + `wsl.exe --exec` | `/p` path-translates our dir, then bash's `--rcfile` runs INSIDE the distro |
//! | cmd   | `PROMPT` | `$e` OSC 133 A/B + OSC 633 `Cwd=` woven around the user's prompt |
//!
//! # Usage
//!
//! ```rust,no_run
//! use aterm_shell_integration::{ShellType, prepare};
//!
//! let shell = ShellType::detect("/bin/zsh");
//! if let Ok(Some(injection)) = prepare(shell) {
//!     // Add injection.env_add to SpawnConfig.env before fork
//!     // Use injection.argv_override if Some (bash --rcfile)
//! }
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Embedded shell integration scripts (compiled into the binary).
///
/// `aterm-core` is the canonical owner of the shell script bodies. The macOS
/// app bundle ships byte-identical copies in
/// `apps/aterm-mac/Sources/ATermMac/Resources/ShellIntegration/`, and the
/// shell-integration test module enforces that parity so cross-consumer drift
/// fails in Rust tests instead of shipping silently.
pub mod scripts {
    /// zsh shell integration (OSC 7/133 + prompt override).
    pub const ZSH: &str = include_str!("scripts/aterm_shell_integration.zsh");
    /// bash shell integration (OSC 7/133 + prompt override).
    pub const BASH: &str = include_str!("scripts/aterm_shell_integration.bash");
    /// fish shell integration (OSC 7/133 + prompt override).
    pub const FISH: &str = include_str!("scripts/aterm_shell_integration.fish");
    /// PowerShell / pwsh shell integration (OSC 7/133; no macOS bundle
    /// counterpart — the Windows/pwsh path ships from the Rust binary only).
    pub const POWERSHELL: &str = include_str!("scripts/aterm_shell_integration.ps1");
}

/// Shell type detected from the command path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShellType {
    /// Zsh (injected via ZDOTDIR override).
    Zsh,
    /// Bash (injected via --rcfile wrapper).
    Bash,
    /// Fish (injected via XDG_DATA_DIRS vendor conf.d).
    Fish,
    /// PowerShell / pwsh (injected via `-NoExit -Command` argv override
    /// that dot-sources the embedded `.ps1` after profiles load).
    PowerShell,
    /// `wsl.exe` — the Windows front door to a Linux distro. The shell that
    /// ends up running is a LINUX bash, so the injected script is the bash one;
    /// the Windows→Linux boundary is crossed by `WSLENV` (see [`prepare_wsl`]).
    Wsl,
    /// Windows `cmd.exe`. cmd has no preexec hook, so this is a PARTIAL
    /// integration: prompt marks and cwd, woven into `%PROMPT%` (see
    /// [`prepare_cmd`]). Jump-to-prompt and cwd tracking work; the blocks it
    /// produces carry no command text and no exit code.
    Cmd,
    /// Unknown shell (no injection available).
    Unknown,
}

impl ShellType {
    /// Detect shell type from a command path (e.g. "/bin/zsh", "bash",
    /// `C:\Program Files\PowerShell\7\pwsh.exe`).
    ///
    /// Matching is case-insensitive and ignores a trailing `.exe`, so the
    /// resolved Windows shell program (`pwsh.exe`, `PowerShell.EXE`, ...)
    /// detects correctly.
    #[must_use]
    pub fn detect(shell_path: &str) -> Self {
        // BOTH separators, on every host. `Path::file_name` splits on the
        // separator of the platform doing the LOOKING, so on Linux a Windows
        // program path — `C:\Windows\System32\wsl.exe`, which is exactly what
        // the WSL and cmd aliases resolve to — contains no `/` and comes back
        // whole, and every match below misses. The shell path is data (a config
        // value, a remote handoff, a test fixture), not a fact about this host,
        // so the split is spelled for both.
        let tail = shell_path.rsplit(['/', '\\']).next().unwrap_or(shell_path);
        let name = tail.to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "zsh" => Self::Zsh,
            "bash" | "bash5" => Self::Bash,
            "fish" => Self::Fish,
            "pwsh" | "powershell" => Self::PowerShell,
            // The `shell = "wsl"` / `"cmd"` aliases the Windows PTY seam already
            // resolves first-class (`aterm_pty::windows::shell::discover_shell`).
            // Before these arms both fell to `Unknown`, `prepare()` injected
            // NOTHING, and every OSC 133 consumer — jump-to-prompt, command
            // blocks, `blocks`/`wait`, cwd inherit — was silently dead in a WSL
            // or cmd tab even though the tab looked like any other.
            "wsl" => Self::Wsl,
            "cmd" => Self::Cmd,
            _ => Self::Unknown,
        }
    }

    /// Detect the interactive shell aterm will launch.
    ///
    /// Unix / git-bash: `$SHELL`. Native Windows never sets `$SHELL` (and under an
    /// inherited git-bash env it holds a POSIX path `CreateProcessW` can't exec),
    /// so there we mirror the PTY seam's `select_shell()`: an `ATERM_SHELL` override,
    /// else PowerShell — the shell aterm actually spawns (`pwsh`/`powershell`, both in
    /// System32, resolve before any `cmd` fallback). Returning PowerShell here is what
    /// makes the `-ExecutionPolicy Bypass` + OSC 7/133 injection reach the spawned
    /// shell; the previous `$SHELL`-only body returned `Unknown` on Windows, so NOTHING
    /// was injected and a policy-restricted box failed with "running scripts is disabled".
    #[must_use]
    pub fn detect_current() -> Self {
        #[cfg(not(windows))]
        {
            match std::env::var("SHELL") {
                Ok(shell) => Self::detect(&shell),
                Err(_) => Self::Unknown,
            }
        }
        #[cfg(windows)]
        {
            if let Some(sh) = std::env::var_os("ATERM_SHELL").filter(|s| !s.is_empty()) {
                return Self::detect(&sh.to_string_lossy());
            }
            Self::PowerShell
        }
    }
}

/// Result of preparing shell integration injection.
///
/// Contains environment variable modifications to apply to the child
/// process before exec.
#[derive(Debug)]
pub struct InjectionEnv {
    /// Environment variables to set in the child process.
    pub env_add: Vec<(String, String)>,
    /// For bash: override argv to use `--rcfile`. `None` for other shells.
    pub argv_override: Option<Vec<String>>,
}

/// Byte length of the shell-integration capability-nonce (#7960).
pub const SHELL_NONCE_BYTES: usize = 32;

/// Hex-encoded length of the shell-integration nonce (#7960).
pub const SHELL_NONCE_HEX_LEN: usize = SHELL_NONCE_BYTES * 2;

/// A freshly generated 32-byte CSPRNG nonce for OSC 133/633 gating (#7960, #7987).
///
/// Produced by [`generate_nonce`]. Carries both the raw bytes (for
/// `Terminal::authorize_shell_integration` in `aterm-core`) and the hex
/// encoding (for the `ATERM_SHELL_NONCE` child env var).
#[derive(Debug, Clone)]
pub struct ShellNonce {
    raw: [u8; SHELL_NONCE_BYTES],
    hex: String,
}

impl ShellNonce {
    /// Raw 32-byte nonce to pass to `Terminal::authorize_shell_integration`.
    #[must_use]
    pub const fn raw(&self) -> &[u8; SHELL_NONCE_BYTES] {
        &self.raw
    }

    /// 64-char lowercase hex encoding to set as `ATERM_SHELL_NONCE` in the
    /// child shell environment.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// Consume the nonce and return both halves. Callers typically use
    /// [`raw`](Self::raw) to authorize the terminal, then [`hex`](Self::hex)
    /// to inject into the child environment.
    #[must_use]
    pub fn into_parts(self) -> ([u8; SHELL_NONCE_BYTES], String) {
        (self.raw, self.hex)
    }
}

/// Generate a fresh 32-byte shell-integration capability-nonce (#7960, #7987).
///
/// Minted from the operating-system CSPRNG — see [`fill_nonce_entropy`] — so
/// the nonce is unpredictable across restarts. The host is responsible for:
///
/// 1. Installing the raw bytes via `Terminal::authorize_shell_integration`.
/// 2. Setting `ATERM_SHELL_NONCE=<hex>` in the spawned shell's environment
///    (see [`augment_with_nonce`]).
/// 3. Flipping `TerminalModes::require_shell_integration_nonce` on after
///    (1) and (2) are wired.
#[must_use]
// Trust: the fill bottoms out in an out-of-bundle syscall wrapper whose only panic
// path is an UNRECOVERABLE OS-entropy failure — a deliberate, documented fail-loud
// design choice (below): we PREFER that catastrophic-rare panic to silently weakening
// the nonce. Its panic-freedom is therefore a documented ASSUMPTION on the OS CSPRNG,
// not a provable property (the OS RNG can, in principle, fail), so this thin
// nonce-constructor takes `#[trust::skip]` responsibility for it — the same
// documented-external-assumption tier as the workspace's other skips.
#[cfg_attr(trust_verify, trust::skip)]
pub fn generate_nonce() -> ShellNonce {
    let mut raw = [0u8; SHELL_NONCE_BYTES];
    fill_nonce_entropy(&mut raw);
    let hex = hex_encode(&raw);
    ShellNonce { raw, hex }
}

/// Fill `buf` from the OS CSPRNG, on the ONE audited entropy surface for this
/// platform.
///
/// Native (unix and Windows) goes through [`aterm_uds::rand::fill`] —
/// `getentropy(2)` with a bounded `read_exact` fallback, `BCryptGenRandom` on
/// Windows. That is the rule `tools/grep_guard.sh` B4 enforces after the
/// 2026-07-04/05 unbounded-`/dev/urandom` kernel panic, and routing here is
/// what let `rand_core` leave the shipped graph.
///
/// `wasm32-unknown-unknown` has no OS entropy syscall and no `aterm-uds`
/// (there are no Unix-domain sockets in a browser), so it calls the JS
/// `crypto.getRandomValues` bridge directly — the same source `OsRng` reached
/// there, one indirection fewer.
///
/// # Panics
/// When the OS CSPRNG is unavailable. Deliberate and documented: a weaker
/// nonce would silently un-gate shell integration, so this fails loud.
#[cfg(any(unix, windows))]
fn fill_nonce_entropy(buf: &mut [u8; SHELL_NONCE_BYTES]) {
    aterm_uds::rand::fill(buf)
        .expect("OS CSPRNG unavailable: cannot mint a shell-integration nonce");
}

/// `wasm32-unknown-unknown` arm of [`fill_nonce_entropy`].
///
/// The predicate matches the manifest's
/// `[target.'cfg(all(target_arch = "wasm32", target_os = "unknown"))'.dependencies]`
/// section EXACTLY, not `not(any(unix, windows))`. The two must partition the
/// target space identically: a `not(any(unix, windows))` arm also selects
/// `wasm32-wasip1`, whose manifest section does not apply, so `getrandom` would
/// not be in the graph and the crate would fail to build on an unresolved path
/// rather than on a sentence.
///
/// # Panics
/// As the native arm: an unavailable CSPRNG fails loud rather than weakening
/// the nonce.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn fill_nonce_entropy(buf: &mut [u8; SHELL_NONCE_BYTES]) {
    getrandom::getrandom(buf)
        .expect("OS CSPRNG unavailable: cannot mint a shell-integration nonce");
}

// Any target that is neither unix, nor windows, nor wasm32-unknown-unknown has
// no entropy source declared in this crate's manifest. Say so in one sentence
// at compile time instead of leaving a reader to decode an unresolved
// `getrandom::` path — and make adding such a target a deliberate act that
// names its CSPRNG, since the alternative is a silently weaker nonce.
#[cfg(not(any(unix, windows, all(target_arch = "wasm32", target_os = "unknown"))))]
compile_error!(
    "aterm-shell-integration has no OS entropy source for this target: the capability-nonce \
     mint routes through aterm-uds on unix/windows and getrandom on wasm32-unknown-unknown. \
     Add a target section to Cargo.toml and an arm to fill_nonce_entropy before building here."
);

/// Lowercase hex-encode a 32-byte nonce. Exposed for host-side helpers
/// that wire a caller-provided nonce (e.g. test fixtures that want
/// deterministic bytes).
#[must_use]
pub fn hex_encode(bytes: &[u8; SHELL_NONCE_BYTES]) -> String {
    let mut out = String::with_capacity(SHELL_NONCE_HEX_LEN);
    // Trust: bind each byte BY VALUE (`&b` pattern) rather than shifting the
    // `&u8` loop binding. With `for b in bytes`, `b >> 4` / `b & 0x0F` lower
    // through the std reference-operator shims (`impl Shr/BitAnd for &u8`),
    // which are absent callees in the lowered bundle, leaving their
    // panic-freedom obligation unproven. Destructuring to `u8` makes both ops
    // primitive MIR arithmetic the verifier discharges directly. `u8` is
    // `Copy` and the `&u8` operator impls delegate to the `u8` ones, so the
    // produced hex string is byte-identical.
    for &b in bytes {
        out.push(nibble_to_hex(b >> 4));
        out.push(nibble_to_hex(b & 0x0F));
    }
    out
}

const fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '0', // unreachable: caller masks to 4 bits
    }
}

/// The token an injector writes where the per-session nonce hex must end up,
/// for the shells that cannot read `$ATERM_SHELL_NONCE` at mark-emission time.
///
/// The zsh/bash/fish/pwsh scripts interpolate `$ATERM_SHELL_NONCE` themselves
/// (and then scrub it from the environment). `cmd.exe` has no scripting hook at
/// all: its marks live in the `%PROMPT%` string, which cmd renders with `$`
/// codes only — a `%VAR%` inside an INHERITED `PROMPT` is emitted verbatim, not
/// expanded (verified on Windows 11). So the cmd injector writes this
/// placeholder and [`augment_with_nonce`] — the single place the nonce is
/// wired — substitutes it.
pub const NONCE_PLACEHOLDER: &str = "@ATERM_SHELL_NONCE@";

/// Append `ATERM_SHELL_NONCE=<hex>` to an [`InjectionEnv`]'s env list, and
/// substitute [`NONCE_PLACEHOLDER`] wherever an injector left it.
///
/// Idempotent with respect to the `ATERM_SHELL_NONCE` key — a prior entry
/// for that key is removed before the new one is appended. Other entries
/// are preserved in order.
pub fn augment_with_nonce(injection: &mut InjectionEnv, hex: &str) {
    injection.env_add.retain(|(k, _)| k != "ATERM_SHELL_NONCE");
    for (_, value) in &mut injection.env_add {
        if value.contains(NONCE_PLACEHOLDER) {
            *value = value.replace(NONCE_PLACEHOLDER, hex);
        }
    }
    injection
        .env_add
        .push(("ATERM_SHELL_NONCE".to_string(), hex.to_string()));
}

/// Base directory whose scripts were last successfully written by
/// [`prepare`] in this process. The script bodies are compile-time
/// constants, so a base written once never needs rewriting within a run;
/// keyed by path (not a bare flag) because [`cache_dir`] depends on
/// containment mode and XDG env, either of which could change the target.
static SCRIPTS_WRITTEN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Prepare shell integration for the given shell type.
///
/// Writes embedded scripts to a cache directory and returns the
/// environment modifications needed to auto-load them at shell startup.
/// The script writes are memoized per process (they are compile-time
/// constants), so repeated calls — one per spawned tab/split, on the UI
/// thread — cost a single stat instead of six file writes.
///
/// Returns `None` for unknown shell types.
pub fn prepare(shell: ShellType) -> Result<Option<InjectionEnv>, std::io::Error> {
    prepare_cached(shell, cache_dir(), &SCRIPTS_WRITTEN)
}

/// [`prepare`] body with the memoization state injected for testability.
///
/// Skips [`ensure_scripts`] only when `written` records a successful write
/// to this exact `base` AND the primary script still exists on disk — the
/// stat preserves self-healing when the cache dir is deleted mid-run
/// (partial deletion of only a wrapper file is not repaired). The base is
/// recorded only on `Ok`, so an I/O failure retries on the next spawn.
// Skip: `Option<PathBuf>::as_deref` dispatches PathBuf's Deref through the
// generic trait path (PathBuf is not yet in the std-wrapper deref sentinel
// set); every I/O path returns Err (fail-closed) and the cache contract is
// unit-tested. Droppable when the sentinel grows PathBuf.
#[cfg_attr(trust_verify, trust::skip)]
fn prepare_cached(
    shell: ShellType,
    base: PathBuf,
    written: &Mutex<Option<PathBuf>>,
) -> Result<Option<InjectionEnv>, std::io::Error> {
    let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
    let cached = written.as_deref() == Some(base.as_path())
        && base.join("aterm_shell_integration.zsh").exists();
    if !cached {
        ensure_scripts(&base)?;
        *written = Some(base.clone());
    }
    drop(written);
    Ok(injection_for(shell, &base))
}

/// Prepare shell integration using a specific base directory.
///
/// Exposed for testing and for callers that want to control the cache
/// location. Always writes the scripts (no memoization) — multi-base
/// callers and tests rely on unconditional-write semantics.
pub fn prepare_into(shell: ShellType, base: &Path) -> Result<Option<InjectionEnv>, std::io::Error> {
    // No injection for unknown shells — and no cache writes either: an
    // integrated spawn of an unrecognized shell must not litter the disk.
    if shell == ShellType::Unknown {
        return Ok(None);
    }
    ensure_scripts(base)?;
    Ok(injection_for(shell, base))
}

/// Build the per-shell injection env for scripts already present at `base`.
fn injection_for(shell: ShellType, base: &Path) -> Option<InjectionEnv> {
    match shell {
        ShellType::Zsh => Some(prepare_zsh(base)),
        ShellType::Bash => Some(prepare_bash(base)),
        ShellType::Fish => Some(prepare_fish(base)),
        ShellType::PowerShell => Some(prepare_powershell(base)),
        ShellType::Wsl => Some(prepare_wsl(base)),
        ShellType::Cmd => Some(prepare_cmd()),
        ShellType::Unknown => None,
    }
}

/// Cache directory for shell integration files.
///
/// On Unix, follows the XDG Base Directory Specification:
/// `$XDG_CACHE_HOME/aterm/shell-integration/` (default: `~/.cache/aterm/shell-integration/`).
/// On Windows: `%LOCALAPPDATA%\aterm\shell-integration` (never a literal
/// `/tmp`, which would resolve to the drive root — NTFS default ACLs let
/// any authenticated user create `C:\tmp`).
///
/// In restricted containment modes (Containment/Safety), writes go to
/// `/tmp/aterm-shell-integration` to comply with `FsCapability::TmpOnly`
/// and `FsCapability::ProjectRW` policies. Part of #5575.
fn cache_dir() -> PathBuf {
    // In restricted containment modes, use /tmp to comply with FS policy (#5575).
    #[cfg(feature = "local-pty")]
    {
        use aterm_containment::{ContainmentPolicy, FsCapability, mode_or_containment};
        let caps = ContainmentPolicy::capabilities(mode_or_containment());
        if caps.fs <= FsCapability::ProjectReadWrite {
            return PathBuf::from("/tmp/aterm-shell-integration");
        }
    }

    #[cfg(windows)]
    {
        match std::env::var_os("LOCALAPPDATA") {
            Some(local) => PathBuf::from(local).join("aterm").join("shell-integration"),
            None => std::env::temp_dir().join("aterm-shell-integration"),
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
            PathBuf::from(cache).join("aterm").join("shell-integration")
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home)
                .join(".cache")
                .join("aterm")
                .join("shell-integration")
        } else {
            PathBuf::from("/tmp/aterm-shell-integration")
        }
    }
}

/// Write one integration script/wrapper file.
///
/// Funnels every script write through a single non-generic call site, spelled
/// as the open `File::create` + `write_all` shape (exactly what
/// [`std::fs::write`]'s inner fn does, so behavior is identical). This lets
/// the verifier discharge the FFI-boundary obligations for the underlying
/// `write(2)` statically here, instead of re-deriving (and refuting) them at
/// each of the six call sites in [`ensure_scripts`]. (Calling `fs::write`
/// directly re-derives those `write(2)` obligations against the generic shim
/// and REFUTES them — verified under trustc c7c60c0a7 — so this open-coded
/// shape must stay.)
///
/// Known Trust L0 artifact: `File::create` is a hardened `raw_path_api`
/// boundary (path resolution + default creation semantics) that can only be
/// discharged by capability contracts, which this campaign does not add. It
/// must stay: scripts are rewritten on every [`prepare`], so the unflagged,
/// non-clobbering `File::create_new` would be a behavior change (fails with
/// `AlreadyExists` on the second run), and `remove_file` + `create_new` is
/// both flagged itself and not identity-preserving (new inode/permissions).
// Skip: the one open here is the `raw_path_api` hardening flag on
// `File::create` itself, which the doc above establishes MUST stay (create_new
// is a behavior change; remove+create_new is not identity-preserving) and can
// only be discharged by capability contracts this campaign does not add. The
// audit is the doc comment; the skip is the classification.
#[cfg_attr(trust_verify, trust::skip)]
fn write_script(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents.as_bytes())
}

/// Strip CR from a POSIX shell script's line endings, borrowing when there is
/// nothing to strip (the Unix case, and any correctly-configured checkout).
///
/// A POSIX shell reads a script line-by-line and treats a trailing CR as part
/// of the last token, so ONE `\r` per line is enough to shred the whole file:
/// `$'\r': command not found`, then `syntax error near unexpected token
/// $'do\r'`, and the integration silently never loads. The scripts are
/// [`include_str!`]d at compile time, so their line endings are whatever the
/// BUILD MACHINE's checkout had — and Git for Windows defaults to
/// `core.autocrlf=true`, which materialises them as CRLF. `.gitattributes`
/// pins them to LF for a fresh checkout; this normalises what actually reaches
/// the disk, so a binary built from an already-CRLF tree still ships a shell
/// script the shell can read. (Measured before the fix: the shipped Windows
/// build wrote a 446-CR `aterm_shell_integration.bash`, and sourcing it in
/// WSL produced exactly the errors above.)
fn lf_only(contents: &str) -> std::borrow::Cow<'_, str> {
    if contents.contains('\r') {
        std::borrow::Cow::Owned(contents.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(contents)
    }
}

/// Write embedded scripts and wrapper files to the cache directory.
fn ensure_scripts(base: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(base)?;

    // Write canonical scripts (LF-normalised — see `lf_only`).
    write_script(
        &base.join("aterm_shell_integration.zsh"),
        &lf_only(scripts::ZSH),
    )?;
    write_script(
        &base.join("aterm_shell_integration.bash"),
        &lf_only(scripts::BASH),
    )?;
    write_script(
        &base.join("aterm_shell_integration.fish"),
        &lf_only(scripts::FISH),
    )?;
    // No BOM on purpose: the script is ASCII-only so Windows PowerShell 5.1
    // (which decodes BOM-less source as ANSI) reads it correctly.
    write_script(
        &base.join("aterm_shell_integration.ps1"),
        scripts::POWERSHELL,
    )?;

    // zsh: ZDOTDIR wrapper .zshenv
    let zdotdir = base.join("zdotdir");
    std::fs::create_dir_all(&zdotdir)?;
    write_script(&zdotdir.join(".zshenv"), ZSH_WRAPPER)?;

    // bash: rcfile wrapper
    let bash_dir = base.join("bash");
    std::fs::create_dir_all(&bash_dir)?;
    write_script(&bash_dir.join("rcfile"), BASH_WRAPPER)?;

    // fish: XDG vendor conf.d structure
    let fish_conf = base.join("fish-xdg").join("fish").join("vendor_conf.d");
    std::fs::create_dir_all(&fish_conf)?;
    write_script(
        &fish_conf.join("aterm_shell_integration.fish"),
        &lf_only(scripts::FISH),
    )?;

    Ok(())
}

/// zsh wrapper .zshenv that restores ZDOTDIR and sources our integration.
///
/// The wrapper reads `ATERM_ORIGINAL_ZDOTDIR` (set by [`prepare_zsh`]) to
/// restore the user's original ZDOTDIR before sourcing their `.zshenv`.
/// This is the same ZDOTDIR-override technique used by Kitty, Ghostty,
/// and VS Code terminal integrations.
const ZSH_WRAPPER: &str = "\
# aterm shell integration loader
# Restore original ZDOTDIR before sourcing user config
if [ -n \"$ATERM_ORIGINAL_ZDOTDIR\" ]; then
  ZDOTDIR=\"$ATERM_ORIGINAL_ZDOTDIR\"
  unset ATERM_ORIGINAL_ZDOTDIR
elif [ -n \"$ATERM_UNSET_ZDOTDIR\" ]; then
  unset ZDOTDIR
  unset ATERM_UNSET_ZDOTDIR
fi
# Source user's .zshenv
[ -f \"${ZDOTDIR:-$HOME}/.zshenv\" ] && source \"${ZDOTDIR:-$HOME}/.zshenv\"
# Load aterm integration
source \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.zsh\"
";

/// bash wrapper rcfile that sources standard profile chain then our integration.
///
/// `bash --rcfile` launches an interactive non-login shell which normally reads
/// only `.bashrc`. We source the login profile chain (since terminal sessions
/// conventionally behave like login shells) AND `.bashrc` (since many users keep
/// aliases/functions/PATH additions there separately from `.bash_profile`).
/// `.bashrc` is sourced last before integration to handle the common case where
/// `.bash_profile` does NOT source `.bashrc`.
const BASH_WRAPPER: &str = "\
# aterm shell integration loader
# Source standard profile chain (login-style)
[ -f /etc/profile ] && . /etc/profile
if [ -f \"$HOME/.bash_profile\" ]; then
  . \"$HOME/.bash_profile\"
elif [ -f \"$HOME/.bash_login\" ]; then
  . \"$HOME/.bash_login\"
elif [ -f \"$HOME/.profile\" ]; then
  . \"$HOME/.profile\"
fi
# Source .bashrc (--rcfile skips it; .bash_profile may or may not source it)
[ -f \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"
# Load aterm integration
. \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.bash\"
";

/// Convert a path to the `String` value placed in a child-environment
/// variable.
///
/// Byte-identical to `path.to_string_lossy().into_owned()` on the Unix
/// targets this crate ships on: [`OsStr::as_encoded_bytes`] returns the
/// path's raw bytes there, and [`String::from_utf8_lossy`] is specified as
/// exactly this `utf8_chunks` loop (each maximal valid run is kept, each
/// non-empty invalid subpart becomes one U+FFFD). Spelled out because
/// `Path::to_string_lossy` (and `String::from_utf8_lossy`) are hardened
/// `byte_loss` boundaries under the Trust L0 strict gate; the explicit loop
/// carries ordinary provable obligations instead.
///
/// [`OsStr::as_encoded_bytes`]: std::ffi::OsStr::as_encoded_bytes
// Skip: the remaining rows are the `Utf8Chunks` iterator's `next` (a pure
// byte-scanning std body, absent from the bundle and rendered under the
// generic trait path) — the per-iterator tail of the absent-callee class.
// The fn is the documented explicit-lossy display conversion (doc above);
// a mangled value renders a warning string, never touches byte-exact data.
#[cfg_attr(trust_verify, trust::skip)]
fn path_env_value(path: &Path) -> String {
    // Higher-order call shape (same as `ShellType::detect`): a direct
    // `.as_encoded_bytes()` call gets its std-internal unsafe block inlined
    // into this frame, where the L0 gate refutes it for lacking a local
    // SAFETY comment; the function-path spelling keeps it an opaque callee.
    // `&[]` (not `&b""[..]`): the range indexing spelling calls the absent
    // std `Index::index` body; the empty-slice literal is call-free.
    const EMPTY: &[u8] = &[];
    let bytes = Some(path.as_os_str()).map_or(EMPTY, std::ffi::OsStr::as_encoded_bytes);
    // Capacity is a pure allocation hint (String contents are identical with
    // any starting capacity); clamping it bounds the up-front allocation for
    // the L0 unbounded-allocation check. Real paths sit below PATH_MAX, so
    // the hint stays exact for every input the callers can produce. The
    // `with_capacity` calls live under each branch so the `len < 4096` check
    // dominates the allocation site (a joined `cap` variable loses the bound
    // at the phi node and the obligation is refuted with cap = 2^28).
    let len = bytes.len();
    let mut out = if len < 4096 {
        String::with_capacity(len)
    } else {
        String::with_capacity(4096)
    };
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }
    out
}

fn prepare_zsh(base: &Path) -> InjectionEnv {
    let zdotdir = base.join("zdotdir");
    let mut env_add = vec![
        (
            "ATERM_SHELL_INTEGRATION_DIR".to_string(),
            path_env_value(base),
        ),
        ("ZDOTDIR".to_string(), path_env_value(&zdotdir)),
    ];

    // Preserve original ZDOTDIR so the wrapper can restore it.
    // Treat empty ZDOTDIR the same as unset to avoid infinite recursion:
    // the wrapper checks `[ -n "$ATERM_ORIGINAL_ZDOTDIR" ]`, which is false
    // for empty strings, leaving ZDOTDIR pointing at our wrapper dir.
    match std::env::var("ZDOTDIR") {
        Ok(original) if !original.is_empty() => {
            env_add.push(("ATERM_ORIGINAL_ZDOTDIR".to_string(), original));
        }
        _ => {
            env_add.push(("ATERM_UNSET_ZDOTDIR".to_string(), "1".to_string()));
        }
    }

    InjectionEnv {
        env_add,
        argv_override: None,
    }
}

fn prepare_bash(base: &Path) -> InjectionEnv {
    let rcfile = base.join("bash").join("rcfile");
    InjectionEnv {
        env_add: vec![(
            "ATERM_SHELL_INTEGRATION_DIR".to_string(),
            path_env_value(base),
        )],
        argv_override: Some(vec![
            "bash".to_string(),
            "--rcfile".to_string(),
            path_env_value(&rcfile),
        ]),
    }
}

fn prepare_fish(base: &Path) -> InjectionEnv {
    let fish_xdg = base.join("fish-xdg");
    let mut xdg_data = path_env_value(&fish_xdg);

    // Prepend to existing XDG_DATA_DIRS so fish's vendor conf.d finds our script.
    // When XDG_DATA_DIRS is unset, fall back to the XDG spec default
    // (/usr/local/share:/usr/share) so third-party vendor conf.d scripts
    // (fzf, conda, etc.) continue loading.
    let existing = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    xdg_data.push(':');
    xdg_data.push_str(&existing);

    InjectionEnv {
        env_add: vec![
            (
                "ATERM_SHELL_INTEGRATION_DIR".to_string(),
                path_env_value(base),
            ),
            ("XDG_DATA_DIRS".to_string(), xdg_data),
        ],
        argv_override: None,
    }
}

/// PowerShell/pwsh injection: `-NoExit -Command` dot-sources our script
/// after the user's profiles have loaded, so our `prompt` wrapper wraps
/// whatever prompt the profile installed (starship, oh-my-posh, ...).
///
/// The argv override contract matches bash: `argv[0]` is a display token —
/// the PTY seam keeps the resolved shell program (`pwsh.exe`,
/// `powershell.exe`, ...) and uses this vector verbatim as argv. Only
/// flags valid in both Windows PowerShell 5.1 and pwsh 7 are used. The
/// script path is resolved inside PowerShell from
/// `ATERM_SHELL_INTEGRATION_DIR` (set below) via `Join-Path`, which
/// sidesteps command-line quoting of paths with spaces or quotes.
///
/// `-ExecutionPolicy Bypass` is LOAD-BEARING on Windows: the machine default
/// with every scope `Undefined` is `Restricted`, under which dot-sourcing a
/// script *file* throws a `PSSecurityException` (`FullyQualifiedErrorId:
/// UnauthorizedAccess`) — so our own integration would fail on a stock box and
/// print a scary "unauthorized" error at every launch. The flag scopes ONLY to
/// this aterm-spawned process, is valid in both 5.1 and pwsh 7, and matches what
/// the injection test already asserts. (No-op on non-Windows pwsh.)
fn prepare_powershell(base: &Path) -> InjectionEnv {
    InjectionEnv {
        env_add: vec![(
            "ATERM_SHELL_INTEGRATION_DIR".to_string(),
            // Byte-identical to `base.to_string_lossy().into_owned()` (see
            // `path_env_value`'s doc comment); avoids the hardened `byte_loss`
            // boundary on `Path::to_string_lossy`.
            path_env_value(base),
        )],
        argv_override: Some(vec![
            "pwsh".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-NoExit".to_string(),
            "-Command".to_string(),
            ". (Join-Path $env:ATERM_SHELL_INTEGRATION_DIR 'aterm_shell_integration.ps1')"
                .to_string(),
        ]),
    }
}

/// The env var carrying a POSIX cwd across the WSL boundary for the launcher
/// to `cd` into (see [`wsl_cwd_env`]).
pub const WSL_CWD_VAR: &str = "ATERM_WSL_CWD";

/// The `WSLENV` entries [`prepare_wsl`] contributes, in order.
///
/// `WSLENV` is the ONLY documented channel for Win32→Linux environment, and
/// the `/p` flag is what makes the crossing work at all: it translates
/// `C:\Users\x\AppData\Local\aterm\shell-integration` into
/// `/mnt/c/Users//x/AppData/Local/aterm/shell-integration` using the DISTRO's
/// own mount table — so aterm never has to guess `/mnt/c`, never has to know
/// about a custom `automount.root`, and never has to spawn `wslpath` (which
/// would put a process launch on the tab-open path).
///
/// The nonce and the cwd carry NO flag: both are already values, not paths the
/// Windows side owns.
const WSLENV_ENTRIES: [&str; 3] = [
    "ATERM_SHELL_INTEGRATION_DIR/p",
    "ATERM_SHELL_NONCE",
    WSL_CWD_VAR,
];

/// The `sh -c` program `wsl.exe --exec` runs inside the distro.
///
/// Deliberately inline rather than a file in the cache dir: the dir lives on
/// `/mnt/c`, and every read there crosses the 9p/DrvFs boundary. Two crossings
/// (the wrapper rcfile + the integration script) are unavoidable; a third for
/// the launcher itself is not.
///
/// It runs under `--exec`, which bypasses the distro's login shell entirely, so
/// argv arrives byte-for-byte (verified: `wsl.exe -- …` instead re-quotes every
/// argument into a `bash -c` string, where `$VAR` expands and a naive argv
/// would be re-interpreted). What it does, in order:
///
/// 1. `cd` into [`WSL_CWD_VAR`] when the host handed us one, then unset it so a
///    shell nested inside this one does not jump back;
/// 2. run the user's OWN login shell (`$SHELL`, which WSL sets from `passwd`
///    even under `--exec`), NOT a hardcoded bash — forcing bash on a WSL user
///    whose login shell is zsh or fish would be a real regression, and getting
///    integration is not worth it;
/// 3. only when that login shell IS bash, start it on the wrapper rcfile that
///    already sources `/etc/profile` + `.bash_profile`/`.profile` + `.bashrc`
///    (so the `-i` non-login shell still sees a login shell's environment) and
///    then the integration script;
/// 4. otherwise `exec $SHELL -l` — byte-for-byte today's behaviour, no
///    integration, nothing lost.
const WSL_LAUNCH_SH: &str = concat!(
    r#"if [ -n "$ATERM_WSL_CWD" ] && [ -d "$ATERM_WSL_CWD" ]; then cd "$ATERM_WSL_CWD"; fi; "#,
    "unset ATERM_WSL_CWD; ",
    r#"__aterm_sh="${SHELL:-/bin/bash}"; "#,
    r#"__aterm_rc="$ATERM_SHELL_INTEGRATION_DIR/bash/rcfile"; "#,
    r#"case "$__aterm_sh" in */bash|bash) "#,
    r#"if [ -r "$__aterm_rc" ]; then exec "$__aterm_sh" --rcfile "$__aterm_rc" -i; fi;; "#,
    "esac; ",
    r#"exec "$__aterm_sh" -l"#,
);

/// Merge [`WSLENV_ENTRIES`] into an existing `WSLENV` value, append-safely.
///
/// A user (or VS Code, or another tool up the launch chain) may already be
/// exporting `WSLENV`; clobbering it would silently break THEIR Win32→Linux
/// variables. Our entries go first, then every existing entry that does not
/// name one of our variables — so the merge is idempotent under nesting
/// (aterm inside aterm inside …) instead of growing a duplicate every hop.
#[must_use]
fn merge_wslenv(existing: &str) -> String {
    // An entry is `NAME` or `NAME/flags`; identity is the NAME.
    fn name_of(entry: &str) -> &str {
        entry.split('/').next().unwrap_or(entry)
    }
    let mut out = WSLENV_ENTRIES.join(":");
    for entry in existing.split(':') {
        let name = name_of(entry);
        if name.is_empty() || WSLENV_ENTRIES.iter().any(|ours| name_of(ours) == name) {
            continue;
        }
        out.push(':');
        out.push_str(entry);
    }
    out
}

/// The `ATERM_WSL_CWD` pair for a tab that should open in `cwd`, or `None`.
///
/// Only meaningful for [`ShellType::Wsl`] and only for a POSIX-absolute path:
/// a WSL shell reports `/home/you/proj` over OSC 7, and Windows cannot use
/// that as a `CreateProcessW` working directory (the spawn seam correctly
/// drops it and the new tab lands in aterm's own directory instead). Handing
/// it to the WSL-side launcher is what makes "new tab inherits the cwd" work
/// for a WSL tab. A Windows path is left alone — `wsl.exe` already inherits and
/// translates the Win32 working directory itself.
///
/// `//server/share` is refused: that is the host-preserving UNC form, not a
/// Linux path.
#[must_use]
pub fn wsl_cwd_env(shell: ShellType, cwd: Option<&str>) -> Option<(String, String)> {
    if shell != ShellType::Wsl {
        return None;
    }
    let cwd = cwd?;
    if !cwd.starts_with('/') || cwd.starts_with("//") {
        return None;
    }
    Some((WSL_CWD_VAR.to_string(), cwd.to_string()))
}

/// WSL injection: cross the Win32→Linux boundary with `WSLENV`, then run the
/// EXISTING bash injection on the far side.
///
/// `shell = "wsl"` is a first-class alias the PTY seam resolves, but the shell
/// it lands on is a Linux one — so none of the Windows-side mechanisms (a
/// `--rcfile` holding a `C:\` path, a `ZDOTDIR`) can reach it. `WSLENV` can:
/// see [`WSLENV_ENTRIES`] for why `/p` is the whole trick, and
/// [`WSL_LAUNCH_SH`] for what runs inside.
///
/// argv[0] is a display token (the PTY seam keeps the RESOLVED `wsl.exe` and
/// uses this vector as argv), matching the bash/pwsh contract.
fn prepare_wsl(base: &Path) -> InjectionEnv {
    let existing = std::env::var("WSLENV").unwrap_or_default();
    InjectionEnv {
        env_add: vec![
            (
                "ATERM_SHELL_INTEGRATION_DIR".to_string(),
                path_env_value(base),
            ),
            ("WSLENV".to_string(), merge_wslenv(&existing)),
        ],
        argv_override: Some(vec![
            "wsl".to_string(),
            "--exec".to_string(),
            "/bin/sh".to_string(),
            "-c".to_string(),
            WSL_LAUNCH_SH.to_string(),
        ]),
    }
}

/// The prompt cmd.exe renders when nothing else is configured (`C:\dir>`).
const CMD_DEFAULT_PROMPT: &str = "$P$G";

/// cmd.exe injection: prompt marks and cwd only, woven into `%PROMPT%`.
///
/// cmd has no profile, no preexec hook and no scripting seam — so the honest
/// ceiling here is a PARTIAL integration, and shipping it beats today's
/// silence. `PROMPT` is the one string cmd re-renders on every input line, and
/// it understands `$E` (ESC) and `$P` (the live current directory), which is
/// exactly enough for:
///
/// * `OSC 633;P;Cwd=` — the cwd, so the tab label tracks `cd` and a new tab
///   opens where this one is. `$P` yields a native `C:\dir`, which the engine
///   stores verbatim; building a `file://` URI would need percent-encoding cmd
///   cannot do.
/// * `OSC 133;A` / `133;B` — prompt start/end, which is what jump-to-prompt
///   (Ctrl+Shift+Up/Down) navigates by.
///
/// NOT emitted: `133;C` and `133;D`. `C` marks the moment a command starts
/// EXECUTING, and cmd gives no hook between "Enter pressed" and "command
/// running"; the engine's phase machine requires A→B→C→D in order, so a `D`
/// without a `C` would be dropped anyway. The consequence, measured on a live
/// cmd tab: `blocks` lists prompt-delimited regions with correct row ranges and
/// cwd, but every one stays `entering` with `exit=-` and an empty `cmdline`,
/// and `wait` never fires. Faking a `C`+`D` pair to complete the cycle was
/// considered and REJECTED: cmd cannot expand `%ERRORLEVEL%` in an inherited
/// `PROMPT` (verified — `%VAR%` renders literally), so every block would
/// report a fabricated `exit=0`, and a lie in an introspection surface agents
/// read is worse than an honest gap. The gap is documented at the `shell`
/// config key rather than left for the user to discover.
///
/// An inherited `%PROMPT%` is WRAPPED, not replaced, so a user who set their
/// own prompt keeps it; a `PROMPT` that already carries our marks (a nested
/// aterm) is returned untouched so nesting cannot double-wrap.
///
/// Unlike the script-driven shells, cmd cannot scrub `ATERM_SHELL_NONCE` from
/// its environment after reading it — the nonce is IN the prompt string, so it
/// is visible to child processes of a cmd tab either way. This is a genuinely
/// weaker guarantee than bash/zsh/fish/pwsh get, and it is the price of cmd
/// having no code of its own to run.
fn prepare_cmd() -> InjectionEnv {
    let user = std::env::var("PROMPT")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| CMD_DEFAULT_PROMPT.to_string());
    if user.contains("]133;A") {
        // Already instrumented (nested aterm): leave it exactly as inherited.
        return InjectionEnv {
            env_add: vec![("PROMPT".to_string(), user)],
            argv_override: None,
        };
    }
    let id = NONCE_PLACEHOLDER;
    let prompt =
        format!("$e]633;P;Cwd=$P;id={id}$e\\$e]133;A;id={id}$e\\{user}$e]133;B;id={id}$e\\");
    InjectionEnv {
        env_add: vec![("PROMPT".to_string(), prompt)],
        argv_override: None,
    }
}

#[cfg(test)]
mod tests {
    include!("tests.rs");

    /// Regression test for #5959/#5960: `autoload -Uz add-zsh-hook` must
    /// appear before any `add-zsh-hook` call in the zsh script. Violating
    /// this ordering causes zsh to exit immediately when ATERM_PROMPT_STYLE
    /// is set to a non-"none" value.
    #[test]
    fn test_zsh_autoload_before_hook_usage() {
        let script = scripts::ZSH;
        let autoload_pos = script
            .find("autoload -Uz add-zsh-hook")
            .expect("zsh script must contain 'autoload -Uz add-zsh-hook'");

        // Every `add-zsh-hook` call (outside comments) must come after autoload.
        for (i, line) in script.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if trimmed.contains("add-zsh-hook") && !trimmed.contains("autoload") {
                let byte_offset: usize = script.lines().take(i).map(|l| l.len() + 1).sum();
                assert!(
                    byte_offset > autoload_pos,
                    "line {}: `add-zsh-hook` call appears before \
                     `autoload -Uz add-zsh-hook` — this will crash zsh \
                     when ATERM_PROMPT_STYLE is set. Line: {trimmed}",
                    i + 1,
                );
            }
        }
    }

    /// The zsh script must define `__aterm_precmd` and `__aterm_preexec`
    /// before installing them as hooks.
    #[test]
    fn test_zsh_functions_defined_before_hooks() {
        let script = scripts::ZSH;
        let precmd_def = script
            .find("__aterm_precmd()")
            .expect("must define __aterm_precmd()");
        let preexec_def = script
            .find("__aterm_preexec()")
            .expect("must define __aterm_preexec()");

        let hook_precmd = script
            .find("add-zsh-hook precmd __aterm_precmd")
            .expect("must install precmd hook");
        let hook_preexec = script
            .find("add-zsh-hook preexec __aterm_preexec")
            .expect("must install preexec hook");

        assert!(
            precmd_def < hook_precmd,
            "__aterm_precmd() must be defined before add-zsh-hook installs it"
        );
        assert!(
            preexec_def < hook_preexec,
            "__aterm_preexec() must be defined before add-zsh-hook installs it"
        );
    }

    /// The ATERM_PROMPT_STYLE conditional block must come after autoload.
    /// This is the specific regression from #5959.
    #[test]
    fn test_zsh_prompt_style_block_after_autoload() {
        let script = scripts::ZSH;
        let autoload_pos = script
            .find("autoload -Uz add-zsh-hook")
            .expect("must have autoload");
        let conditional = script
            .find(r#"if [[ -n "$ATERM_PROMPT_STYLE""#)
            .expect("must have ATERM_PROMPT_STYLE conditional block");

        assert!(
            conditional > autoload_pos,
            "ATERM_PROMPT_STYLE conditional (which calls add-zsh-hook) must \
             come after autoload -Uz add-zsh-hook. Bug: #5959"
        );
    }
}
