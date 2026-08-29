// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Allowlist enforcement primitives for Safety mode.
//!
//! Safety mode is intended to restrict MCP tools, plugins, network targets, and
//! shell commands to an explicit allowlist. This module provides the data model
//! and the fail-closed decision functions:
//!
//! - [`AllowlistConfig`] — the allowlist data, parsed from TOML
//! - [`init_allowlist`] — one-shot initialization (mirrors [`super::init_mode`])
//! - `is_*_allowed()` — per-subsystem decision functions
//!
//! **Status — NOT YET WIRED (honest scope).** These gates are a policy / proof
//! artifact today: NO production code path calls [`init_allowlist`] or consults
//! any `is_*_allowed()` (the only callers are this crate's tests), so Safety
//! mode performs no allowlist confinement at runtime yet. Wiring
//! [`init_allowlist`] into the launcher and invoking the gates at the real
//! MCP / network / process exec sites is a follow-up.
//!
//! The functions are written fail-closed so that wiring them up defaults to
//! deny: with no [`init_allowlist`] call every `is_*_allowed()` returns `false`
//! for `Allowlist`/`Restricted` capability levels.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::ContainmentPolicy;

/// Global allowlist config, set once at startup after [`super::init_mode`].
static ALLOWLIST: OnceLock<AllowlistConfig> = OnceLock::new();

/// Allowlist configuration for Safety mode enforcement.
///
/// Each field lists the identifiers permitted for that subsystem.
/// Empty lists mean "deny all" — this is intentional fail-closed behavior.
#[derive(Debug, Clone, Default)]
pub struct AllowlistConfig {
    /// MCP tool names permitted in Safety mode (e.g. `["read_file", "write_file"]`).
    pub mcp_tools: Vec<String>,
    /// Plugin manifest IDs permitted in Safety mode.
    pub plugins: Vec<String>,
    /// Network targets permitted in Safety mode.
    /// Format: `"host:port"`, `"[ipv6]:port"`, `"host:*"`, `"[ipv6]:*"`,
    /// `"unix:/path"`.
    pub network: Vec<String>,
    /// Process/command executable paths permitted in Safety mode.
    ///
    /// Rules must resolve to absolute filesystem paths. Runtime command names
    /// are resolved through `$PATH` and canonicalized before comparison.
    pub processes: Vec<String>,
}

/// Initialize the global allowlist. INTENDED to be called once at startup, after
/// [`super::init_mode`] — but NOT yet wired: no production code path calls this
/// today (see the module docs). If not called, the gates deny all allowlisted
/// operations (fail-closed).
///
/// # Errors
///
/// Returns [`AllowlistError::AlreadyInitialized`] if called more than once.
pub fn init_allowlist(config: AllowlistConfig) -> Result<(), AllowlistError> {
    ALLOWLIST
        .set(config)
        .map_err(|_| AllowlistError::AlreadyInitialized)
}

/// Whether the Safety-mode allowlist gates are actually consulted by a
/// production enforcement path. `false` today on EVERY platform: the
/// `is_*_allowed()` gates below are fail-closed policy primitives, but no
/// exec / connect / MCP-dispatch site calls them yet (see the module docs), so
/// selecting Safety mode confines nothing at runtime. Launchers use this to
/// print an honest posture line — and MUST NOT let the CLI/explain text imply
/// "allowlisted operations only" is enforced — rather than overstating the
/// posture (house rule: never overstate). Flip this to a real per-subsystem
/// query once the gates are wired at their exec/connect/MCP sites.
// Dead-code allow: this is a posture hook for the launchers, reachable only
// once lib.rs re-exports it (`pub use allowlist::allowlist_enforced;`), the
// same wiring `verify_executable_handle` carries.
#[allow(dead_code)]
#[must_use]
pub const fn allowlist_enforced() -> bool {
    false
}

/// Check if an MCP tool is allowed under the current containment mode.
///
/// Returns `true` for `Full` capability, checks the allowlist for `Allowlist`,
/// and `false` for `Disabled` or unknown variants.
#[must_use]
pub fn is_mcp_allowed(tool_name: &str) -> bool {
    let mode = crate::mode_or_containment();
    match ContainmentPolicy::mcp(mode) {
        crate::McpCapability::Full => true,
        crate::McpCapability::Allowlist => match ALLOWLIST.get() {
            Some(cfg) => cfg.mcp_tools.iter().any(|t| t == tool_name),
            None => false, // fail-closed
        },
        _ => false, // Disabled or unknown
    }
}

/// Check if a plugin is allowed under the current containment mode.
#[must_use]
pub fn is_plugin_allowed(plugin_id: &str) -> bool {
    let mode = crate::mode_or_containment();
    match ContainmentPolicy::plugins(mode) {
        crate::PluginCapability::Full => true,
        crate::PluginCapability::Allowlist => match ALLOWLIST.get() {
            Some(cfg) => cfg.plugins.iter().any(|p| p == plugin_id),
            None => false,
        },
        _ => false,
    }
}

/// Check if a network target is allowed under the current containment mode.
///
/// Supports exact match and `host:*` wildcard (any port on that host).
#[must_use]
pub fn is_network_allowed(target: &str) -> bool {
    let mode = crate::mode_or_containment();
    match ContainmentPolicy::network(mode) {
        crate::NetworkCapability::Full => true,
        crate::NetworkCapability::Allowlist => match ALLOWLIST.get() {
            Some(cfg) => cfg.network.iter().any(|rule| network_matches(rule, target)),
            None => false,
        },
        _ => false, // None or unknown
    }
}

/// Check if a shell/command is allowed under the current containment mode.
///
/// # TOCTOU caveat
///
/// This function uses `canonicalize()` to resolve symlinks at check time.
/// Between this check and the actual `exec()`, the file could be swapped
/// (symlink or rename race). For security-critical callers, use
/// [`verify_executable_fd`] after opening the file descriptor to confirm
/// the resolved path still matches the allowlist at exec time.
#[must_use]
pub fn is_process_allowed(command: &str) -> bool {
    let mode = crate::mode_or_containment();
    match ContainmentPolicy::process(mode) {
        crate::ProcessCapability::Full => true,
        crate::ProcessCapability::Restricted => match ALLOWLIST.get() {
            Some(cfg) => process_allowed_by_config(cfg, command),
            None => false,
        },
        _ => false, // NoFork or unknown
    }
}

/// Verify that an already-opened file descriptor points to an allowlisted
/// executable.
///
/// This closes the TOCTOU window between `is_process_allowed` (which uses
/// `canonicalize()` at check time) and `exec()`. By resolving the path
/// from the open fd, we verify what will actually be executed rather than
/// what was at the path when we checked.
///
/// On macOS, reads from `/dev/fd/{fd}` via `fcntl(F_GETPATH)`.
/// On Linux, reads from `/proc/self/fd/{fd}`.
/// On other platforms, falls back to `false` (fail-closed).
/// On Windows, use [`verify_executable_handle`] / [`open_verified_executable`]
/// instead.
///
/// # Arguments
///
/// * `fd` - An open file descriptor for the executable (opened with `O_RDONLY`).
///
/// Returns `true` if the fd resolves to an allowlisted executable path.
#[cfg(unix)]
#[must_use]
pub fn verify_executable_fd(fd: std::os::unix::io::RawFd) -> bool {
    let mode = crate::mode_or_containment();
    match ContainmentPolicy::process(mode) {
        crate::ProcessCapability::Full => true,
        crate::ProcessCapability::Restricted => match ALLOWLIST.get() {
            Some(cfg) => {
                let Some(path) = fd_to_path(fd) else {
                    return false; // fail-closed
                };
                cfg.processes
                    .iter()
                    .filter_map(|rule| normalize_process_rule(rule))
                    .any(|rule| rule == path)
            }
            None => false,
        },
        _ => false,
    }
}

/// Resolve an open file descriptor to its filesystem path.
///
/// Uses platform-specific mechanisms:
/// - macOS: `fcntl(fd, F_GETPATH)`
/// - Linux: `readlink("/proc/self/fd/{fd}")`
#[cfg(unix)]
fn fd_to_path(fd: std::os::unix::io::RawFd) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: buf is a valid mutable buffer of PATH_MAX bytes, fd is a
        // valid file descriptor. F_GETPATH writes a null-terminated path
        // into the buffer.
        let ret = unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) };
        if ret == -1 {
            return None;
        }
        let nul_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf.truncate(nul_pos);
        // SAFETY: `OsString::from_vec` inlines std's unchecked byte-buffer
        // adoption (`Buf { inner }` construction) into this function's MIR.
        // On Unix an `OsString` is an arbitrary byte sequence with no UTF-8 or
        // interior-NUL invariant, and `buf` is a plain owned `Vec<u8>` holding
        // the kernel-written, NUL-truncated F_GETPATH result, so adopting it
        // wholesale upholds every `OsString` invariant.
        let os_str = std::ffi::OsString::from_vec(buf);
        Some(PathBuf::from(os_str))
    }

    #[cfg(target_os = "linux")]
    {
        let link = format!("/proc/self/fd/{fd}");
        std::fs::read_link(link).ok()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = fd;
        None // fail-closed on unsupported platforms
    }
}

/// Verify that an already-opened file handle refers to an allowlisted
/// executable — the Windows analog of [`verify_executable_fd`].
///
/// Resolves the handle back to its final filesystem path via
/// `GetFinalPathNameByHandleW` (normalized `\\?\` DOS form, symlinks and
/// junctions resolved) and compares it, case-folded, against the
/// canonicalized allowlist rules. Fails closed on any resolution error.
///
/// # Residual TOCTOU (honest scope)
///
/// Unlike Unix `fexecve`, `CreateProcessW` takes a *path*, not a handle, so
/// verification cannot be made atomic with the spawn. The intended pattern is
/// [`open_verified_executable`]: hold the returned handle (opened without
/// `FILE_SHARE_WRITE`/`FILE_SHARE_DELETE`, so the verified file cannot be
/// modified, renamed, or deleted while held) across `CreateProcessW` of the
/// returned final path. The remaining gap: the spawn re-walks that path, and
/// a directory component swapped for a junction between verification and
/// spawn could route it to a different file. Callers needing a stronger
/// guarantee must re-verify the child's image path before resuming a
/// `CREATE_SUSPENDED` child.
// Dead-code allow: reachable only once lib.rs re-exports it
// (`#[cfg(windows)] pub use allowlist::verify_executable_handle;`), the same
// wiring `verify_executable_fd` has for unix.
#[allow(dead_code)]
#[cfg(windows)]
#[must_use]
pub fn verify_executable_handle(handle: std::os::windows::io::RawHandle) -> bool {
    let mode = crate::mode_or_containment();
    match ContainmentPolicy::process(mode) {
        crate::ProcessCapability::Full => true,
        crate::ProcessCapability::Restricted => match ALLOWLIST.get() {
            Some(cfg) => handle_allowed_by_config(cfg, handle),
            None => false,
        },
        _ => false,
    }
}

/// Open `path` for verified execution: `GENERIC_READ` with only
/// `FILE_SHARE_READ` sharing (writes, renames, and deletes to the file are
/// denied while the returned handle is held), verify the handle against the
/// allowlist, and resolve the final path through the handle.
///
/// Returns the open file (hold it across `CreateProcessW`) and the
/// handle-resolved final path (`\\?\`-prefixed) to pass as the application
/// name. `None` means denied or unresolvable (fail-closed). See
/// [`verify_executable_handle`] for the residual TOCTOU gap.
// Dead-code allow: reachable only once lib.rs re-exports it (see
// `verify_executable_handle`).
#[allow(dead_code)]
#[cfg(windows)]
#[must_use]
pub fn open_verified_executable(path: &Path) -> Option<(std::fs::File, PathBuf)> {
    use std::os::windows::io::AsRawHandle;

    let file = open_no_write_share(path)?;
    if !verify_executable_handle(file.as_raw_handle()) {
        return None;
    }
    let final_path = handle_to_path(file.as_raw_handle())?;
    Some((file, final_path))
}

/// Open a file read-only with `FILE_SHARE_READ` as the only sharing mode, so
/// no other open can write, rename, or delete it while the handle is held
/// (execute opens like `CreateProcessW`'s image mapping remain permitted).
#[cfg(windows)]
fn open_no_write_share(path: &Path) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x1;
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .ok()
}

#[cfg(windows)]
fn handle_allowed_by_config(
    cfg: &AllowlistConfig,
    handle: std::os::windows::io::RawHandle,
) -> bool {
    let Some(path) = handle_to_path(handle) else {
        return false; // fail-closed
    };
    cfg.processes
        .iter()
        .filter_map(|rule| normalize_process_rule(rule))
        .any(|rule| windows_paths_equal(&rule, &path))
}

/// Resolve an open file handle to its final filesystem path via
/// `GetFinalPathNameByHandleW` (`FILE_NAME_NORMALIZED | VOLUME_NAME_DOS`,
/// both 0), the Windows analog of [`fd_to_path`].
#[cfg(windows)]
fn handle_to_path(handle: std::os::windows::io::RawHandle) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;

    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: buf is a valid mutable buffer of buf.len() u16s and the
        // handle is only read from. When the buffer is too small the call
        // returns the required size (including the NUL) without writing
        // past the end.
        let len = unsafe {
            GetFinalPathNameByHandleW(handle, buf.as_mut_ptr(), u32::try_from(buf.len()).ok()?, 0)
        };
        if len == 0 {
            return None; // fail-closed (invalid handle, non-file object, ...)
        }
        let len = len as usize;
        if len > buf.len() {
            buf.resize(len, 0);
            continue;
        }
        buf.truncate(len);
        return Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
    }
}

/// Case-folded comparison for canonical Windows paths. Both sides come from
/// filesystem-resolving APIs (`canonicalize` / `GetFinalPathNameByHandleW`),
/// so both carry the `\\?\` prefix and on-disk casing, but NTFS is
/// case-insensitive, so fold defensively.
#[cfg(windows)]
fn windows_paths_equal(a: &Path, b: &Path) -> bool {
    a == b
        || a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

// Hand-rolled kernel32 binding in the same direct style as
// `aterm-pty::windows::ffi` (std already links kernel32).
#[cfg(windows)]
#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetFinalPathNameByHandleW(
        hFile: std::os::windows::io::RawHandle,
        lpszFilePath: *mut u16,
        cchFilePath: u32,
        dwFlags: u32,
    ) -> u32;
}

/// Match a network target against an allowlist rule.
///
/// Rules:
/// - Exact match: `"localhost:8080"` matches `"localhost:8080"`
/// - Wildcard port: `"localhost:*"` matches `"localhost:8080"`, `"localhost:443"`
/// - Unix sockets: `"unix:/tmp/foo.sock"` matches exactly
fn network_matches(rule: &str, target: &str) -> bool {
    if rule == target {
        return true;
    }
    let Some(rule) = parse_network_rule(rule) else {
        return false;
    };
    let Some(target) = parse_network_target(target) else {
        return false;
    };
    match (rule, target) {
        (
            NetworkRule::Socket {
                host: rule_host,
                port: rule_port,
            },
            NetworkTarget::Socket {
                host: target_host,
                port: target_port,
            },
        ) => rule_host == target_host && rule_port.matches(target_port),
        (NetworkRule::Unix(rule_path), NetworkTarget::Unix(target_path)) => {
            rule_path == target_path
        }
        _ => false,
    }
}

fn process_allowed_by_config(cfg: &AllowlistConfig, command: &str) -> bool {
    let Some(command_path) = normalize_process_command(command) else {
        return false;
    };
    cfg.processes
        .iter()
        .filter_map(|rule| normalize_process_rule(rule))
        .any(|rule| rule == command_path)
}

fn normalize_process_rule(rule: &str) -> Option<PathBuf> {
    let path = Path::new(rule);
    path.is_absolute().then_some(())?;
    path.canonicalize().ok()
}

fn normalize_process_command(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.canonicalize().ok();
    }
    if !is_bare_command(path) {
        return None;
    }
    resolve_command_from_path(path)
}

fn is_bare_command(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn resolve_command_from_path(command: &Path) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for candidate in candidate_paths(&dir.join(command)) {
            if is_executable_file(&candidate) {
                return candidate.canonicalize().ok();
            }
        }
    }
    None
}

#[cfg(windows)]
fn candidate_paths(base: &Path) -> Vec<PathBuf> {
    let pathext = std::env::var_os("PATHEXT")
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into())
        .to_string_lossy()
        .into_owned();
    candidate_paths_with_pathext(base, &pathext)
}

#[cfg(windows)]
fn candidate_paths_with_pathext(base: &Path, pathext: &str) -> Vec<PathBuf> {
    let exts: Vec<String> = pathext
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(|ext| format!(".{}", ext.trim_start_matches('.')))
        .collect();
    // A name only counts as "already has an executable extension" when it ends
    // in an actual PATHEXT extension; a mere dot in the stem (`python3.11`)
    // must still get PATHEXT candidates.
    let name = base
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if exts
        .iter()
        .any(|ext| name.ends_with(&ext.to_ascii_lowercase()))
    {
        return vec![base.to_path_buf()];
    }
    exts.iter()
        .map(|ext| {
            // `with_extension` would REPLACE a trailing dotted segment
            // (`python3.11` -> `python3.EXE`); append to the full name instead.
            let mut with_ext = base.as_os_str().to_owned();
            with_ext.push(ext);
            PathBuf::from(with_ext)
        })
        .collect()
}

#[cfg(not(windows))]
fn candidate_paths(base: &Path) -> Vec<PathBuf> {
    vec![base.to_path_buf()]
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NetworkRule {
    Socket { host: String, port: PortMatcher },
    Unix(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NetworkTarget {
    Socket { host: String, port: u16 },
    Unix(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortMatcher {
    Exact(u16),
    Any,
}

impl PortMatcher {
    fn matches(self, port: u16) -> bool {
        match self {
            Self::Exact(expected) => expected == port,
            Self::Any => true,
        }
    }
}

fn parse_network_rule(rule: &str) -> Option<NetworkRule> {
    let decoded = percent_decode(rule);
    let rule = decoded.as_str();
    if let Some(path) = rule.strip_prefix("unix:") {
        return Some(NetworkRule::Unix(normalize_unix_path(path)));
    }
    if let Some(host) = rule.strip_suffix(":*") {
        return Some(NetworkRule::Socket {
            host: normalize_host(host),
            port: PortMatcher::Any,
        });
    }
    let (host, port) = parse_host_port(rule)?;
    Some(NetworkRule::Socket {
        host,
        port: PortMatcher::Exact(port),
    })
}

fn parse_network_target(target: &str) -> Option<NetworkTarget> {
    let decoded = percent_decode(target);
    let target = decoded.as_str();
    if let Some(path) = target.strip_prefix("unix:") {
        return Some(NetworkTarget::Unix(normalize_unix_path(path)));
    }
    if target.contains("://") {
        return parse_url_target(target);
    }
    let (host, port) = parse_host_port(target)?;
    Some(NetworkTarget::Socket { host, port })
}

fn parse_url_target(target: &str) -> Option<NetworkTarget> {
    let (scheme, remainder) = target.split_once("://")?;
    let authority = remainder
        .split(['/', '?', '#'])
        .next()
        .filter(|authority| !authority.is_empty())?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, authority)| authority);

    let (host, port) = if authority.starts_with('[') {
        parse_bracketed_host_port(authority)
            .or_else(|| Some((normalize_host(authority), default_port_for_scheme(scheme)?)))?
    } else if let Some((host, port_str)) = authority.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            (normalize_host(host), port)
        } else {
            (normalize_host(authority), default_port_for_scheme(scheme)?)
        }
    } else {
        (normalize_host(authority), default_port_for_scheme(scheme)?)
    };

    Some(NetworkTarget::Socket { host, port })
}

fn default_port_for_scheme(scheme: &str) -> Option<u16> {
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

fn parse_host_port(value: &str) -> Option<(String, u16)> {
    if value.starts_with('[') {
        return parse_bracketed_host_port(value);
    }
    let (host, port) = value.rsplit_once(':')?;
    Some((normalize_host(host), port.parse().ok()?))
}

fn parse_bracketed_host_port(value: &str) -> Option<(String, u16)> {
    let (host, rest) = value.strip_prefix('[')?.split_once(']')?;
    let port = rest.strip_prefix(':')?.parse().ok()?;
    Some((normalize_host(host), port))
}

fn normalize_host(host: &str) -> String {
    let stripped = host.trim_matches(['[', ']']);
    let decoded = percent_decode(stripped);
    let lowered = decoded.to_ascii_lowercase();
    normalize_ip(&lowered)
}

/// Decode percent-encoded (`%XX`) sequences in a string.
///
/// Invalid sequences (non-hex digits, truncated `%` at end) are passed through
/// verbatim so that malformed input does not silently disappear.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    // Capacity hint clamped to 4 KiB: an input-length-derived capacity is
    // unbounded under the verifier's open model and refutes the bulk-allocation
    // obligation, so each `with_capacity` call site is directly dominated by a
    // bound check (a hoisted `min` loses the bound across the join). Allowlist
    // entries are far below 4 KiB, so the pre-size is unchanged in practice;
    // longer inputs merely reallocate on growth. Capacity is not observable
    // behavior.
    let mut out = if bytes.len() < 4096 {
        Vec::with_capacity(bytes.len())
    } else {
        Vec::with_capacity(4096)
    };
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(decoded) = decode_hex_pair(bytes[i + 1], bytes[i + 2])
        {
            out.push(decoded);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Decode a pair of ASCII hex digits into a byte. Returns `None` if either
/// character is not a valid hex digit.
fn decode_hex_pair(hi: u8, lo: u8) -> Option<u8> {
    let h = hex_digit(hi)?;
    let l = hex_digit(lo)?;
    Some(h << 4 | l)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Canonicalize an IP address string so that equivalent representations compare
/// equal. Handles:
///
/// - Leading-zero stripping (e.g. `0177.0.0.01` -> `127.0.0.1`)
/// - Hex IPv4 (e.g. `0x7f000001` -> `127.0.0.1`)
/// - Per-octet hex (e.g. `0x7f.0.0.1` -> `127.0.0.1`)
/// - IPv4-mapped IPv6 (e.g. `::ffff:127.0.0.1` -> `127.0.0.1`)
/// - IPv6 canonicalization via `std::net::Ipv6Addr`
///
/// If the input is not a recognized IP format, it is returned unchanged.
fn normalize_ip(host: &str) -> String {
    // Try hex-encoded single-integer IPv4 (0x7f000001)
    if let Some(ip) = try_parse_hex_ipv4(host) {
        return ip;
    }

    // Try dotted IPv4 with possible octal/hex octets (0177.0.0.01, 0x7f.0.0.1)
    if let Some(ip) = try_parse_mixed_ipv4(host) {
        return ip;
    }

    // Try standard IPv6 parsing (handles ::ffff:x.x.x.x mapped addresses)
    if let Ok(v6) = host.parse::<std::net::Ipv6Addr>() {
        // Convert IPv4-mapped IPv6 to plain IPv4
        if let Some(v4) = v6.to_ipv4_mapped() {
            return v4.to_string();
        }
        // Canonicalize IPv6 (collapses zeros, lowercase)
        return v6.to_string();
    }

    // Try standard IPv4 (std already strips leading zeros on output)
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        return v4.to_string();
    }

    host.to_owned()
}

/// Parse a single hex integer like `0x7f000001` as an IPv4 address.
fn try_parse_hex_ipv4(host: &str) -> Option<String> {
    let hex_str = host
        .strip_prefix("0x")
        .or_else(|| host.strip_prefix("0X"))?;
    if hex_str.is_empty() || hex_str.len() > 8 {
        return None;
    }
    // Must be all hex digits (no dots)
    if hex_str.contains('.') {
        return None;
    }
    let val = u32::from_str_radix(hex_str, 16).ok()?;
    let ip = std::net::Ipv4Addr::from(val);
    Some(ip.to_string())
}

/// Parse dotted-quad IPv4 where each octet may be decimal, octal (0-prefixed),
/// or hex (0x-prefixed). E.g. `0177.0.0.01` or `0x7f.0.0.0x01`.
fn try_parse_mixed_ipv4(host: &str) -> Option<String> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    // Only try mixed parsing if at least one octet looks non-decimal
    // (starts with 0 and has more digits, or starts with 0x)
    let needs_mixed = parts.iter().any(|p| {
        (p.len() > 1 && p.starts_with('0') && !p.starts_with("0x") && !p.starts_with("0X"))
            || p.starts_with("0x")
            || p.starts_with("0X")
    });
    if !needs_mixed {
        return None;
    }
    let mut octets = [0u8; 4];
    // Iterate the destination array zipped with the parts instead of
    // integer-indexing `octets[i]`: `parts.len() == 4` is checked above, but
    // the modular verifier does not carry that length relation into the loop
    // and refutes the index bound. `zip` visits exactly the 4 slots, so with
    // exactly 4 parts every part is parsed — identical behavior.
    for (slot, part) in octets.iter_mut().zip(parts.iter()) {
        *slot = parse_octet(part)?;
    }
    Some(std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]).to_string())
}

/// Parse a single IPv4 octet that may be decimal, octal (0-prefixed), or hex
/// (0x-prefixed). Returns `None` if the value exceeds 255 or the format is
/// invalid.
fn parse_octet(s: &str) -> Option<u8> {
    if s.is_empty() {
        return None;
    }
    let val = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()?
    } else if s.len() > 1 && s.starts_with('0') {
        // Octal
        u16::from_str_radix(s, 8).ok()?
    } else {
        s.parse::<u16>().ok()?
    };
    u8::try_from(val).ok()
}

/// Normalize a Unix socket path by collapsing redundant separators, `.`
/// components, and resolving `..` components without filesystem access.
fn normalize_unix_path(path: &str) -> String {
    let p = Path::new(path);
    let mut normalized = PathBuf::new();
    for component in p.components() {
        match component {
            Component::CurDir => {} // skip `.`
            Component::ParentDir => {
                normalized.pop(); // apply `..` by removing last component
            }
            other => normalized.push(other),
        }
    }
    let normalized = normalized.to_string_lossy().into_owned();
    // Windows twin: `PathBuf` re-renders separators as `\`, but a `unix:` rule
    // is written POSIX-style, so fold to `/` for a platform-stable canonical
    // form (rule and target both pass through here, so matching stays exact).
    // Unix is untouched — `\` is a legal filename byte there, never a separator.
    #[cfg(windows)]
    let normalized = normalized.replace('\\', "/");
    normalized
}

#[cfg(feature = "allowlist-toml")]
impl AllowlistConfig {
    /// Parse an [`AllowlistConfig`] from a TOML string.
    ///
    /// Requires the `allowlist-toml` feature. Gated so `aterm-core`'s default
    /// build tree does not pull `toml` + `serde` through this crate (#7729).
    ///
    /// # Errors
    ///
    /// Returns [`AllowlistError::Parse`] if the TOML is malformed.
    pub(crate) fn from_toml_str(s: &str) -> Result<Self, AllowlistError> {
        let table: aterm_toml::Table = s.parse().map_err(AllowlistError::Parse)?;
        Ok(Self {
            mcp_tools: extract_string_array(&table, "mcp", "allowed"),
            plugins: extract_string_array(&table, "plugins", "allowed"),
            network: extract_string_array(&table, "network", "allowed"),
            processes: extract_string_array(&table, "process", "allowed"),
        })
    }

    /// Parse an [`AllowlistConfig`] from a TOML file.
    ///
    /// Requires the `allowlist-toml` feature. See [`Self::from_toml_str`].
    ///
    /// # Errors
    ///
    /// Returns [`AllowlistError::Io`] if the file cannot be read, or
    /// [`AllowlistError::Parse`] if the TOML is malformed.
    pub fn from_toml_file(path: impl AsRef<std::path::Path>) -> Result<Self, AllowlistError> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .map_err(|e| AllowlistError::Io(path.display().to_string(), e))?;
        Self::from_toml_str(&content)
    }
}

/// Extract a string array from a TOML table at `[section].key`.
/// Returns an empty vec if the section or key is missing.
#[cfg(feature = "allowlist-toml")]
fn extract_string_array(table: &aterm_toml::Table, section: &str, key: &str) -> Vec<String> {
    table
        .get(section)
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Errors from allowlist operations.
#[derive(Debug)]
#[non_exhaustive]
pub enum AllowlistError {
    /// Failed to read allowlist file.
    Io(String, std::io::Error),
    /// Failed to parse TOML.
    ///
    /// Only produced when the `allowlist-toml` feature is enabled (#7729).
    #[cfg(feature = "allowlist-toml")]
    Parse(aterm_toml::de::Error),
    /// Allowlist was already initialized.
    AlreadyInitialized,
}

// Hand-written `Display`/`Error` (was `#[derive(aterm_error::Error)]` with
// `#[error("failed to read allowlist from {0}: {1}")]` /
// `#[error("failed to parse allowlist TOML: {0}")]` /
// `#[error("allowlist already initialized")]` and `#[source]` on the inner
// errors): the derive's generated `fmt` expands a runtime-argument
// `format_args!`, whose unsafe `fmt::Arguments::new` constructor the Trust
// strict gate's native lowering fails closed on. Byte-identical rendering:
// a `{0}` on a `String` with default options is a verbatim write
// (`write_str`), and `err.to_string()` is exactly the placeholder's
// default-options `Display` rendering of the inner error. `source()` mirrors
// the derive's `#[source]` arms.
impl std::fmt::Display for AllowlistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, err) => {
                f.write_str("failed to read allowlist from ")?;
                f.write_str(path)?;
                f.write_str(": ")?;
                f.write_str(&err.to_string())
            }
            #[cfg(feature = "allowlist-toml")]
            Self::Parse(err) => {
                f.write_str("failed to parse allowlist TOML: ")?;
                f.write_str(&err.to_string())
            }
            Self::AlreadyInitialized => f.write_str("allowlist already initialized"),
        }
    }
}

impl std::error::Error for AllowlistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(_, err) => Some(err),
            #[cfg(feature = "allowlist-toml")]
            Self::Parse(err) => Some(err),
            Self::AlreadyInitialized => None,
        }
    }
}

#[cfg(test)]
mod tests {
    // Sole consumers are the platform-gated TOCTOU tests below.
    #[cfg(any(unix, windows))]
    use std::fs;

    use super::*;

    #[cfg(feature = "allowlist-toml")]
    #[test]
    fn parse_valid_toml() {
        let toml = r#"
[mcp]
allowed = ["read_file", "write_file"]

[plugins]
allowed = ["spell-check"]

[network]
allowed = ["localhost:*", "unix:/tmp/aterm.sock"]

[process]
allowed = ["/bin/bash", "/bin/zsh"]
"#;
        let config = AllowlistConfig::from_toml_str(toml).unwrap();
        assert_eq!(config.mcp_tools, vec!["read_file", "write_file"]);
        assert_eq!(config.plugins, vec!["spell-check"]);
        assert_eq!(config.network, vec!["localhost:*", "unix:/tmp/aterm.sock"]);
        assert_eq!(config.processes, vec!["/bin/bash", "/bin/zsh"]);
    }

    #[cfg(feature = "allowlist-toml")]
    #[test]
    fn parse_empty_toml() {
        let config = AllowlistConfig::from_toml_str("").unwrap();
        assert!(config.mcp_tools.is_empty());
        assert!(config.plugins.is_empty());
        assert!(config.network.is_empty());
        assert!(config.processes.is_empty());
    }

    #[cfg(feature = "allowlist-toml")]
    #[test]
    fn parse_partial_toml() {
        let toml = r#"
[mcp]
allowed = ["read_file"]
"#;
        let config = AllowlistConfig::from_toml_str(toml).unwrap();
        assert_eq!(config.mcp_tools, vec!["read_file"]);
        assert!(config.plugins.is_empty());
        assert!(config.network.is_empty());
        assert!(config.processes.is_empty());
    }

    #[cfg(feature = "allowlist-toml")]
    #[test]
    fn parse_invalid_toml() {
        let result = AllowlistConfig::from_toml_str("not valid [[[toml");
        assert!(result.is_err());
    }

    #[test]
    fn network_exact_match() {
        assert!(network_matches("localhost:8080", "localhost:8080"));
        assert!(!network_matches("localhost:8080", "localhost:9090"));
    }

    #[test]
    fn network_wildcard_port() {
        assert!(network_matches("localhost:*", "localhost:8080"));
        assert!(network_matches("localhost:*", "localhost:443"));
        assert!(!network_matches("localhost:*", "example.com:80"));
    }

    #[test]
    fn network_wildcard_matches_bracketed_ipv6() {
        assert!(network_matches("::1:*", "[::1]:8080"));
        assert!(network_matches("[::1]:*", "[::1]:443"));
        assert!(!network_matches("[::1]:*", "[::2]:443"));
    }

    #[test]
    fn network_matches_https_url_with_default_port() {
        assert!(network_matches(
            "example.com:443",
            "https://example.com/login"
        ));
        assert!(network_matches(
            "example.com:*",
            "https://example.com/login"
        ));
        assert!(!network_matches(
            "example.com:80",
            "https://example.com/login"
        ));
    }

    #[test]
    fn network_matches_bracketed_ipv6_url() {
        assert!(network_matches("::1:443", "https://[::1]/oauth"));
        assert!(network_matches("::1:*", "https://[::1]:8443/oauth"));
        assert!(!network_matches("::2:*", "https://[::1]:8443/oauth"));
    }

    #[test]
    fn network_unix_socket_exact() {
        assert!(network_matches(
            "unix:/tmp/aterm.sock",
            "unix:/tmp/aterm.sock"
        ));
        assert!(!network_matches(
            "unix:/tmp/aterm.sock",
            "unix:/tmp/other.sock"
        ));
    }

    #[test]
    fn default_config_denies_all() {
        let config = AllowlistConfig::default();
        assert!(config.mcp_tools.is_empty());
        assert!(config.plugins.is_empty());
        assert!(config.network.is_empty());
        assert!(config.processes.is_empty());
    }

    #[test]
    fn process_relative_path_is_rejected() {
        assert!(normalize_process_command("./bash").is_none());
        assert!(normalize_process_command("../bin/bash").is_none());
    }

    #[test]
    fn process_relative_rule_is_rejected() {
        assert!(normalize_process_rule("bash").is_none());
        assert!(normalize_process_rule("./bash").is_none());
    }

    #[test]
    fn process_canonicalizes_parent_segments_before_match() {
        let command = std::env::current_exe().unwrap();
        let canonical = command.canonicalize().unwrap();
        let variant = canonical
            .parent()
            .unwrap()
            .join("..")
            .join(canonical.parent().unwrap().file_name().unwrap())
            .join(canonical.file_name().unwrap());
        let config = AllowlistConfig {
            processes: vec![canonical.display().to_string()],
            ..AllowlistConfig::default()
        };
        assert!(process_allowed_by_config(
            &config,
            variant.to_str().unwrap()
        ));
    }

    #[test]
    fn process_canonicalizes_absolute_command_before_match() {
        let command = std::env::current_exe().unwrap();
        let canonical = command.canonicalize().unwrap();
        let variant = canonical
            .parent()
            .unwrap()
            .join(".")
            .join(canonical.file_name().unwrap());
        let config = AllowlistConfig {
            processes: vec![canonical.display().to_string()],
            ..AllowlistConfig::default()
        };
        assert!(process_allowed_by_config(
            &config,
            variant.to_str().unwrap()
        ));
    }

    // --- Percent-decode tests ---

    #[test]
    fn percent_decode_basic() {
        assert_eq!(percent_decode("localhost"), "localhost");
        assert_eq!(percent_decode("%6c%6f%63%61%6c%68%6f%73%74"), "localhost");
        assert_eq!(percent_decode("exam%70le.com"), "example.com");
    }

    #[test]
    fn percent_decode_uppercase_hex() {
        assert_eq!(percent_decode("%4A%4B"), "JK");
        assert_eq!(percent_decode("%4a%4b"), "JK");
    }

    #[test]
    fn percent_decode_passthrough_invalid() {
        assert_eq!(percent_decode("foo%2"), "foo%2");
        assert_eq!(percent_decode("foo%zz"), "foo%zz");
        assert_eq!(percent_decode("foo%"), "foo%");
    }

    // --- IP normalization tests ---

    #[test]
    fn normalize_ip_hex_ipv4() {
        assert_eq!(normalize_ip("0x7f000001"), "127.0.0.1");
        assert_eq!(normalize_ip("0X7F000001"), "127.0.0.1");
        assert_eq!(normalize_ip("0x00000000"), "0.0.0.0");
        assert_eq!(normalize_ip("0xffffffff"), "255.255.255.255");
    }

    #[test]
    fn normalize_ip_octal_dotted() {
        assert_eq!(normalize_ip("0177.0.0.01"), "127.0.0.1");
        assert_eq!(normalize_ip("0300.0250.0.01"), "192.168.0.1");
    }

    #[test]
    fn normalize_ip_hex_dotted() {
        assert_eq!(normalize_ip("0x7f.0.0.0x01"), "127.0.0.1");
    }

    #[test]
    fn normalize_ip_standard_ipv4_passthrough() {
        assert_eq!(normalize_ip("127.0.0.1"), "127.0.0.1");
        assert_eq!(normalize_ip("192.168.1.1"), "192.168.1.1");
    }

    #[test]
    fn normalize_ip_ipv4_mapped_ipv6() {
        assert_eq!(normalize_ip("::ffff:127.0.0.1"), "127.0.0.1");
        assert_eq!(normalize_ip("::ffff:192.168.1.1"), "192.168.1.1");
        assert_eq!(normalize_ip("::ffff:7f00:1"), "127.0.0.1");
    }

    #[test]
    fn normalize_ip_standard_ipv6() {
        assert_eq!(normalize_ip("::1"), "::1");
        assert_eq!(
            normalize_ip("0000:0000:0000:0000:0000:0000:0000:0001"),
            "::1"
        );
    }

    #[test]
    fn normalize_ip_non_ip_passthrough() {
        assert_eq!(normalize_ip("localhost"), "localhost");
        assert_eq!(normalize_ip("example.com"), "example.com");
    }

    // --- Unix path normalization tests ---

    #[test]
    fn normalize_unix_path_double_slash() {
        assert_eq!(normalize_unix_path("/tmp//aterm.sock"), "/tmp/aterm.sock");
        assert_eq!(normalize_unix_path("//tmp///aterm.sock"), "/tmp/aterm.sock");
    }

    #[test]
    fn normalize_unix_path_dot_segments() {
        assert_eq!(normalize_unix_path("/tmp/./aterm.sock"), "/tmp/aterm.sock");
        assert_eq!(
            normalize_unix_path("/tmp/./././aterm.sock"),
            "/tmp/aterm.sock"
        );
    }

    #[test]
    fn normalize_unix_path_combined() {
        assert_eq!(
            normalize_unix_path("/tmp/./foo//aterm.sock"),
            "/tmp/foo/aterm.sock"
        );
    }

    #[test]
    fn normalize_unix_path_clean() {
        assert_eq!(normalize_unix_path("/tmp/aterm.sock"), "/tmp/aterm.sock");
    }

    // --- Network matching bypass vector tests ---

    #[test]
    fn network_bypass_octal_ip() {
        assert!(network_matches("127.0.0.1:8080", "0177.0.0.01:8080"));
        assert!(network_matches("127.0.0.1:*", "0177.0.0.01:8080"));
        assert!(network_matches("0177.0.0.01:8080", "127.0.0.1:8080"));
    }

    #[test]
    fn network_bypass_hex_ip() {
        assert!(network_matches("127.0.0.1:8080", "0x7f000001:8080"));
        assert!(network_matches("127.0.0.1:*", "0x7f000001:443"));
        assert!(network_matches("0x7f000001:8080", "127.0.0.1:8080"));
    }

    #[test]
    fn network_bypass_hex_dotted_ip() {
        assert!(network_matches("127.0.0.1:8080", "0x7f.0.0.0x01:8080"));
    }

    #[test]
    fn network_bypass_percent_encoded_host() {
        assert!(network_matches(
            "localhost:8080",
            "%6c%6f%63%61%6c%68%6f%73%74:8080"
        ));
        assert!(network_matches(
            "localhost:*",
            "%6c%6f%63%61%6c%68%6f%73%74:9090"
        ));
    }

    #[test]
    fn network_bypass_percent_encoded_in_url() {
        assert!(network_matches(
            "example.com:443",
            "https://exam%70le.com/path"
        ));
    }

    #[test]
    fn network_bypass_ipv4_mapped_ipv6() {
        assert!(network_matches("127.0.0.1:8080", "[::ffff:127.0.0.1]:8080"));
        assert!(network_matches("127.0.0.1:*", "[::ffff:127.0.0.1]:9090"));
        assert!(network_matches("[::ffff:127.0.0.1]:8080", "127.0.0.1:8080"));
    }

    #[test]
    fn network_bypass_ipv6_expanded_vs_compressed() {
        assert!(network_matches(
            "::1:8080",
            "[0000:0000:0000:0000:0000:0000:0000:0001]:8080"
        ));
    }

    #[test]
    fn network_bypass_unix_double_slash() {
        assert!(network_matches(
            "unix:/tmp/aterm.sock",
            "unix:/tmp//aterm.sock"
        ));
        assert!(network_matches(
            "unix:/tmp//aterm.sock",
            "unix:/tmp/aterm.sock"
        ));
    }

    #[test]
    fn network_bypass_unix_dot_segment() {
        assert!(network_matches(
            "unix:/tmp/aterm.sock",
            "unix:/tmp/./aterm.sock"
        ));
        assert!(network_matches(
            "unix:/tmp/./aterm.sock",
            "unix:/tmp/aterm.sock"
        ));
    }

    #[test]
    fn network_bypass_nonmatch_still_denied() {
        assert!(!network_matches("127.0.0.1:8080", "127.0.0.2:8080"));
        assert!(!network_matches("127.0.0.1:*", "0x7f000002:8080"));
        assert!(!network_matches("localhost:*", "evil.com:80"));
        assert!(!network_matches(
            "unix:/tmp/aterm.sock",
            "unix:/var/evil.sock"
        ));
    }

    // Note: is_*_allowed() functions depend on global OnceLock state
    // (MODE and ALLOWLIST), so full integration tests are in
    // tests/allowlist_integration.rs using separate test binaries.

    // --- TOCTOU documentation tests (#7591) ---

    /// Document that `canonicalize()` resolves symlinks at check time,
    /// creating a TOCTOU window before exec. The `verify_executable_fd`
    /// function closes this gap by resolving from an open fd.
    #[cfg(unix)]
    #[test]
    fn toctou_symlink_swap_between_canonicalize_and_exec() {
        use std::os::unix::fs::symlink;

        let dir = aterm_tempfile::tempdir().unwrap();
        let real_bin = dir.path().join("real_bin");
        let evil_bin = dir.path().join("evil_bin");
        let link = dir.path().join("link_bin");

        // Create two "executables"
        fs::write(&real_bin, "#!/bin/sh\necho safe").unwrap();
        fs::write(&evil_bin, "#!/bin/sh\necho pwned").unwrap();

        // Symlink initially points to the safe binary
        symlink(&real_bin, &link).unwrap();

        // canonicalize() at check time resolves to real_bin
        let check_time_path = link.canonicalize().unwrap();
        assert_eq!(check_time_path, real_bin.canonicalize().unwrap());

        // --- TOCTOU window: attacker swaps the symlink ---
        fs::remove_file(&link).unwrap();
        symlink(&evil_bin, &link).unwrap();

        // At exec time, the symlink now points to evil_bin
        let exec_time_path = link.canonicalize().unwrap();
        assert_eq!(exec_time_path, evil_bin.canonicalize().unwrap());

        // The two paths differ -- this IS the TOCTOU bug.
        assert_ne!(
            check_time_path, exec_time_path,
            "TOCTOU: path changed between check and exec"
        );

        // The fix: verify_executable_fd resolves from the fd, not the path.
        // (Full integration of fd-based verification requires the exec
        //  callsite to open + verify, which is tested in the PTY layer.)
    }

    /// Verify that `fd_to_path` correctly resolves an open fd back to
    /// its filesystem path, which is the foundation of the TOCTOU fix.
    #[cfg(unix)]
    #[test]
    fn fd_to_path_resolves_open_file() {
        use std::os::unix::io::AsRawFd;

        let dir = aterm_tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test_exec");
        fs::write(&file_path, "#!/bin/sh").unwrap();

        let file = fs::File::open(&file_path).unwrap();
        let fd = file.as_raw_fd();

        let resolved = super::fd_to_path(fd);
        assert!(
            resolved.is_some(),
            "fd_to_path should resolve an open file descriptor"
        );
        let resolved = resolved.unwrap();
        assert_eq!(
            resolved,
            file_path.canonicalize().unwrap(),
            "resolved path should match canonical path"
        );
    }

    /// Verify that `fd_to_path` detects symlink swap after open: if we
    /// open the real file and then swap the symlink, the fd still
    /// resolves to the original (real) file.
    #[cfg(unix)]
    #[test]
    fn fd_to_path_stable_after_symlink_swap() {
        use std::os::unix::fs::symlink;
        use std::os::unix::io::AsRawFd;

        let dir = aterm_tempfile::tempdir().unwrap();
        let real_bin = dir.path().join("real");
        let evil_bin = dir.path().join("evil");
        let link = dir.path().join("cmd");

        fs::write(&real_bin, "safe").unwrap();
        fs::write(&evil_bin, "evil").unwrap();
        symlink(&real_bin, &link).unwrap();

        // Open via the symlink -- fd points to real_bin
        let file = fs::File::open(&link).unwrap();
        let fd = file.as_raw_fd();

        // Swap the symlink to evil_bin
        fs::remove_file(&link).unwrap();
        symlink(&evil_bin, &link).unwrap();

        // fd_to_path still resolves to the original real_bin
        let resolved = super::fd_to_path(fd).unwrap();
        assert_eq!(
            resolved,
            real_bin.canonicalize().unwrap(),
            "fd should still point to original file after symlink swap"
        );
    }

    // --- Windows PATH candidate tests (dotted bare names) ---

    #[cfg(windows)]
    #[test]
    fn candidate_paths_appends_pathext_to_dotted_stem() {
        let candidates =
            candidate_paths_with_pathext(Path::new(r"C:\tools\python3.11"), ".COM;.EXE;.BAT");
        assert_eq!(
            candidates,
            vec![
                PathBuf::from(r"C:\tools\python3.11.COM"),
                PathBuf::from(r"C:\tools\python3.11.EXE"),
                PathBuf::from(r"C:\tools\python3.11.BAT"),
            ],
            "extension must be appended, never swapped for the dotted stem"
        );
    }

    #[cfg(windows)]
    #[test]
    fn candidate_paths_keeps_existing_executable_extension() {
        let candidates = candidate_paths_with_pathext(Path::new(r"C:\tools\foo.exe"), ".COM;.EXE");
        assert_eq!(candidates, vec![PathBuf::from(r"C:\tools\foo.exe")]);
    }

    #[cfg(windows)]
    #[test]
    fn candidate_paths_expands_bare_name() {
        let candidates = candidate_paths_with_pathext(Path::new(r"C:\tools\cmd"), ".COM;.EXE");
        assert_eq!(
            candidates,
            vec![
                PathBuf::from(r"C:\tools\cmd.COM"),
                PathBuf::from(r"C:\tools\cmd.EXE"),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn candidate_paths_expands_dotted_name_without_pathext_suffix() {
        // `node.js` has an extension, but not an executable one — it must
        // still get PATHEXT candidates (and keep the full `node.js` stem).
        let candidates = candidate_paths_with_pathext(Path::new(r"C:\tools\node.js"), ".EXE;.CMD");
        assert_eq!(
            candidates,
            vec![
                PathBuf::from(r"C:\tools\node.js.EXE"),
                PathBuf::from(r"C:\tools\node.js.CMD"),
            ]
        );
    }

    // --- Windows handle-based TOCTOU verification tests ---

    /// Windows twin of `fd_to_path_resolves_open_file`: an open handle
    /// resolves back to its canonical filesystem path.
    #[cfg(windows)]
    #[test]
    fn handle_to_path_resolves_open_file() {
        use std::os::windows::io::AsRawHandle;

        let dir = aterm_tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test_exec.exe");
        fs::write(&file_path, "MZ").unwrap();

        let file = fs::File::open(&file_path).unwrap();
        let resolved = handle_to_path(file.as_raw_handle())
            .expect("handle_to_path should resolve an open handle");
        assert!(
            windows_paths_equal(&resolved, &file_path.canonicalize().unwrap()),
            "resolved {} should match canonical path",
            resolved.display()
        );
    }

    /// The handle-based allowlist check matches the file the handle refers
    /// to, and denies handles to unlisted files.
    #[cfg(windows)]
    #[test]
    fn handle_allowed_by_config_matches_open_handle() {
        use std::os::windows::io::AsRawHandle;

        let dir = aterm_tempfile::tempdir().unwrap();
        let listed = dir.path().join("listed.exe");
        let unlisted = dir.path().join("unlisted.exe");
        fs::write(&listed, "MZ").unwrap();
        fs::write(&unlisted, "MZ").unwrap();

        let config = AllowlistConfig {
            processes: vec![listed.canonicalize().unwrap().display().to_string()],
            ..AllowlistConfig::default()
        };

        let listed_file = fs::File::open(&listed).unwrap();
        assert!(handle_allowed_by_config(
            &config,
            listed_file.as_raw_handle()
        ));

        let unlisted_file = fs::File::open(&unlisted).unwrap();
        assert!(!handle_allowed_by_config(
            &config,
            unlisted_file.as_raw_handle()
        ));
    }

    /// The share mode used by `open_verified_executable` denies write,
    /// rename, and delete of the verified file while the handle is held —
    /// the lock that narrows the check-to-spawn TOCTOU window.
    #[cfg(windows)]
    #[test]
    fn open_no_write_share_locks_file_against_swap() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool.exe");
        let swapped = dir.path().join("tool.exe.swap");
        fs::write(&exe, "safe").unwrap();

        let held = open_no_write_share(&exe).expect("open should succeed");
        assert!(
            fs::write(&exe, "evil").is_err(),
            "write must be denied while the verified handle is held"
        );
        assert!(
            fs::rename(&exe, &swapped).is_err(),
            "rename must be denied while the verified handle is held"
        );
        assert!(
            fs::remove_file(&exe).is_err(),
            "delete must be denied while the verified handle is held"
        );

        drop(held);
        assert!(fs::write(&exe, "fine").is_ok(), "lock released on drop");
    }
}
