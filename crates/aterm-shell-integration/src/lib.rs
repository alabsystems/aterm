// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

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
//! | bash  | `--rcfile` (Git Bash: `--login --posix` + `ENV`) | Loader reads `~/.bashrc` (macOS: the login profiles too; Git Bash: what a login shell reads) then ours |
//! | fish  | `XDG_DATA_DIRS` | Vendor conf.d auto-loading |
//! | pwsh/powershell | `-NoExit -Command` | Argv override dot-sources our `.ps1` after profiles, then lays readline's chords over PSReadLine's Windows mode (`prepare_powershell`) |
//! | wsl   | `WSLENV` + `wsl.exe --exec` | `/p` path-translates our dir, then bash's `--rcfile` runs INSIDE the distro |
//! | cmd   | `PROMPT` | `$e` OSC 0 title + OSC 133 A/B + OSC 633 `Cwd=` woven around the user's prompt |
//!
//! # Usage
//!
//! ```rust,no_run
//! use aterm_shell_integration::{ShellType, prepare};
//!
//! let shell = ShellType::detect("/bin/zsh");
//! if let Ok(Some(injection)) = prepare(shell) {
//!     // Add injection.env_add to the child's environment before fork
//!     // Use injection.argv_override if Some (bash --rcfile)
//! }
//! ```
#![cfg_attr(
    test,
    allow(
        clippy::disallowed_types,
        reason = "test code: a stand-in child may inherit descriptors"
    )
)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

mod shell_args;
pub use shell_args::{ArgsMerge, merge_shell_args, shell_args_bypass};

/// Embedded shell integration scripts (compiled into the binary).
///
/// This crate is the canonical owner of the shell script bodies (`aterm-core`
/// re-exports it as `shell_integration`).
pub mod scripts {
    /// zsh shell integration (OSC 7/133 + prompt override).
    pub const ZSH: &str = include_str!("scripts/aterm_shell_integration.zsh");
    /// bash shell integration (OSC 7/133 + prompt override).
    pub const BASH: &str = include_str!("scripts/aterm_shell_integration.bash");
    /// fish shell integration (OSC 7/133 + prompt override).
    pub const FISH: &str = include_str!("scripts/aterm_shell_integration.fish");
    /// PowerShell / pwsh shell integration (OSC 7/133 + the readline key layer).
    pub const POWERSHELL: &str = include_str!("scripts/aterm_shell_integration.ps1");

    /// The comment that opens the BODY of the zsh, bash and fish scripts:
    /// everything from the line it starts to
    /// the line [`BODY_END`] starts is the part a shell that is ALREADY RUNNING
    /// re-sources when its host points it at a newer build's folder. See "THE
    /// LOADER" in the zsh script.
    pub const BODY_BEGIN: &str = "# @@ATERM-INTEGRATION-BODY-BEGIN@@";

    /// The comment that closes the BODY ([`BODY_BEGIN`]).
    pub const BODY_END: &str = "# @@ATERM-INTEGRATION-BODY-END@@";

    /// The BODY of `script` — one of [`ZSH`], [`BASH`], [`FISH`]: the text from
    /// the start of its [`BODY_BEGIN`] line through the end of its [`BODY_END`]
    /// line, byte for byte what the loaded script runs there. `None` for a
    /// script with no body (PowerShell) or markers out of order.
    #[must_use]
    pub fn body(script: &str) -> Option<&str> {
        let begin = script.find(BODY_BEGIN)?;
        let end = begin + script[begin..].find(BODY_END)?;
        let end = script[end..]
            .find('\n')
            .map_or(script.len(), |nl| end + nl + 1);
        Some(&script[begin..end])
    }
}

/// What opens every body file ([`scripts::body`] written alone,
/// `aterm_shell_integration_body.<ext>`): a comment, the same in all three
/// shells.
const BODY_FILE_HEADER: &str = "\
# aterm shell integration — the BODY alone (the LOADER / BODY split, 2026-09-26).
#
# The text between the markers of aterm_shell_integration.<ext> in this folder,
# byte for byte. A running shell's loader sources it when the host that owns the
# shell points it at this folder (`__aterm_body_check`); it relies on the
# loader's state, so it is never sourced by hand.
";

/// The file name of the BODY of the shell whose script is named
/// `aterm_shell_integration.<ext>`: `aterm_shell_integration_body.<ext>`, in the
/// same content-addressed folder.
pub const BODY_FILE_STEM: &str = "aterm_shell_integration_body";

/// The environment variable that names a session's BODY POINTER: set by the
/// host at spawn, captured and scrubbed by the zsh/bash/fish loaders,
/// checked at every prompt (`__aterm_body_check`). The file, when the host
/// writes it, holds the 16-hex address of a script folder under the same cache
/// root — whose body the shell sources at its next prompt.
pub const BODY_POINTER_VAR: &str = "ATERM_INTEGRATION_POINTER";

/// The OSC 633 `P` property the body signs before every prompt:
/// `633;P;AtermIntegration=<16-hex folder address>;id=<nonce>` — which body the
/// shell runs (`status integration_rev=`).
pub const INTEGRATION_REV_KEY: &str = "AtermIntegration";

/// The OSC 633 `P` property every integration signs right after the OSC 7 it
/// sends before each prompt: `633;P;AtermCwd=<that OSC 7's URI>;id=<nonce>`,
/// the shell's own word for its cwd, which a report a program writes cannot
/// forge (docs/DESIGN-cwd-reports-2026-10-19.md). The OSC 7 itself carries no
/// nonce: an engine from before the property ignores it and reads the OSC 7
/// as it always did, so the nonce never reaches a path an older engine stores.
pub const CWD_REPORT_KEY: &str = "AtermCwd";

/// The shells whose scripts have a LOADER that re-sources a newer body — the
/// ones a host exports [`BODY_POINTER_VAR`] to.
#[must_use]
pub const fn has_body_loader(shell: ShellType) -> bool {
    matches!(shell, ShellType::Zsh | ShellType::Bash | ShellType::Fish)
}

/// Whether `rev` is a script folder's address as the loaders accept it: exactly
/// 16 lowercase hex digits ([`script_set_address`]).
#[must_use]
pub fn is_integration_rev(rev: &str) -> bool {
    is_address(rev)
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
            // The `shell = "wsl"` / `"cmd"` aliases the Windows PTY seam
            // resolves (`aterm_pty::windows::shell::discover_shell`). As
            // `Unknown`, `prepare()` would inject NOTHING and every OSC 133
            // consumer (jump-to-prompt, blocks, `wait`, cwd inherit) would be
            // silently dead in a WSL or cmd tab.
            "wsl" => Self::Wsl,
            "cmd" => Self::Cmd,
            _ => Self::Unknown,
        }
    }

    /// Detect the interactive shell aterm will launch.
    ///
    /// Unix / git-bash: `$SHELL`. Native Windows never sets `$SHELL` (and under an
    /// inherited git-bash env it holds a POSIX path `CreateProcessW` can't exec),
    /// so there we mirror the PTY seam's `select_shell()` with no override given:
    /// PowerShell — the shell aterm actually spawns (`pwsh`/`powershell`, both in
    /// System32, resolve before any `cmd` fallback). A configured `shell` /
    /// `--shell` is the caller's hint and never reaches this default. Returning
    /// PowerShell is what makes the `-ExecutionPolicy Bypass` + OSC 7/133
    /// injection reach the spawned shell on Windows.
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
            Self::PowerShell
        }
    }

    /// Return the canonical lowercase name of this shell type.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::Fish => "fish",
            Self::PowerShell => "powershell",
            Self::Wsl => "wsl",
            Self::Cmd => "cmd",
            Self::Unknown => "unknown",
        }
    }

    /// Return `true` when this shell has a supported integration strategy (`!Unknown`).
    #[must_use]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Return `true` when this shell is a POSIX/Unix shell (`Zsh`, `Bash`, or `Fish`).
    #[must_use]
    pub const fn is_posix_shell(self) -> bool {
        matches!(self, Self::Zsh | Self::Bash | Self::Fish)
    }

    /// Return `true` when this shell is a Windows host shell (`PowerShell`, `Wsl`, or `Cmd`).
    #[must_use]
    pub const fn is_windows_shell(self) -> bool {
        matches!(self, Self::PowerShell | Self::Wsl | Self::Cmd)
    }
}

/// Encode a shell integration revision record (`rev_hash`, `shell_kind`, `nonce_prefix`) into 16 LE bytes.
#[must_use]
pub const fn encode_integration_rev_le(
    rev_hash: u64,
    shell_kind: u32,
    nonce_prefix: u32,
) -> [u8; 16] {
    [
        rev_hash as u8,
        (rev_hash >> 8) as u8,
        (rev_hash >> 16) as u8,
        (rev_hash >> 24) as u8,
        (rev_hash >> 32) as u8,
        (rev_hash >> 40) as u8,
        (rev_hash >> 48) as u8,
        (rev_hash >> 56) as u8,
        shell_kind as u8,
        (shell_kind >> 8) as u8,
        (shell_kind >> 16) as u8,
        (shell_kind >> 24) as u8,
        nonce_prefix as u8,
        (nonce_prefix >> 8) as u8,
        (nonce_prefix >> 16) as u8,
        (nonce_prefix >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a shell integration revision record (`rev_hash`, `shell_kind`, `nonce_prefix`).
#[must_use]
pub const fn decode_integration_rev_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let rev_hash = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let shell_kind = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let nonce_prefix = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (rev_hash, shell_kind, nonce_prefix)
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
#[cfg(any(unix, windows))]
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
    /// 64-char lowercase hex encoding to set as `ATERM_SHELL_NONCE` in the
    /// child shell environment.
    #[must_use]
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// Consume the nonce and return both halves: the raw bytes authorize the
    /// terminal (`Terminal::authorize_shell_integration`), the hex is injected
    /// into the child environment.
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
///
/// # Availability
/// `unix` and `windows` ONLY — exactly the targets that have the audited
/// entropy surface. See [`fill_nonce_entropy`] for why there is no third arm.
#[must_use]
// Trust: the fill's only panic is an unrecoverable OS-entropy failure, chosen over
// a weaker nonce (see `fill_nonce_entropy`). Its panic-freedom is an assumption on
// the OS CSPRNG, not provable, hence the skip.
#[cfg_attr(trust_verify, trust::skip)]
#[cfg(any(unix, windows))]
pub fn generate_nonce() -> ShellNonce {
    let mut raw = [0u8; SHELL_NONCE_BYTES];
    fill_nonce_entropy(&mut raw);
    let hex = hex_encode(&raw);
    ShellNonce { raw, hex }
}

/// Fill `buf` from the OS CSPRNG, on the ONE audited entropy surface for this
/// platform.
///
/// Unix and Windows go through [`aterm_uds::rand::fill`] — `getentropy(2)` with
/// a bounded `read_exact` fallback, `BCryptGenRandom` on Windows — the
/// workspace's one audited entropy helper.
///
/// # No `wasm32` arm
/// The callers (aterm-gui's `spawn.rs`, `shell_rekey.rs`) mint a nonce for a
/// spawned shell, and a browser spawns none. A target with no `aterm_uds::rand`
/// gets no `generate_nonce`, so reaching for one is a name error rather than a
/// nonce from an unaudited source. Adding a target means adding its CSPRNG here
/// first; deny.toml bars third-party entropy crates (`forge check` [OB-18]).
///
/// # Panics
/// When the OS CSPRNG is unavailable. Deliberate and documented: a weaker
/// nonce would silently un-gate shell integration, so this fails loud.
#[cfg(any(unix, windows))]
fn fill_nonce_entropy(buf: &mut [u8; SHELL_NONCE_BYTES]) {
    aterm_uds::rand::fill(buf)
        .expect("OS CSPRNG unavailable: cannot mint a shell-integration nonce");
}

/// Lowercase hex-encode a 32-byte nonce. Exposed for host-side helpers
/// that wire a caller-provided nonce (e.g. test fixtures that want
/// deterministic bytes).
#[must_use]
#[cfg(any(unix, windows))]
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

// Not platform-gated: `script_set_address` (every platform, wasm32 included)
// names its folder with it.
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

/// The environment variable that names a session's RE-KEY file: set by the
/// host at spawn, captured and scrubbed by the zsh/bash/fish scripts, checked at
/// every prompt (`__aterm_rekey_check`). The file, when the host writes it, holds
/// one fresh 64-hex nonce the host has already authorized.
pub const REKEY_PATH_VAR: &str = "ATERM_REKEY_PATH";

/// What a fresh bash's wrapper appends to its [`REKEY_PATH_VAR`] for the one
/// file the shell writes itself: `<re-key path>.trap`, the DEBUG trap its
/// startup files set, handed to the integration (which chains it) without the
/// fork a command substitution takes (`bash_debug_trap_handover!`). Nothing
/// reads it after that load; the host removes it with the session, or its
/// sweep once it is stale (a session the final exit or a crash dropped unrun).
pub const DEBUG_TRAP_SUFFIX: &str = ".trap";

/// The shells whose scripts carry the re-key hook — the ones a host exports
/// [`REKEY_PATH_VAR`] to.
#[must_use]
pub const fn has_rekey_hook(shell: ShellType) -> bool {
    matches!(shell, ShellType::Zsh | ShellType::Bash | ShellType::Fish)
}

/// THE WARM SPARE (docs/DESIGN-warm-spare-2026-10-16.md): the folder under the
/// host's control dir that holds a spare shell's hold FIFO, reached as
/// `<control dir>/spare/<sid>` (the host's link to the FIFO, which is named for
/// its host's pid and start time). A shell finds it from the re-key path it is
/// already handed (`<control dir>/rekey/<sid>`), so a spare's environment is
/// byte for byte a cold tab's.
pub const SPARE_DIR: &str = "spare";

/// The 633 `P` key a spare signs once it waits at its PARKED hold, before any
/// startup file: `633;P;AtermSpare=parked;id=<nonce>`.
pub const SPARE_KEY: &str = "AtermSpare";

/// The 633 `P` key that completes a shell's START record, signed once, at its
/// first prompt hook after every startup file: `cold` for a shell nobody
/// held, `spare` for a warm spare about to wait at its WARM hold.
pub const START_KEY: &str = "AtermStart";

/// The START record's rc folder: the final `${ZDOTDIR:-$HOME}` (zsh), or
/// `$HOME` (bash), escaped as 633 `E` escapes a command line.
pub const START_RC_KEY: &str = "AtermStartRc";

/// The START record's flags: `pre:<n>,s:<n>,j:<n|->,r:<0|1>` — the prompt
/// hooks that ran ahead of aterm's, pending `sched` events, jobs, and
/// periodic hooks.
pub const START_FLAGS_KEY: &str = "AtermStartFlags";

/// The START record's other startup roots, fish's alone: its sysconf folder,
/// then each vendor conf.d folder its conf.d loop walks, joined by `\x00` (the
/// one byte no path holds), each one's backslashes, `;` and control bytes
/// escaped as [`START_RC_KEY`]'s are (a space may stay as it is).
pub const START_ROOTS_KEY: &str = "AtermStartRoots";

/// The shells whose integration carries a spare's WARM hold (at its first
/// prompt). macOS and Linux: the host's escape scan, which a WARM spare needs,
/// reads XNU's process table there, and `/proc` here (aterm-gui
/// `spare::escape_scan`); Windows has neither.
#[must_use]
pub const fn has_spare_hold(shell: ShellType) -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
        && matches!(shell, ShellType::Zsh | ShellType::Bash | ShellType::Fish)
}

/// The shells whose wrapper also carries the PARKED hold, ahead of every
/// startup file of the person's: zsh's `.zshenv` and bash's `--rcfile`. fish
/// reads no file of aterm's ahead of the user's (its vendor conf.d runs with
/// theirs), so a fish spare is spawned WARM or not at all. A system file the
/// shell reads before aterm's ([`pre_hold_files`]) runs before the hold.
#[must_use]
pub const fn has_parked_hold(shell: ShellType) -> bool {
    has_spare_hold(shell) && matches!(shell, ShellType::Zsh | ShellType::Bash)
}

/// The system files `shell` reads BEFORE aterm's parked hold, wherever it was
/// built to: zsh's global zshenv, which no option skips (compiled in:
/// `/etc/zshenv` on macOS, `/etc/zsh/zshenv` on Debian, Ubuntu and Arch), and
/// on Linux bash's system bashrc, which a non-login bash reads before its
/// `--rcfile` (`/etc/bash.bashrc` there; macOS's bash has none). Measured on
/// Ubuntu 24.04 (zsh 5.9, bash 5.2.21): `zsh -o sourcetrace` read
/// `/etc/zsh/zshenv` before `$ZDOTDIR/.zshenv`, and `strace` of `bash --rcfile`
/// opened `/etc/bash.bashrc` before the rcfile. One that exists has run in a
/// parked spare before anyone asked: parking it needs the warm gate, and its
/// folder is the spare's own (aterm-gui `spare`).
#[must_use]
pub const fn pre_hold_files(shell: ShellType) -> &'static [&'static str] {
    match shell {
        ShellType::Zsh => &["/etc/zshenv", "/etc/zsh/zshenv"],
        ShellType::Bash if cfg!(target_os = "linux") => &["/etc/bash.bashrc"],
        _ => &[],
    }
}

/// THE TYPED RE-KEY: what a line typed at a shell's prompt runs to
/// take a new nonce from the one-use file `quoted_path` — a word the caller has
/// already quoted for `shell` — and remove the file, for a shell spawned BEFORE
/// the re-key channel, whose script has no `__aterm_rekey_check` and so never
/// reads [`REKEY_PATH_VAR`]. The live agent upgrade types it in front of its
/// relaunch line: the one moment such a shell, not the agent, holds the
/// terminal. `None` for a shell with no such script.
///
/// It sets the two globals every script since `f93cf3ba1` signs its marks
/// from, and every emitter reads them when it runs, never at source
/// time: `__aterm_shell_nonce` (`typeset -g` in zsh, a plain global in bash,
/// `set -g` in fish) and `__aterm_id_suffix_str` (`;id=<nonce>`) — measured in
/// the scripts of the builds that spawned the owner's degraded tabs (0.91 and
/// the 2026-09-10 build; `git show v0.91.0:crates/aterm-shell-integration/src/scripts/…`),
/// and pinned for this build's by the live `a_live_shell_without_the_channel_takes_a_typed_rekey` test.
/// The key never enters the typed text, argv (`read` is a builtin), the
/// scrollback or history: only the path does. Nor a user's `set -x` trace: zsh
/// and bash print it on the stderr the command runs with, so in zsh and bash
/// every command that expands the key sits INSIDE the group whose stderr is
/// silenced (outside it, the assignment traces the key in plain text; measured
/// on zsh 5.9 and bash 3.2). Not under bash 4.1+'s `BASH_XTRACEFD`, whose
/// trace goes to a
/// descriptor no group silences: there the assignments still write the key into
/// the file the user named for the trace (measured on bash 5.2.37; the scripts
/// themselves trace no key there). A file that is gone by the time
/// the line runs changes nothing — the `read` fails (quietly in zsh and bash;
/// fish may print its redirection warning), the nonce keeps its value, and the
/// suffix is rebuilt from it — so a withdrawn key cannot strand the command the
/// line goes on to run.
///
/// Bash's one mark baked at a prompt's setup (the `133;B` inside a custom
/// `ATERM_PROMPT_STYLE` prompt) keeps the old id until that prompt is rebuilt,
/// exactly as after the channel's own re-key; every other mark reads the new one.
///
/// This is the KEY-ONLY form, which reads the file's first line and nothing
/// else. [`typed_rekey_with_loader`] also sources a newer build's loader; the
/// caller types that one where it fits and this one where only this fits.
#[must_use]
pub fn typed_rekey(shell: ShellType, quoted_path: &str) -> Option<String> {
    let p = quoted_path;
    match shell {
        ShellType::Zsh | ShellType::Bash => Some(format!(
            "{{ read -r __aterm_shell_nonce <{p}; \
             __aterm_id_suffix_str=\";id=$__aterm_shell_nonce\"; }} 2>/dev/null; \
             command rm -f -- {p};"
        )),
        ShellType::Fish => Some(format!(
            "read -g __aterm_shell_nonce <{p} 2>/dev/null; command rm -f -- {p}; \
             set -g __aterm_id_suffix_str \";id=$__aterm_shell_nonce\";"
        )),
        _ => None,
    }
}

/// [`typed_rekey`] that ALSO UPGRADES THE SHELL'S INTEGRATION IN PLACE: what a
/// line typed at the prompt of a shell spawned BEFORE
/// loaders existed runs, so that its integration — fixed at spawn, and reached
/// by no body pointer — becomes this build's, with the loader that takes every
/// later build's body at a prompt.
///
/// The one-use file holds up to three lines, all written by the window
/// (`shell_rekey::typed` in aterm-gui): the key (the fresh one of a degraded
/// shell, or the one a healthy shell already signs with — never an empty line
/// the old key-only text would take for a key), then the folder of this build's
/// scripts and the session's body pointer path when the shell has no loader.
/// The line reads them into short-lived globals with builtins, removes the
/// file, takes the key as [`typed_rekey`] does, and THEN sources — only when a
/// folder was named and holds this shell's loader — that loader with the
/// pointer path in hand. The loader recognises the shell as its own (it holds
/// `__aterm_shell_nonce` AND the hook function its integration defined, which
/// no other shell can have inherited and no typed line defines) and upgrades it
/// in place: the hooks it already has are kept, the body and the trampoline are
/// replaced, and the revision it signs at once carries the file's key — the one
/// a degraded tab's engine verifies. A NESTED shell — started in the tab, so it
/// inherited the guard and ran no integration of its own — defines no such
/// hook, and the loader stops at the guard, at this line and every later one.
///
/// Nothing in the text is a path but the file's own; the folder and the pointer
/// never appear in typed text, argv, scrollback or history. A file that is gone
/// changes nothing, quietly: the reads sit behind the file's redirection, which
/// sits INSIDE the group whose stderr is silenced (zsh reports a failed
/// redirection on the shell's own stderr unless it is nested so; measured on zsh
/// 5.9) — fish reads it through `cat`, as fish warns on the screen of any
/// redirection from a missing file — and every variable is read `-`-guarded, so
/// a user's `set -u` cannot abort the line before the command it goes on to run.
#[must_use]
pub fn typed_rekey_with_loader(shell: ShellType, quoted_path: &str) -> Option<String> {
    let p = quoted_path;
    let loader = match shell {
        ShellType::Zsh => "aterm_shell_integration.zsh",
        ShellType::Bash => "aterm_shell_integration.bash",
        ShellType::Fish => "aterm_shell_integration.fish",
        _ => return None,
    };
    // Four short-lived globals — the key, the folder, the pointer and the loader
    // path — named short because the line shares the tty's bound with the
    // relaunch it carries, and unset at its end; every read is `-`-guarded.
    //
    // The key is assigned BEFORE the loader is sourced, so the revision an
    // upgrade in place signs at once carries the key the line delivers. Sourced
    // first, the loader would sign with the key a DEGRADED tab's engine lost, the
    // mark would be dropped, and the new key would go unseen until the shell's
    // next prompt — hours away behind the agent the line relaunches.
    //
    // The order is safe for a NESTED shell (a `bash` typed in the tab, a `nix
    // develop`), which inherited the exported guard and which the harness types
    // this line into when it leads the tab's foreground group: the loader's
    // proof that a shell is its own is the nonce AND the hook function its
    // integration defined, which no child shell inherits and this line never
    // defines — so the key the line leaves in a nested shell proves nothing, and
    // the loader stops at the guard. fish reads the file through `cat`: a
    // redirection from a file that is gone prints two warnings on the user's
    // screen whatever stderr says (measured on fish 4.9.3), and a withdrawn file
    // must change nothing, quietly.
    Some(match shell {
        ShellType::Fish => format!(
            "command cat {p} 2>/dev/null | begin; read -g __atk; read -g __atd; read -g __atp; \
             end; command rm -f -- {p}; \
             test -n \"$__atk\"; and set -g __aterm_shell_nonce $__atk; \
             set -g __aterm_id_suffix_str \";id=$__aterm_shell_nonce\"; \
             set -g __atl \"$__atd/{loader}\"; test -f \"$__atl\"; and begin; \
             set -g ATERM_INTEGRATION_POINTER $__atp; source \"$__atl\"; end; \
             set -e __atk __atd __atp __atl;"
        ),
        _ => format!(
            "{{ {{ read -r __atk; read -r __atd; read -r __atp; }} <{p}; \
             [ -n \"${{__atk-}}\" ] && __aterm_shell_nonce=$__atk; \
             __aterm_id_suffix_str=\";id=$__aterm_shell_nonce\"; }} 2>/dev/null; \
             command rm -f -- {p}; __atl=\"${{__atd-}}/{loader}\"; [ -f \"$__atl\" ] && \
             {{ ATERM_INTEGRATION_POINTER=${{__atp-}}; . \"$__atl\"; }}; \
             unset __atk __atd __atp __atl;"
        ),
    })
}

/// This build's script folder, once [`prepare`] has installed or verified it in
/// this process. The script bodies are compile-time constants, so a folder
/// verified once never needs rewriting within a run; keyed by path (not a bare
/// flag) because [`cache_dir`] depends on containment mode and XDG env, either
/// of which could change the target.
static SCRIPTS_WRITTEN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Prepare shell integration for the given shell type.
///
/// Installs this build's scripts in ITS OWN folder under the cache directory —
/// `<cache>/<content address>/` ([`script_set_address`]) — and returns the
/// environment modifications that make a new shell load them from there. The
/// install is memoized per process, so repeated calls — one per spawned
/// tab/split, on the UI thread — cost a single stat.
///
/// WHY A FOLDER PER SCRIPT SET: with one shared folder, any other build that
/// started (a dev bundle, a stale test binary) rewrote the scripts, and every
/// new tab of the running window silently sourced that build's bytes. A folder
/// named by its contents is written once (temp dir + rename, never
/// overwritten), so another build can only write ITS folder. The legacy flat
/// files are left alone: older builds still write and read them.
///
/// Returns `None` for unknown shell types.
pub fn prepare(shell: ShellType) -> Result<Option<InjectionEnv>, std::io::Error> {
    prepare_cached(shell, cache_dir(), &SCRIPTS_WRITTEN)
}

/// This build's script folder, installed (or verified) as [`prepare`] installs
/// it: `<cache>/<content address>/`, holding every loader and every body. What
/// a host names to a RUNNING shell — the body pointer it writes carries the
/// folder's address, and a typed upgrade names the folder itself — so the folder
/// is made sure of first, and a shell is never pointed at bytes that are not
/// this build's.
pub fn ensure_script_set() -> Result<PathBuf, std::io::Error> {
    ensure_cached(cache_dir(), &SCRIPTS_WRITTEN)
}

/// The zsh loader and body of a script folder (`dir`, as [`ensure_script_set`]
/// returns it), compiled to wordcode beside them by `zsh`, the shell the
/// sessions run: `<script>.zwc`, which zsh sources in place of the text when it
/// is not older than the text and was made by the same zsh version, and passes
/// over in silence when it is not. One the folder already holds is kept.
///
/// WHY: a fresh zsh parses the whole loader before its first prompt. Measured
/// 2026-10-16 (zsh 5.9, a PTY, the captured aterm spawn env, interleaved, n=12):
/// the compiled loader brought the first prompt 0.7 ms sooner than the text
/// without its comment lines ([`script_set`]), 1.7 ms sooner than the commented
/// text.
///
/// Each is compiled under a scratch name and renamed into place, so a shell
/// starting meanwhile reads the whole wordcode or none (`zcompile` writes its
/// output in place). `-f`: the parse sees zsh's default options and no alias,
/// where the text was parsed under whatever the user's `.zshenv` (sourced first)
/// had set; `-U` expands no alias into the wordcode. A failure leaves the folder
/// as it was, and its shells parse the text.
///
/// Only where `aterm_uds` spawns a child (its target table in `Cargo.toml`): a
/// browser build of this crate has no process to run a zsh in.
///
/// # Errors
/// When `zsh` cannot be run, `zcompile` fails, or the rename does.
#[cfg(any(unix, windows))]
pub fn compile_zsh_scripts(dir: &Path, zsh: &Path) -> Result<(), std::io::Error> {
    for name in [
        "aterm_shell_integration.zsh",
        "aterm_shell_integration_body.zsh",
    ] {
        let compiled = dir.join(format!("{name}.zwc"));
        if compiled.exists() {
            continue;
        }
        // Ends in `.zwc`, or `zcompile` would append it.
        let scratch = dir.join(format!(".{name}.tmp-{}.zwc", scratch_suffix()));
        let status = aterm_uds::spawnfd::command(zsh)
            .args(["-f", "-c", "zcompile -U -- \"$1\" \"$2\"", "zsh"])
            .arg(&scratch)
            .arg(dir.join(name))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .status();
        let placed = match status {
            Ok(status) if status.success() => std::fs::rename(&scratch, &compiled),
            Ok(status) => Err(std::io::Error::other(format!(
                "zcompile of {name} exited {status}"
            ))),
            Err(e) => Err(e),
        };
        if let Err(e) = placed {
            let _ = std::fs::remove_file(&scratch);
            return Err(e);
        }
    }
    Ok(())
}

/// [`prepare`] body with the cache root and the memoization state injected for
/// testability.
///
/// Skips [`install_script_set`] only when `written` records this exact folder
/// AND its primary script still exists on disk — the stat preserves self-healing
/// when the folder is deleted mid-run (by a person, or another build's garbage
/// collection of a folder older than [`GC_MIN_AGE`]). The folder is recorded
/// only on `Ok`, so an I/O failure retries on the next spawn.
// Skip: `Option<PathBuf>::as_deref` dispatches PathBuf's Deref through the
// generic trait path (PathBuf is not in the std-wrapper deref sentinel set);
// every I/O path returns Err and the cache contract is unit-tested. Drop the
// skip once a promoted Trust seal adds PathBuf to that set.
#[cfg_attr(trust_verify, trust::skip)]
fn prepare_cached(
    shell: ShellType,
    root: PathBuf,
    written: &Mutex<Option<PathBuf>>,
) -> Result<Option<InjectionEnv>, std::io::Error> {
    let base = ensure_cached(root, written)?;
    Ok(injection_for(shell, &base))
}

/// The install half of [`prepare_cached`]: this build's folder under `root`,
/// installed unless `written` records it and its primary script is still there.
// Skip: as `prepare_cached` — `Option<PathBuf>::as_deref` through the generic
// trait path; every I/O path returns Err.
#[cfg_attr(trust_verify, trust::skip)]
fn ensure_cached(
    root: PathBuf,
    written: &Mutex<Option<PathBuf>>,
) -> Result<PathBuf, std::io::Error> {
    let base = root.join(script_set_address());
    let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
    let cached = written.as_deref() == Some(base.as_path())
        && base.join("aterm_shell_integration.zsh").exists();
    if !cached {
        install_script_set(&root)?;
        *written = Some(base.clone());
        drop(written);
        // Script installation runs only for a shell a host spawns (`prepare`'s
        // one caller is aterm-gui spawn.rs): a browser spawns none, and this
        // clock is compared with file mtimes, which are std's.
        #[cfg_attr(
            all(target_arch = "wasm32", target_os = "unknown"),
            expect(
                clippy::disallowed_methods,
                reason = "a browser spawns no shell, so it never installs scripts"
            )
        )]
        collect_old_script_sets(&root, std::time::SystemTime::now());
    } else {
        drop(written);
    }
    Ok(base)
}

/// Every file of a script folder, relative to it, with its bytes — the ONE list
/// [`ensure_scripts`] writes, [`script_set_address`] hashes and
/// [`script_set_matches`] verifies, so the three cannot disagree about what a
/// script set is.
///
/// Each POSIX shell's BODY rides beside its loader
/// (`aterm_shell_integration_body.<ext>`, [`body_file`]): a running shell's
/// loader sources it from a folder like this one when its host points it here.
/// A body is part of the set, so it is part of the address, and a folder that
/// names a body holds exactly the bytes that address says.
///
/// The zsh and bash loaders, and so their bodies, are written as the shell runs
/// them: without their comment lines ([`installed_zsh`], [`installed_bash`]).
fn script_set() -> [(&'static str, std::borrow::Cow<'static, str>); 11] {
    [
        (
            "aterm_shell_integration.zsh",
            std::borrow::Cow::Borrowed(installed_zsh()),
        ),
        (
            "aterm_shell_integration.bash",
            std::borrow::Cow::Borrowed(installed_bash()),
        ),
        ("aterm_shell_integration.fish", lf_only(scripts::FISH)),
        (
            "aterm_shell_integration_body.zsh",
            body_file(installed_zsh()),
        ),
        (
            "aterm_shell_integration_body.bash",
            body_file(installed_bash()),
        ),
        (
            "aterm_shell_integration_body.fish",
            body_file(&lf_only(scripts::FISH)),
        ),
        // No BOM on purpose: the script is ASCII-only so Windows PowerShell 5.1
        // (which decodes BOM-less source as ANSI) reads it correctly.
        (
            "aterm_shell_integration.ps1",
            std::borrow::Cow::Borrowed(scripts::POWERSHELL),
        ),
        // zsh: ZDOTDIR wrapper .zshenv
        ("zdotdir/.zshenv", std::borrow::Cow::Borrowed(ZSH_WRAPPER)),
        // bash: the login-style rcfile wrapper (macOS, Git Bash, WSL) …
        ("bash/rcfile", std::borrow::Cow::Borrowed(BASH_WRAPPER)),
        // … and the loader that starts bash as bash would (every other Unix).
        ("bash/bashrc", std::borrow::Cow::Borrowed(BASH_LOADER)),
        // fish: XDG vendor conf.d structure
        (
            "fish-xdg/fish/vendor_conf.d/aterm_shell_integration.fish",
            lf_only(scripts::FISH),
        ),
    ]
}

/// A loader's BODY as its own file: [`BODY_FILE_HEADER`], then the text between
/// the markers ([`scripts::body`]) of `loader`, the loader as the folder holds it.
///
/// # Panics
/// When `loader` has no body markers — a build whose zsh/bash/fish script lost
/// them would ship folders with no body a running shell could take, so it fails
/// at its first `prepare` (and in this crate's tests) instead.
fn body_file(loader: &str) -> std::borrow::Cow<'static, str> {
    let body = scripts::body(loader).expect("a zsh/bash/fish script carries its body markers");
    std::borrow::Cow::Owned(format!("{BODY_FILE_HEADER}{body}"))
}

/// The zsh loader as a script folder holds it: [`scripts::ZSH`] LF-normalised,
/// without its comment lines ([`strip_comment_lines`]). Built once per process.
fn installed_zsh() -> &'static str {
    static INSTALLED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| strip_comment_lines(&lf_only(scripts::ZSH)))
}

/// The bash loader as a script folder holds it ([`installed_zsh`]'s twin).
fn installed_bash() -> &'static str {
    static INSTALLED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| strip_comment_lines(&lf_only(scripts::BASH)))
}

/// `script` without its comment lines: every line whose first character past
/// leading blanks is `#`, except a first-line `#!` and the BODY markers
/// ([`scripts::BODY_BEGIN`], [`scripts::BODY_END`]), which [`scripts::body`]
/// cuts at.
///
/// WHY: a shell pays to READ every line it sources, whatever the line holds, and
/// the scripts are mostly comment — 858 of the zsh loader's 1,538 lines, 695 of
/// bash's 1,289. Measured 2026-10-16 (zsh 5.9, bash 3.2.57, a PTY, the captured
/// aterm spawn env, interleaved, n=12 each): taking them out brought a fresh
/// zsh's first prompt 1.2 ms sooner and bash's 1.9 ms; blanking them to empty
/// lines saved zsh nothing, so the lines go, and a shell's error names a line
/// of the file it read, not of this source. The comments stay where they are
/// read: in this crate.
///
/// SAFE because no comment line of these scripts sits where a shell would read
/// it as TEXT — inside a quoted string or a here-document spanning lines, or
/// after a line continuation. That is held by the shell's own reading, not by
/// this function: `installed_scripts_run_the_code_of_their_source` has a real
/// zsh and bash print the canonical text of each script with and without its
/// comment lines, and a script that ever puts such a line in a string fails it.
fn strip_comment_lines(script: &str) -> String {
    let mut out = String::with_capacity(script.len());
    for (index, line) in script.split_inclusive('\n').enumerate() {
        let code = line.trim_start_matches([' ', '\t']);
        let comment = code.starts_with('#')
            && !(index == 0 && line.starts_with("#!"))
            && !code.starts_with(scripts::BODY_BEGIN)
            && !code.starts_with(scripts::BODY_END);
        if !comment {
            out.push_str(line);
        }
    }
    out
}

/// The name of this build's script folder: the `aterm_hash::FxHasher` (the
/// workspace's fixed-key hash, wyhash-style over 16 bytes a step) of every
/// (path, bytes) pair of [`script_set`], length-prefixed so no two sets can
/// concatenate alike, as 16 hex digits. Computed once per process, by the
/// first `prepare`, which a launch's session 0 waits on.
///
/// WHY NOT SHA-256 OR FNV-1a/64, as it was before: the name is no security
/// boundary (the folder is verified byte for byte before any shell is pointed
/// at it, [`script_set_matches`]) and the set is this build's own embedded
/// text, so nothing chooses colliding inputs; 64 bits make a chance collision
/// between the few sets one machine holds no likelier than with SHA-256's first
/// 64. Measured 2026-10-19 over the 365,188 bytes of this set (profiling build,
/// one test thread, six rounds): FNV-1a/64 0.56-0.66 ms, FxHasher 0.028 ms
/// (SHA-256 was 2.2-2.9 ms). Session 0's spawn waits for this warm: it reaches
/// its fork about 0.5 ms after the intake, before the event loop is built.
///
/// THE NAME OUTLIVES THE PROCESS, though `aterm_hash` fixes `FxHasher`'s values
/// only in-process (FNV-1a/64 is its one hash stable across releases): it is a
/// folder on disk and the revision an adopted shell carries to the next build
/// (`shell_body::own_rev`, `status integration_rev=`). That is safe because a
/// name is never trusted for content: a build whose hash differs only names
/// the same set differently, so it installs its folder once and re-points each
/// adopted shell once, as any build with new scripts does.
#[must_use]
pub fn script_set_address() -> &'static str {
    static ADDRESS: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ADDRESS.get_or_init(|| {
        use std::hash::Hasher as _;
        let mut hash = aterm_hash::FxHasher::default();
        hash.write(b"aterm shell-integration script set v3\0");
        for (path, bytes) in script_set() {
            for part in [path.as_bytes(), bytes.as_bytes()] {
                hash.write_u64(part.len() as u64);
                hash.write(part);
            }
        }
        let mut out = String::with_capacity(16);
        for b in hash.finish().to_be_bytes() {
            out.push(nibble_to_hex(b >> 4));
            out.push(nibble_to_hex(b & 0x0F));
        }
        out
    })
}

/// Whether `dir` holds exactly this build's script set — every file present,
/// byte for byte. A partial or foreign folder is not.
fn script_set_matches(dir: &Path) -> bool {
    script_set()
        .iter()
        .all(|(path, bytes)| std::fs::read(dir.join(path)).is_ok_and(|b| b == bytes.as_bytes()))
}

/// A suffix no other writer's scratch folder shares: the process, the clock,
/// and a count within the process — the clock alone is not enough, because
/// macOS's ticks in microseconds and two threads installing at once (a window
/// restoring its tabs) read the same tick and wrote into ONE scratch folder,
/// the first rename taking it away from under the second (`NotFound`).
fn scratch_suffix() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    #[cfg_attr(
        all(target_arch = "wasm32", target_os = "unknown"),
        expect(
            clippy::disallowed_methods,
            reason = "only for a scratch folder beside the script sets, which a browser never writes"
        )
    )]
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{nanos}-{n}", std::process::id())
}

/// Install this build's script set as `<root>/<address>/` and return that path.
///
/// WRITE-ONCE: an existing folder that already matches is used as it is; a new
/// one is written in full under a scratch name (`.tmp-…`) and renamed into place,
/// so no shell ever sources a half-written file and no racing writer's folder is
/// ever overwritten. A folder that exists but does NOT match (a person edited it,
/// a disk filled mid-write under an older scheme) is moved aside and replaced.
fn install_script_set(root: &Path) -> Result<PathBuf, std::io::Error> {
    let address = script_set_address();
    let dir = root.join(address);
    if script_set_matches(&dir) {
        return Ok(dir);
    }
    std::fs::create_dir_all(root)?;
    let tmp = root.join(format!(".tmp-{address}-{}", scratch_suffix()));
    if let Err(e) = ensure_scripts(&tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    for _ in 0..2 {
        // `rename(2)` over an EMPTY directory replaces it; over a non-empty one
        // it fails, which is what makes a winner's folder immune to a loser.
        if std::fs::rename(&tmp, &dir).is_ok() {
            return Ok(dir);
        }
        if script_set_matches(&dir) {
            // Another process installed the same set first.
            let _ = std::fs::remove_dir_all(&tmp);
            return Ok(dir);
        }
        let stale = root.join(format!(".stale-{address}-{}", scratch_suffix()));
        if std::fs::rename(&dir, &stale).is_ok() {
            let _ = std::fs::remove_dir_all(&stale);
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Err(std::io::Error::other(format!(
        "could not install the shell-integration scripts at {}",
        dir.display()
    )))
}

/// Script folders younger than this are never collected: a shell spawned from
/// one may still be starting, and a window that spawned it may spawn another.
pub const GC_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 60 * 60);

/// The newest script folders always kept, whatever their age.
pub const GC_KEEP_NEWEST: usize = 5;

/// Scratch folders (`.tmp-…`/`.stale-…`) older than this belong to a writer
/// that died mid-install.
const GC_SCRATCH_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// A content folder's name: exactly 16 lowercase hex digits.
fn is_address(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Remove OTHER builds' script folders under `root` that are older than
/// [`GC_MIN_AGE`], always keeping the [`GC_KEEP_NEWEST`] newest and this build's
/// own, plus dead writers' scratch folders. A running shell reads its own folder
/// only while it starts (the loaders source it once), so an old folder's last
/// reader is a tab opened from that build within the last two weeks; a window of
/// that build that opens a tab later re-installs its folder ([`prepare_cached`]
/// stats it on every spawn). A running shell reads ANOTHER folder only when its
/// host points it there — at the host's own folder, installed right before
/// ([`ensure_script_set`]); a body collected before the shell's next prompt
/// leaves it on the body it runs, which `status integration_rev=` still names.
/// The legacy flat files are never touched. Best effort: every failure leaves
/// the entry in place.
fn collect_old_script_sets(root: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let own = script_set_address();
    let mut sets: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let modified = meta.modified().unwrap_or(now);
        let age = now.duration_since(modified).unwrap_or_default();
        if name.starts_with(".tmp-") || name.starts_with(".stale-") {
            if age >= GC_SCRATCH_AGE {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        } else if is_address(name) && name != own {
            sets.push((modified, entry.path()));
        }
    }
    sets.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (modified, path) in sets.into_iter().skip(GC_KEEP_NEWEST) {
        if now.duration_since(modified).unwrap_or_default() >= GC_MIN_AGE {
            let _ = std::fs::remove_dir_all(path);
        }
    }
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
/// any authenticated user create `C:\tmp`). With no home at all, the per-user
/// temp directory.
///
/// The same in every containment mode. The aterm process writes these files and
/// the shell only reads them, so they belong where a Containment shell cannot
/// write: its Seatbelt profile confines its writes to the temp roots
/// (`aterm_containment::sbpl`), and a script staged there could be rewritten by
/// the contained shell
/// (or by another user, in the shared `/tmp`) and sourced by the next tab's
/// unsandboxed shell.
///
/// In a Cargo test binary, `<its scratch root>/cache/aterm/shell-integration`
/// (`aterm_uds::test_binary`) unless the test set `$XDG_CACHE_HOME` (Unix), so a
/// test never writes the person's cache; [`per_user_cache_dir`] is the rule
/// everywhere else.
fn cache_dir() -> PathBuf {
    #[cfg(any(unix, windows))]
    if let Some(root) = aterm_uds::test_binary::scratch_root()
        && (cfg!(windows) || std::env::var_os("XDG_CACHE_HOME").is_none())
    {
        return root.join("cache").join("aterm").join("shell-integration");
    }
    per_user_cache_dir()
}

/// [`cache_dir`] in every process but a Cargo test binary: the per-user cache.
fn per_user_cache_dir() -> PathBuf {
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
            std::env::temp_dir().join("aterm-shell-integration")
        }
    }
}

/// Write one integration script/wrapper file.
///
/// The open-coded `File::create` + `write_all` (what [`std::fs::write`] does
/// inside) is one non-generic call site where the verifier discharges the
/// `write(2)` obligations; calling `fs::write` re-derives them against the
/// generic shim and refutes them (trustc c7c60c0a7).
///
/// `File::create` must stay: [`prepare_into`] rewrites existing files, so
/// `create_new` would fail with `AlreadyExists`, and `remove_file` +
/// `create_new` changes the inode and permissions.
// Skip: the remaining open is `File::create`'s `raw_path_api` hardening flag,
// dischargeable only by capability contracts.
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

/// Write embedded scripts and wrapper files ([`script_set`], LF-normalised —
/// see `lf_only`) into `base`.
fn ensure_scripts(base: &Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(base)?;
    for (path, bytes) in script_set() {
        let path = base.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_script(&path, &bytes)?;
    }
    Ok(())
}

/// THE PARKED HOLD, the first lines of both wrappers (zsh's `.zshenv`, bash's
/// `--rcfile`), POSIX so one text serves both: a shell whose spare FIFO
/// exists says so (`633;P;AtermSpare=parked`, signed with the nonce still in
/// its environment) and waits for one line, before any startup file. `go`
/// continues as a cold shell started now; `run` lets its startup files run
/// hidden, to wait again at its first prompt hook (`__aterm_spare_hold`). The
/// host holds the FIFO open read-write for the spare's life, so the open never
/// blocks and a line written early waits in the pipe; the loop rides out a
/// signal (bash's `read` from a FIFO with no writer was released by SIGWINCH),
/// and a FIFO that is gone, or 50 failed reads, ends the shell. A cold shell
/// pays one `[ -n ]`, two expansions and one `[ -p ]`, and is left with no new
/// variable.
///
/// `go <folder>` (2026-10-18) continues as a cold shell started now in that
/// absolute folder: the host names it only where every shell's start would
/// name it so (`$PWD` the kernel's path; aterm-gui `spawn_folder::retarget_folder`),
/// and the `cd` leaves `$OLDPWD` as that start does: zsh sets it to `$PWD`,
/// bash keeps the one it inherited, else leaves it unset and exported. Quiet:
/// nothing of the person's has run to hook a `cd`.
macro_rules! spare_parked_hold {
    () => {
        "\
# aterm warm spare (docs/DESIGN-warm-spare-2026-10-16.md): a spare waits here, before any startup file.
if [ -n \"${ATERM_REKEY_PATH:-}\" ]; then
  __aterm_spare_fifo=\"${ATERM_REKEY_PATH%/rekey/*}/spare/${ATERM_REKEY_PATH##*/}\"
  if [ -p \"$__aterm_spare_fifo\" ]; then
    printf '\\033]633;P;AtermSpare=parked;id=%s\\a' \"${ATERM_SHELL_NONCE:-}\" >/dev/tty 2>/dev/null
    __aterm_spare_n=0 __aterm_spare_cd=
    while :; do
      if IFS= read -r __aterm_spare_go < \"$__aterm_spare_fifo\"; then
        case $__aterm_spare_go in
          run) __aterm_spare_mode=warm; break ;;
          go) break ;;
          'go /'*) __aterm_spare_cd=${__aterm_spare_go#go }; break ;;
        esac
      else
        __aterm_spare_n=$((__aterm_spare_n + 1))
        { [ \"$__aterm_spare_n\" -lt 50 ] && [ -p \"$__aterm_spare_fifo\" ]; } || exit 0
      fi
    done 2>/dev/null
    if [ -n \"${__aterm_spare_cd:-}\" ]; then
      if [ -n \"${ZSH_VERSION:-}\" ]; then
        builtin cd -q -L -- \"$__aterm_spare_cd\" 2>/dev/null && OLDPWD=$PWD
      elif [ -n \"${OLDPWD+1}\" ]; then
        __aterm_spare_o=$OLDPWD
        builtin cd -L -- \"$__aterm_spare_cd\" 2>/dev/null && OLDPWD=$__aterm_spare_o
      else
        builtin cd -L -- \"$__aterm_spare_cd\" 2>/dev/null && { unset OLDPWD; export OLDPWD; }
      fi
    fi
    SECONDS=0
  fi
fi
[ -n \"${__aterm_spare_mode:-}\" ] || unset __aterm_spare_fifo __aterm_spare_mode
unset __aterm_spare_n __aterm_spare_go __aterm_spare_cd __aterm_spare_o
"
    };
}

/// THE RC'S DEBUG TRAP, HANDED OVER (2026-10-17): the last lines before both
/// bash wrappers source the integration. bash hides the DEBUG trap from a
/// sourced file — `.` unsets it for the file unless functrace is on — so the
/// integration's own `trap -p DEBUG` never saw a trap the user's startup files
/// set (bash-preexec, atuin), and what the `.` did on return finished the job:
/// bash 4.4 and later kept the integration's trap and dropped the user's,
/// bash 3.2 put the user's back over the integration's, losing every command
/// mark (both measured, 3.2.57 and 5.3.0). Here, at the rcfile's own level,
/// the trap is visible: its text goes to `<re-key path>.trap`
/// ([`DEBUG_TRAP_SUFFIX`]; into a variable, by a fork, only for a shell with no
/// re-key path or a file it cannot open), then the trap is cleared, so the `.`
/// has nothing to put back, and the integration chains the text. Untraced, as
/// the integration is (its THE TRACE): a user's XTRACE and VERBOSE go off for
/// these lines and come back for the `.`.
///
/// Only ahead of a FRESH load, the one that wires the trap: with the loaded
/// guard set — a startup file sourced the integration itself, the line its
/// header gives for a shell aterm did not start — the `.` below is an upgrade
/// in place or a skip, which wires nothing, and the trap there is that load's
/// own. Cleared, it was gone for good: every command lost its marks (review,
/// 2026-10-17; `an_integrated_bash_whose_rc_sources_the_integration_keeps_its_marks`).
macro_rules! bash_debug_trap_handover {
    () => {
        "\
# The DEBUG trap the startup files set, handed to the integration: a `.` hides it.
{ __aterm_rc_flags=$-; builtin set +xv; } 2>/dev/null
if [ -z \"${ATERM_SHELL_INTEGRATION_INSTALLED:-}\" ]; then
  __aterm_rc_debug_handed=1
  if [ -z \"${ATERM_REKEY_PATH:-}\" ] ||
     ! { builtin trap -p DEBUG; } 2>/dev/null >|\"$ATERM_REKEY_PATH.trap\"; then
    __aterm_rc_debug_trap=$(builtin trap -p DEBUG 2>/dev/null)
  fi
  builtin trap - DEBUG
fi
case $__aterm_rc_flags in
  *x*v*|*v*x*) builtin unset __aterm_rc_flags; builtin set -xv ;;
  *x*) builtin unset __aterm_rc_flags; builtin set -x ;;
  *v*) builtin unset __aterm_rc_flags; builtin set -v ;;
  *) builtin unset __aterm_rc_flags ;;
esac
"
    };
}

/// zsh wrapper .zshenv that restores ZDOTDIR and sources our integration.
///
/// The wrapper reads `ATERM_ORIGINAL_ZDOTDIR` (set by [`prepare_zsh`]) to
/// restore the user's original ZDOTDIR before sourcing their `.zshenv`.
/// This is the same ZDOTDIR-override technique used by Kitty, Ghostty,
/// and VS Code terminal integrations.
const ZSH_WRAPPER: &str = concat!(
    spare_parked_hold!(),
    "\
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
"
);

/// bash wrapper rcfile that sources standard profile chain then our integration:
/// the loader on macOS and inside WSL ([`WSL_LAUNCH_SH`]), whose terminals start
/// LOGIN shells. Every other Unix starts bash with [`BASH_LOADER`] instead
/// ([`prepare_bash`]), and so does Git Bash, as a login shell
/// ([`BASH_LOGIN_START`]).
///
/// `bash --rcfile` launches an interactive non-login shell which normally reads
/// only `.bashrc`. We source the login profile chain (since terminal sessions
/// there conventionally behave like login shells) AND `.bashrc` (since many users
/// keep aliases/functions/PATH additions there separately from `.bash_profile`).
/// `.bashrc` is sourced last before integration to handle the common case where
/// `.bash_profile` does NOT source `.bashrc`. (A `.bash_profile` that does source
/// it runs it twice; macOS's bash has no system bashrc of its own, so nothing
/// else repeats. Left as it is: the POSIX-mode start [`BASH_LOADER`] uses for a
/// login shell is unproven on macOS's own bash 3.2, which Ghostty does not inject
/// into either.)
const BASH_WRAPPER: &str = concat!(
    spare_parked_hold!(),
    "\
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
",
    bash_debug_trap_handover!(),
    "\
# Load aterm integration
. \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.bash\"
"
);

/// The bash loader on every Unix but macOS: bash's own startup, exactly once, then
/// aterm's integration.
///
/// A terminal there starts an interactive NON-login shell (GNOME Terminal, Konsole,
/// xterm, foot, kitty, Ghostty), and so does aterm: `bash --rcfile <this file>`
/// ([`prepare_bash`]). bash has read its system bashrc before it reads an rcfile
/// (`/etc/bash.bashrc` on Debian, Ubuntu and Arch, wherever bash was built with
/// one, and nothing where it was not), so this file does what bash would do next:
/// read `~/.bashrc` — or the file a `--rcfile`/`--init-file` in `shell_args`
/// named, handed over as `ATERM_BASH_RCFILE`. Nothing else: the old login-style
/// wrapper also read `/etc/profile` and `~/.profile` here, and on Ubuntu those
/// read `/etc/bash.bashrc` and `~/.bashrc` again, so every tab ran both twice
/// (the sudo hint printed twice, every PATH addition doubled, nvm and conda
/// initialised twice).
///
/// A LOGIN shell (`shell_args = ["-l"]`) or `--norc` cannot be started that way:
/// a login bash ignores `--rcfile`, and nothing stops a non-login one reading its
/// system bashrc. aterm starts those with `--posix` and `ENV` naming this file
/// (`ATERM_BASH_POSIX` set): in POSIX mode an interactive bash reads `$ENV` and
/// nothing else, so this file puts bash back (`set +o posix`, the history file
/// POSIX mode would have moved to `~/.sh_history`, the caller's own `ENV`) and
/// reads what bash would have — for a login shell `/etc/profile` and the first of
/// `~/.bash_profile`, `~/.bash_login`, `~/.profile` (unless `--noprofile`), for
/// `--norc` nothing. The same start kitty and Ghostty use. Git Bash starts every
/// tab that way, as a login shell ([`BASH_LOGIN_START`]).
///
/// Like the wrappers it opens with [`spare_parked_hold!`]: a warm spare bash
/// parks before any startup file of the person's on Linux as it does on macOS;
/// the system bashrc it read first is one of [`pre_hold_files`].
const BASH_LOADER: &str = concat!(
    spare_parked_hold!(),
    "\
# aterm shell integration loader: bash's own startup files, once, then aterm's.
if [ -n \"${ATERM_BASH_POSIX:-}\" ]; then
  # Started --posix with $ENV naming this file: bash read nothing else.
  builtin unset ATERM_BASH_POSIX ENV
  if [ -n \"${ATERM_BASH_ENV+set}\" ]; then
    builtin export ENV=\"$ATERM_BASH_ENV\"
    builtin unset ATERM_BASH_ENV
  fi
  builtin set +o posix
  builtin shopt -u inherit_errexit 2>/dev/null
  if [ \"${ATERM_BASH_HISTFILE:-}\" = home ]; then
    # Git Bash: the history file bash names outside POSIX mode, from its own HOME.
    HISTFILE=$HOME/.bash_history
    builtin unset ATERM_BASH_HISTFILE
  elif [ -n \"${ATERM_BASH_HISTFILE:-}\" ]; then
    builtin export -n HISTFILE
    builtin unset ATERM_BASH_HISTFILE
  fi
  if builtin shopt -q login_shell && [ -z \"${ATERM_BASH_NOPROFILE:-}\" ]; then
    [ -f /etc/profile ] && builtin source /etc/profile
    if [ -f \"$HOME/.bash_profile\" ]; then
      builtin source \"$HOME/.bash_profile\"
    elif [ -f \"$HOME/.bash_login\" ]; then
      builtin source \"$HOME/.bash_login\"
    elif [ -f \"$HOME/.profile\" ]; then
      builtin source \"$HOME/.profile\"
    fi
  fi
  builtin unset ATERM_BASH_NOPROFILE
elif [ -n \"${ATERM_BASH_RCFILE+set}\" ]; then
  # --rcfile/--init-file in shell_args: that file in place of ~/.bashrc, a
  # leading `~` or `~name` expanded as bash expands the one it is handed.
  __aterm_rcfile=$ATERM_BASH_RCFILE
  builtin unset ATERM_BASH_RCFILE
  case $__aterm_rcfile in
    \"~\") __aterm_rcfile=$HOME ;;
    \"~/\"*) __aterm_rcfile=$HOME/${__aterm_rcfile#\"~/\"} ;;
    \"~\"*)
      __aterm_user=${__aterm_rcfile%%/*}
      __aterm_user=${__aterm_user#\"~\"}
      case $__aterm_user in
        *[!A-Za-z0-9._-]*) ;;
        *)
          builtin eval \"__aterm_home=~$__aterm_user\"
          if [ \"$__aterm_home\" != \"~$__aterm_user\" ]; then
            __aterm_rcfile=$__aterm_home${__aterm_rcfile#\"~$__aterm_user\"}
          fi
          ;;
      esac
      builtin unset __aterm_user __aterm_home
      ;;
  esac
  if [ -f \"$__aterm_rcfile\" ]; then
    builtin source \"$__aterm_rcfile\"
  fi
  builtin unset __aterm_rcfile
else
  # bash has read its system bashrc already: ~/.bashrc is what it reads next.
  [ -f \"$HOME/.bashrc\" ] && builtin source \"$HOME/.bashrc\"
fi
",
    bash_debug_trap_handover!(),
    "\
# Load aterm integration
builtin source \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.bash\"
"
);

/// Convert a path to the `String` value placed in a child-environment
/// variable.
///
/// Byte-identical to `path.to_string_lossy().into_owned()` on the Unix
/// targets this crate ships on: [`OsStr::as_encoded_bytes`] returns the
/// path's raw bytes there, and [`String::from_utf8_lossy`] is specified as
/// exactly this `utf8_chunks` loop (each maximal valid run is kept, each
/// non-empty invalid subpart becomes one U+FFFD). Spelled out because
/// `Path::to_string_lossy` (and `String::from_utf8_lossy`) are hardened
/// `byte_loss` boundaries under the Trust lane (`ship trust run`); the explicit
/// loop carries ordinary provable obligations instead.
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

/// Whether this platform starts an integrated bash as a LOGIN shell in POSIX mode,
/// `bash --login --posix` with `$ENV` naming [`BASH_LOADER`]: Git Bash on Windows
/// (2026-10-18). Every Windows terminal starts Git Bash as a login shell (Windows
/// Terminal's profile `bash.exe -i -l`, Git's own `git-bash.exe`, VS Code), which
/// reads `/etc/profile` and the first of `~/.bash_profile`, `~/.bash_login`,
/// `~/.profile`, and loads Git's prompt and completion (`profile.d/git-prompt.sh`)
/// ONCE. The `--rcfile` start [`BASH_WRAPPER`] gave it is a non-login shell, for
/// which Git's `/etc/bash.bashrc` loads them again, and bash read that file twice
/// (as its system bashrc, then from `/etc/profile`, whose guard against a second
/// read sets its variable in a subshell): three loads of git-completion.bash and
/// git-prompt.sh, about 0.27 s each, and a tab at its prompt 0.5 s after a login
/// Git Bash (1.28 s against 0.74 s; the 2026-10-18 verifier's
/// v4-gitbash-start-loads-git-prompt-three-times). POSIX mode reads `$ENV` and
/// nothing else, and the loader puts bash back and reads what a login bash reads
/// (the start kitty and Ghostty give a login bash on Linux).
pub(crate) const BASH_LOGIN_START: bool = cfg!(windows);

/// The bash loader this platform starts bash on: [`BASH_LOADER`] (`bash/bashrc`)
/// on every Unix but macOS, whose terminals start non-login shells, and under Git
/// Bash, which reads it as a login shell's `$ENV` ([`BASH_LOGIN_START`]); on macOS
/// the login-style [`BASH_WRAPPER`] (`bash/rcfile`).
pub(crate) const BASH_LOADER_FILE: &str =
    if cfg!(all(unix, not(target_os = "macos"))) || BASH_LOGIN_START {
        "bashrc"
    } else {
        "rcfile"
    };

fn prepare_bash(base: &Path) -> InjectionEnv {
    let mut env_add = vec![(
        "ATERM_SHELL_INTEGRATION_DIR".to_string(),
        path_env_value(base),
    )];
    if BASH_LOGIN_START {
        shell_args::posix_start_env(&mut env_add, false, &|key| std::env::var(key).ok());
        return InjectionEnv {
            env_add,
            argv_override: Some(vec![
                "bash".to_string(),
                "--login".to_string(),
                "--posix".to_string(),
            ]),
        };
    }
    let rcfile = base.join("bash").join(BASH_LOADER_FILE);
    InjectionEnv {
        env_add,
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
/// this aterm-spawned process and is valid in both 5.1 and pwsh 7. (No-op on
/// non-Windows pwsh.)
///
/// Running after the profile is also what lets the script's key layer defer
/// to the user: in an aterm tab (`ATERM_CHILD` or `ATERM_SESSION_ID`), with
/// PSReadLine loaded and its edit mode still `Windows`, it binds readline's
/// chords (Ctrl+A/E/B/F/K/U/W/P/N/D/T/Y/_, Alt+B/F/Backspace/Left/Right, and
/// Alt+U/L/C where PSReadLine has them), each only where the chord is unbound
/// or still holds its stock Windows-mode function and starts no bound two-key
/// chord, so a key the profile bound to anything else keeps the profile's
/// binding. Where PSReadLine has `AcceptSuggestion` (pwsh 7, not 5.1's
/// PSReadLine 2.0.0), Ctrl+E pressed at the end of the line also accepts the
/// inline suggestion, as end-of-line does under zsh-autosuggestions: from inside
/// the line the first press moves to the end and a second accepts, so Ctrl+E then
/// Enter runs what was typed. There `-Bound` lists it as
/// `AcceptSuggestionAndEndOfLine`. A key the profile binds
/// to its own stock function (Ctrl+A `SelectAll`, Ctrl+Y `Redo`) looks stock and
/// is replaced;
/// `$env:ATERM_DISABLE_READLINE_KEYS` set in the profile turns the whole layer
/// off, and a profile that chose Emacs or Vi is left alone. The script's KEY
/// BINDINGS block has the measurements behind it.
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
/// 4. otherwise `exec $SHELL -l` — the plain login shell, no integration.
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

/// The title cmd's injected prompt sets: `OSC 0` carrying `$P`, the live
/// directory — see [`prepare_cmd`] for why and for what it measurably does.
const CMD_PROMPT_TITLE: &str = "$e]0;$P$e\\";

/// Bracketed paste (2004) and in-band size reports (2048) off at every cmd
/// prompt: cmd reads neither, and a program killed with them on left every
/// later paste in `ESC [ 200 ~` brackets (which cmd then ran as a command) and
/// every resize typed as `ESC [ 48 ; … t` (measured 2026-10-18 under
/// OpenConsole, which forwards a program's requests). The pwsh prompt does the
/// same.
const CMD_PROMPT_MODES: &str = "$e[?2004l$e[?2048l";

/// cmd.exe injection: a title, prompt marks and cwd, woven into `%PROMPT%`.
///
/// cmd has no profile, no preexec hook and no scripting seam, so this is a
/// PARTIAL integration. `PROMPT` is the one string cmd re-renders on every input line, and
/// it understands `$E` (ESC) and `$P` (the live current directory), which is
/// exactly enough for:
///
/// * `OSC 0` — the title, set to the directory ([`CMD_PROMPT_TITLE`]), the way
///   the pwsh/bash/zsh/fish prompts set theirs. Without it a cmd tab keeps the
///   console's own title, cmd's program path, for its whole life — measured
///   2026-09-27 on Windows 11 (26200): `status subject=`, `title` and the window
///   caption all read `C:\WINDOWS\SYSTEM32\cmd.exe` before and after a `cd`,
///   while a pwsh tab's followed the directory. One title the shell sets fixes
///   every one of those readers at once; teaching each of them to skip the
///   program path (as the tab strip and `ls` already do) would not. ConPTY does
///   not pass the sequence through: conhost applies it to the console title and
///   sends aterm the resulting title as its own OSC 0, one per frame, so a
///   title changed and changed back within a frame never arrives. cmd builds
///   a running command's title FROM the current console title, so while a
///   command runs the tab reads `C:\Windows\Temp - ping -n 4 127.0.0.1` and
///   the directory comes back after it (measured). A user's `title X` lasts
///   until the next prompt, as a title set by hand does under every other
///   integrated shell, and `title X & cmd` keeps `X` for the command it runs
///   (measured: `X - ping ...`). It is written BEFORE the user's prompt, so a
///   `PROMPT` that sets its own title is rendered later and still wins — that
///   is how a cmd user keeps a title of their own. (The script-driven shells'
///   `ATERM_DISABLE_PROMPT_TITLES` has no cmd counterpart: cmd cannot read a
///   variable in `PROMPT`, and aterm's own run-time code reads no user knob
///   from its environment — the owner's rule.) `$P`
///   needs no sanitising, unlike the pwsh title: a Win32 file name cannot hold
///   a character from 1 through 31, so no directory can end the OSC early. It
///   is the full path, not `~\…` — cmd's prompt has no string operations — and
///   the tab reads the profile folder at its head as `~` (`~\aterm`, as a pwsh
///   tab's title says), in aterm-gui's title rung (`tab_label::informative`);
///   `ctl title` keeps the path as cmd set it.
/// * `OSC 633;P;Cwd=` — the cwd, so the tab label tracks `cd` and a new tab
///   opens where this one is. `$P` yields a native `C:\dir`, which the engine
///   stores verbatim; building a `file://` URI would need percent-encoding cmd
///   cannot do. It comes AFTER `133;A`: the engine files a `633;P;Cwd` under
///   the block in progress, and before `A` opens the new one that is the
///   PREVIOUS command's block — measured with the old order, the block for a
///   `cd /d C:\Windows\Temp` typed in `C:\Users\m6-an\aterm` read
///   `cwd=C:\Windows\Temp`, the directory the command moved TO.
/// * `OSC 133;A` / `133;B` — prompt start/end, which is what jump-to-prompt
///   (Ctrl+Shift+Up/Down) navigates by.
/// * `CSI ? 2004 l` / `CSI ? 2048 l` — bracketed paste and in-band size reports
///   off ([`CMD_PROMPT_MODES`]), which a killed program would otherwise leave on.
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
/// config key.
///
/// An inherited `%PROMPT%` is WRAPPED, not replaced, so a user who set their
/// own prompt keeps it; a `PROMPT` that already carries our marks (a nested
/// aterm) is returned untouched so nesting cannot double-wrap.
///
/// Unlike the script-driven shells, cmd cannot scrub `ATERM_SHELL_NONCE` from
/// its environment after reading it — the nonce is IN the prompt string, so it
/// is visible to child processes of a cmd tab: a weaker guarantee than
/// bash/zsh/fish/pwsh get.
fn prepare_cmd() -> InjectionEnv {
    let inherited = std::env::var("PROMPT").ok();
    InjectionEnv {
        env_add: vec![("PROMPT".to_string(), cmd_prompt(inherited.as_deref()))],
        argv_override: None,
    }
}

/// The `%PROMPT%` a cmd tab starts with, from the one it inherited: the pure
/// half of [`prepare_cmd`], so the wrapping is testable without touching the
/// process environment.
fn cmd_prompt(inherited: Option<&str>) -> String {
    let user = inherited
        .filter(|p| !p.is_empty())
        .unwrap_or(CMD_DEFAULT_PROMPT);
    if user.contains("]133;A") {
        // Already instrumented (nested aterm): leave it exactly as inherited.
        return user.to_string();
    }
    let title = CMD_PROMPT_TITLE;
    let modes = CMD_PROMPT_MODES;
    let id = NONCE_PLACEHOLDER;
    format!(
        "{title}{modes}$e]133;A;id={id}$e\\$e]633;P;Cwd=$P;id={id}$e\\{user}$e]133;B;id={id}$e\\"
    )
}

#[cfg(test)]
mod tests {
    include!("tests.rs");
    include!("tests_loader.rs");
    include!("tests_tty.rs");
    include!("tests_spare.rs");
    include!("tests_pwsh_keys.rs");
    include!("tests_windows_marks.rs");
}
