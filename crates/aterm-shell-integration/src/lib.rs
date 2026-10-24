// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

//! Shell integration injection for aterm.
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

/// The shell integration scripts, compiled in.
pub mod scripts {
    /// zsh shell integration (OSC 7/133 + prompt override).
    pub const ZSH: &str = include_str!("scripts/aterm_shell_integration.zsh");
    /// bash shell integration (OSC 7/133 + prompt override).
    pub const BASH: &str = include_str!("scripts/aterm_shell_integration.bash");
    /// fish shell integration (OSC 7/133 + prompt override).
    pub const FISH: &str = include_str!("scripts/aterm_shell_integration.fish");
    /// PowerShell / pwsh shell integration (OSC 7/133 + the readline key layer).
    pub const POWERSHELL: &str = include_str!("scripts/aterm_shell_integration.ps1");

    /// Sourced once by the zsh body (`aterm_shell_integration_old_bin.zsh`) when upgrading an already-running shell in place (`__aterm_old_bin_off` unset) to remove any legacy `~/.aterm/bin` PATH entry.
    pub const ZSH_OLD_BIN: &str = include_str!("scripts/aterm_shell_integration_old_bin.zsh");
    /// The bash twin of [`ZSH_OLD_BIN`].
    pub const BASH_OLD_BIN: &str = include_str!("scripts/aterm_shell_integration_old_bin.bash");
    /// The fish twin of [`ZSH_OLD_BIN`].
    pub const FISH_OLD_BIN: &str = include_str!("scripts/aterm_shell_integration_old_bin.fish");

    /// Marker comment opening the live-reloadable body section of the zsh, bash, and fish scripts.
    pub const BODY_BEGIN: &str = "# @@ATERM-INTEGRATION-BODY-BEGIN@@";

    /// Marker comment closing the body section ([`BODY_BEGIN`]).
    pub const BODY_END: &str = "# @@ATERM-INTEGRATION-BODY-END@@";

    /// Extract the reloadable body of `script` ([`ZSH`], [`BASH`], or [`FISH`]) from the start of its [`BODY_BEGIN`] line through the end of its [`BODY_END`] line, or `None` if absent.
    #[must_use]
    #[inline]
    pub fn body(script: &str) -> Option<&str> {
        let begin = script.find(BODY_BEGIN)?;
        let end = begin + script[begin..].find(BODY_END)?;
        let end = script[end..]
            .find('\n')
            .map_or(script.len(), |nl| end + nl + 1);
        Some(&script[begin..end])
    }
}

const BODY_FILE_HEADER: &str = "\
# aterm shell integration — the BODY alone (the LOADER / BODY split, 2026-09-26).
#
# The text between the markers of aterm_shell_integration.<ext> in this folder,
# byte for byte. A running shell's loader sources it when the host that owns the
# shell points it at this folder (`__aterm_body_check`); it relies on the
# loader's state, so it is never sourced by hand.
";

/// File stem for standalone body scripts (`aterm_shell_integration_body.<ext>`).
pub const BODY_FILE_STEM: &str = "aterm_shell_integration_body";

/// Environment variable naming the per-session body pointer file.
pub const BODY_POINTER_VAR: &str = "ATERM_INTEGRATION_POINTER";

/// OSC 633 `P` property key signed before every prompt (`633;P;AtermIntegration=<16-hex>;id=<nonce>`).
pub const INTEGRATION_REV_KEY: &str = "AtermIntegration";

/// OSC 633 `P` property key signed alongside OSC 7 before each prompt (`633;P;AtermCwd=<URI>;id=<nonce>`) to authenticate the shell's CWD report while leaving plain OSC 7 unnonced.
pub const CWD_REPORT_KEY: &str = "AtermCwd";

/// Whether `shell` has a loader that supports live body reloading via [`BODY_POINTER_VAR`].
#[must_use]
#[inline]
pub const fn has_body_loader(shell: ShellType) -> bool {
    matches!(shell, ShellType::Zsh | ShellType::Bash | ShellType::Fish)
}

/// Whether `rev` is a valid script folder address (16 lowercase hex digits, [`script_set_address`]).
#[must_use]
#[inline]
pub const fn is_integration_rev(rev: &str) -> bool {
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
    /// PowerShell / pwsh (injected via `-NoExit -Command` argv override that dot-sources the embedded `.ps1` after profiles load).
    PowerShell,
    /// `wsl.exe` (injected via `WSLENV` and `wsl.exe --exec`; see [`prepare_wsl`]).
    Wsl,
    /// Windows `cmd.exe` (partial integration via `%PROMPT%`; see [`prepare_cmd`]).
    Cmd,
    /// Unknown shell (no injection available).
    Unknown,
}

impl ShellType {
    /// Detect shell type from a command path (e.g. "/bin/zsh", "bash", `C:\Program Files\PowerShell\7\pwsh.exe`).
    #[must_use]
    #[inline]
    pub fn detect(shell_path: &str) -> Self {
        let tail = shell_path.rsplit(['/', '\\']).next().unwrap_or(shell_path);
        let name = tail.to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "zsh" => Self::Zsh,
            "bash" | "bash5" => Self::Bash,
            "fish" => Self::Fish,
            "pwsh" | "powershell" => Self::PowerShell,
            "wsl" => Self::Wsl,
            "cmd" => Self::Cmd,
            _ => Self::Unknown,
        }
    }

    /// Detect the default interactive shell aterm will launch (`$SHELL` on Unix, `PowerShell` on Windows).
    #[must_use]
    #[inline]
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
    #[inline]
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
    #[inline]
    pub const fn is_known(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// Return `true` when this shell is a POSIX/Unix shell (`Zsh`, `Bash`, or `Fish`).
    #[must_use]
    #[inline]
    pub const fn is_posix_shell(self) -> bool {
        matches!(self, Self::Zsh | Self::Bash | Self::Fish)
    }

    /// Return `true` when this shell is a Windows host shell (`PowerShell`, `Wsl`, or `Cmd`).
    #[must_use]
    #[inline]
    pub const fn is_windows_shell(self) -> bool {
        matches!(self, Self::PowerShell | Self::Wsl | Self::Cmd)
    }
}

/// Encode a shell integration revision record (`rev_hash`, `shell_kind`, `nonce_prefix`) into 16 LE bytes.
#[must_use]
#[inline]
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
#[inline]
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

/// Result of preparing shell integration: what to apply to the child before exec.
#[derive(Debug)]
pub struct InjectionEnv {
    /// Environment variables to set in the child process.
    pub env_add: Vec<(String, String)>,
    /// For bash: override argv to use `--rcfile`.
    pub argv_override: Option<Vec<String>>,
}

/// Byte length of the shell-integration capability-nonce.
pub const SHELL_NONCE_BYTES: usize = 32;

/// Hex-encoded length of the shell-integration nonce.
#[cfg(any(unix, windows))]
pub const SHELL_NONCE_HEX_LEN: usize = SHELL_NONCE_BYTES * 2;

/// A freshly generated 32-byte CSPRNG nonce for OSC 133/633 gating.
#[derive(Debug, Clone)]
pub struct ShellNonce {
    raw: [u8; SHELL_NONCE_BYTES],
    hex: String,
}

impl ShellNonce {
    /// 64-char lowercase hex encoding to set as `ATERM_SHELL_NONCE` in the child shell environment.
    #[must_use]
    #[inline]
    pub const fn hex(&self) -> &str {
        self.hex.as_str()
    }

    /// Consume the nonce and return both halves: the raw bytes authorize the terminal (`Terminal::authorize_shell_integration`), the hex is injected into the child environment.
    #[must_use]
    #[inline]
    pub fn into_parts(self) -> ([u8; SHELL_NONCE_BYTES], String) {
        (self.raw, self.hex)
    }
}

/// Generate a fresh 32-byte shell-integration capability-nonce from the OS CSPRNG.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
#[cfg(any(unix, windows))]
pub fn generate_nonce() -> ShellNonce {
    let mut raw = [0u8; SHELL_NONCE_BYTES];
    fill_nonce_entropy(&mut raw);
    let hex = hex_encode(&raw);
    ShellNonce { raw, hex }
}

/// Fill `buf` from [`aterm_uds::rand::fill`], panicking if the OS CSPRNG is unavailable.
#[cfg(any(unix, windows))]
fn fill_nonce_entropy(buf: &mut [u8; SHELL_NONCE_BYTES]) {
    aterm_uds::rand::fill(buf)
        .expect("OS CSPRNG unavailable: cannot mint a shell-integration nonce");
}

/// Lowercase hex-encode a 32-byte nonce.
#[must_use]
#[cfg(any(unix, windows))]
pub fn hex_encode(bytes: &[u8; SHELL_NONCE_BYTES]) -> String {
    let mut out = String::with_capacity(SHELL_NONCE_HEX_LEN);
    for &b in bytes {
        out.push(nibble_to_hex(b >> 4));
        out.push(nibble_to_hex(b & 0x0F));
    }
    out
}

#[inline]
const fn nibble_to_hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => '0',
    }
}

/// Placeholder token substituted by [`augment_with_nonce`] in `%PROMPT%` for `cmd.exe` (which does not expand `%VAR%` inside an inherited `PROMPT`).
pub const NONCE_PLACEHOLDER: &str = "@ATERM_SHELL_NONCE@";

/// Append `ATERM_SHELL_NONCE=<hex>` to an [`InjectionEnv`]'s env list, and substitute [`NONCE_PLACEHOLDER`] wherever an injector left it.
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

/// Environment variable naming a session's re-key file (`__aterm_rekey_check`).
pub const REKEY_PATH_VAR: &str = "ATERM_REKEY_PATH";

/// Suffix appended to [`REKEY_PATH_VAR`] (`<re-key path>.trap`) for handing over the startup DEBUG trap to the bash integration without a subshell fork (`bash_debug_trap_handover!`).
pub const DEBUG_TRAP_SUFFIX: &str = ".trap";

/// Whether `shell` carries the re-key hook (`__aterm_rekey_check`).
#[must_use]
#[inline]
pub const fn has_rekey_hook(shell: ShellType) -> bool {
    matches!(shell, ShellType::Zsh | ShellType::Bash | ShellType::Fish)
}

/// Subdirectory under the host's control dir holding warm-spare hold FIFOs (`<control dir>/spare/<sid>`, derived by the shell from [`REKEY_PATH_VAR`]).
pub const SPARE_DIR: &str = "spare";

/// OSC 633 `P` key emitted when a spare shell parks before startup files (`633;P;AtermSpare=parked;id=<nonce>`).
pub const SPARE_KEY: &str = "AtermSpare";

/// OSC 633 `P` key completing a shell's start record at its first prompt (`cold` or `spare`).
pub const START_KEY: &str = "AtermStart";

/// Start record's rc directory (`${ZDOTDIR:-$HOME}` for zsh, `$HOME` for bash), 633 `E`-escaped.
pub const START_RC_KEY: &str = "AtermStartRc";

/// Start record's flags (`pre:<n>,s:<n>,j:<n|->,r:<0|1>`).
pub const START_FLAGS_KEY: &str = "AtermStartFlags";

/// Start record's additional startup roots for fish (sysconf and vendor `conf.d` directories joined by `\x00` and 633 `E`-escaped).
pub const START_ROOTS_KEY: &str = "AtermStartRoots";

/// Whether `shell` supports a warm-spare hold at its first prompt (macOS and Linux).
#[must_use]
#[inline]
pub const fn has_spare_hold(shell: ShellType) -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
        && matches!(shell, ShellType::Zsh | ShellType::Bash | ShellType::Fish)
}

/// Whether `shell`'s wrapper supports a parked hold before user startup files (zsh `.zshenv` and bash `--rcfile`).
#[must_use]
#[inline]
pub const fn has_parked_hold(shell: ShellType) -> bool {
    has_spare_hold(shell) && matches!(shell, ShellType::Zsh | ShellType::Bash)
}

/// System startup files `shell` reads before aterm's parked hold (`/etc/zshenv` and `/etc/zsh/zshenv` for zsh; `/etc/bash.bashrc` for bash on Linux).
#[must_use]
#[inline]
pub const fn pre_hold_files(shell: ShellType) -> &'static [&'static str] {
    match shell {
        ShellType::Zsh => &["/etc/zshenv", "/etc/zsh/zshenv"],
        ShellType::Bash if cfg!(target_os = "linux") => &["/etc/bash.bashrc"],
        _ => &[],
    }
}

/// Build a shell snippet that reads a new nonce from `quoted_path`, updates `__aterm_shell_nonce` and `__aterm_id_suffix_str` with stderr silenced (so `set -x` on stderr does not trace the key), and deletes `quoted_path`.
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

/// Build a shell snippet that performs [`typed_rekey`] and upgrades a pre-loader shell's integration in place by reading up to three lines from `quoted_path` (nonce key, script directory, and body pointer path), assigning the key first so the immediate revision mark is signed with it, and sourcing the loader.
#[must_use]
pub fn typed_rekey_with_loader(shell: ShellType, quoted_path: &str) -> Option<String> {
    let p = quoted_path;
    let loader = match shell {
        ShellType::Zsh => "aterm_shell_integration.zsh",
        ShellType::Bash => "aterm_shell_integration.bash",
        ShellType::Fish => "aterm_shell_integration.fish",
        _ => return None,
    };
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

static SCRIPTS_WRITTEN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Prepare shell integration for `shell`.
#[inline]
pub fn prepare(shell: ShellType) -> Result<Option<InjectionEnv>, std::io::Error> {
    prepare_cached(shell, cache_dir(), &SCRIPTS_WRITTEN)
}

/// Install this build's content-addressed script directory and return its path.
#[inline]
pub fn ensure_script_set() -> Result<PathBuf, std::io::Error> {
    ensure_cached(cache_dir(), &SCRIPTS_WRITTEN)
}

/// Compile the zsh loader and body in `dir` to `.zwc` wordcode via `zsh -f`.
/// # Errors
/// When `zsh` cannot be run, `zcompile` fails, or the rename fails.
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

/// [`prepare`] body with the cache root and memoization state injected for testing.
#[cfg_attr(trust_verify, trust::skip)]
fn prepare_cached(
    shell: ShellType,
    root: PathBuf,
    written: &Mutex<Option<PathBuf>>,
) -> Result<Option<InjectionEnv>, std::io::Error> {
    let base = ensure_cached(root, written)?;
    Ok(injection_for(shell, &base))
}

/// Install this build's folder under `root` unless `written` already records it
/// and its primary script still exists on disk.
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

fn script_set() -> [(&'static str, std::borrow::Cow<'static, str>); 14] {
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
        (
            "aterm_shell_integration_old_bin.zsh",
            lf_only(scripts::ZSH_OLD_BIN),
        ),
        (
            "aterm_shell_integration_old_bin.bash",
            lf_only(scripts::BASH_OLD_BIN),
        ),
        (
            "aterm_shell_integration_old_bin.fish",
            lf_only(scripts::FISH_OLD_BIN),
        ),
        (
            "aterm_shell_integration.ps1",
            std::borrow::Cow::Borrowed(scripts::POWERSHELL),
        ),
        ("zdotdir/.zshenv", std::borrow::Cow::Borrowed(ZSH_WRAPPER)),
        ("bash/rcfile", std::borrow::Cow::Borrowed(BASH_WRAPPER)),
        ("bash/bashrc", std::borrow::Cow::Borrowed(BASH_LOADER)),
        (
            "fish-xdg/fish/vendor_conf.d/aterm_shell_integration.fish",
            lf_only(scripts::FISH),
        ),
    ]
}

fn body_file(loader: &str) -> std::borrow::Cow<'static, str> {
    let body = scripts::body(loader).expect("a zsh/bash/fish script carries its body markers");
    std::borrow::Cow::Owned(format!("{BODY_FILE_HEADER}{body}"))
}

fn installed_zsh() -> &'static str {
    static INSTALLED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| strip_comment_lines(&lf_only(scripts::ZSH)))
}

fn installed_bash() -> &'static str {
    static INSTALLED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    INSTALLED.get_or_init(|| strip_comment_lines(&lf_only(scripts::BASH)))
}

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

/// Compute the 16-hex content address of [`script_set`] using `aterm_hash::FxHasher` (0.028 ms over the script set; verified byte-for-byte on disk by [`script_set_matches`]).
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

fn script_set_matches(dir: &Path) -> bool {
    script_set()
        .iter()
        .all(|(path, bytes)| std::fs::read(dir.join(path)).is_ok_and(|b| b == bytes.as_bytes()))
}

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
        // `rename(2)` over a non-empty directory fails, preventing a racing writer
        // from overwriting a completed folder.
        if std::fs::rename(&tmp, &dir).is_ok() {
            return Ok(dir);
        }
        if script_set_matches(&dir) {
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

/// Minimum age before another build's script folder is eligible for garbage collection.
pub const GC_MIN_AGE: std::time::Duration = std::time::Duration::from_secs(14 * 24 * 60 * 60);

/// Number of newest foreign script folders always retained regardless of age.
pub const GC_KEEP_NEWEST: usize = 5;

const GC_SCRATCH_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Whether `name` is a 16-character lowercase hex content address.
#[inline]
const fn is_address(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() != 16 {
        return false;
    }
    let mut i = 0;
    while i < 16 {
        if !matches!(bytes[i], b'0'..=b'9' | b'a'..=b'f') {
            return false;
        }
        i += 1;
    }
    true
}

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

/// Prepare shell integration into an explicit `base` directory without memoization.
pub fn prepare_into(shell: ShellType, base: &Path) -> Result<Option<InjectionEnv>, std::io::Error> {
    if shell == ShellType::Unknown {
        return Ok(None);
    }
    ensure_scripts(base)?;
    Ok(injection_for(shell, base))
}

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

fn cache_dir() -> PathBuf {
    #[cfg(any(unix, windows))]
    if let Some(root) = aterm_uds::test_binary::scratch_root()
        && (cfg!(windows) || std::env::var_os("XDG_CACHE_HOME").is_none())
    {
        return root.join("cache").join("aterm").join("shell-integration");
    }
    per_user_cache_dir()
}

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

/// Write one integration script or wrapper file.
#[cfg_attr(trust_verify, trust::skip)]
fn write_script(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    file.write_all(contents.as_bytes())
}

/// Normalize CRLF to LF for POSIX shell scripts (borrowing when no `\r` is present)
/// in case the build checkout had `core.autocrlf=true`.
#[inline]
fn lf_only(contents: &str) -> std::borrow::Cow<'_, str> {
    if contents.contains('\r') {
        std::borrow::Cow::Owned(contents.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(contents)
    }
}

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

/// Convert a path to a lossy UTF-8 `String` for child environment variables
/// without calling `Path::to_string_lossy` (a hardened `byte_loss` boundary in
/// the Trust lane).
#[cfg_attr(trust_verify, trust::skip)]
fn path_env_value(path: &Path) -> String {
    const EMPTY: &[u8] = &[];
    let bytes = Some(path.as_os_str()).map_or(EMPTY, std::ffi::OsStr::as_encoded_bytes);
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

/// On Windows (Git Bash), start integrated bash as `bash --login --posix` with `$ENV` naming [`BASH_LOADER`] so `/etc/profile` and `git-prompt.sh` run once rather than three times under `--rcfile`.
pub(crate) const BASH_LOGIN_START: bool = cfg!(windows);

/// Bash loader filename under `bash/`: `"bashrc"` ([`BASH_LOADER`]) on non-macOS Unix and Git Bash, `"rcfile"` ([`BASH_WRAPPER`]) on macOS.
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

fn prepare_powershell(base: &Path) -> InjectionEnv {
    InjectionEnv {
        env_add: vec![(
            "ATERM_SHELL_INTEGRATION_DIR".to_string(),
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

/// Environment variable carrying a POSIX cwd across the WSL boundary ([`wsl_cwd_env`]).
pub const WSL_CWD_VAR: &str = "ATERM_WSL_CWD";

const WSLENV_ENTRIES: [&str; 3] = [
    "ATERM_SHELL_INTEGRATION_DIR/p",
    "ATERM_SHELL_NONCE",
    WSL_CWD_VAR,
];

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

/// Merge [`WSLENV_ENTRIES`] ahead of `existing`, deduplicating by variable name.
#[must_use]
fn merge_wslenv(existing: &str) -> String {
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

/// Build the `ATERM_WSL_CWD` env pair when `shell` is [`ShellType::Wsl`] and `cwd` is a non-UNC POSIX-absolute path (`/home/...`).
#[must_use]
#[inline]
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

const CMD_DEFAULT_PROMPT: &str = "$P$G";

const CMD_PROMPT_TITLE: &str = "$e]0;$P$e\\";

const CMD_PROMPT_MODES: &str = "$e[?2004l$e[?2048l";

fn prepare_cmd() -> InjectionEnv {
    let inherited = std::env::var("PROMPT").ok();
    InjectionEnv {
        env_add: vec![("PROMPT".to_string(), cmd_prompt(inherited.as_deref()))],
        argv_override: None,
    }
}

fn cmd_prompt(inherited: Option<&str>) -> String {
    let user = inherited
        .filter(|p| !p.is_empty())
        .unwrap_or(CMD_DEFAULT_PROMPT);
    if user.contains("]133;A") {
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
