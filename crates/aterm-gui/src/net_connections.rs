// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Saved network-drive connections (the `dial <name>` side of the L3 network
//! drive) and their drive-token storage.
//!
//! A `[[net.connections]]` entry in `aterm.toml` ([`Connection`]) holds the
//! NON-secret coordinates of a remote aterm — `host`, cert `fingerprint`, optional
//! `sid`/`nonce` rebind pins. The drive TOKEN (a bearer secret that grants full
//! remote-drive authority) is kept OUT of the config file:
//!
//! * **macOS** — the system **Keychain** (generic password, service
//!   `aterm-net-drive`, account = the connection name). The best-experience
//!   default; provisioned with [`store_token`], read with [`resolve_token`].
//! * **Linux** — an explicit **0600 `token_file`**, or the conventional
//!   `~/.config/aterm/net/<name>.token` written by [`store_token`].
//!   [`resolve_token`] refuses a group/world-accessible file.
//! * **Windows** — the explicit/conventional token file too, but there are no
//!   POSIX mode bits: the file is exclusive-created and inherits its
//!   directory's ACL (0600 is NOT enforced), and reads refuse
//!   symlinks/junctions and non-regular files.
//!
//! Connections are re-read from disk on each [`resolve`], so edits take effect
//! without a restart.

use aterm_session::EdgeToken;

pub(crate) use crate::app_config::Connection;

/// The macOS Keychain generic-password calls, over Security.framework's
/// `SecItem*` API directly (this module retired the `security-framework`
/// crate — 10,503 lines for a two-function need).
#[cfg(target_os = "macos")]
mod keychain;

/// Keychain generic-password service under which drive tokens are stored on macOS
/// (account = the connection name). Referenced only by the macOS Keychain lookup in
/// [`resolve_token`].
#[cfg(target_os = "macos")]
pub(crate) const KEYCHAIN_SERVICE: &str = "aterm-net-drive";

/// Look up a saved connection by name (re-reads `aterm.toml`, so edits need no
/// restart). `Err` says why none matched: the name is not a usable connection
/// name (so it can never be dialed/stored), `aterm.toml` does not load, or no
/// `[[net.connections]]` record carries it.
pub(crate) fn resolve(name: &str) -> Result<Connection, String> {
    if !valid_connection_name(name) {
        return Err(format!(
            "'{}' is not a usable connection name ([A-Za-z0-9_-] only)",
            name.escape_debug()
        ));
    }
    pick_connection(name, saved_connections()?)
}

/// The `dial-list` answer's text: every DIALABLE saved connection name — only
/// valid names, so it never advertises a name `dial` would then reject — or
/// why there is none. `Err`: `aterm.toml` does not load.
pub(crate) fn dial_list() -> Result<String, String> {
    Ok(dial_list_text(&saved_connections()?))
}

fn dial_list_text(connections: &[Connection]) -> String {
    let names = dialable_names(connections);
    if connections.is_empty() {
        "(no connections — add [[net.connections]] to aterm.toml)".to_owned()
    } else if names.is_empty() {
        "(no usable connection name — [[net.connections]] names are [A-Za-z0-9_-] only)".to_owned()
    } else {
        names.join(" ")
    }
}

/// The `[[net.connections]]` records `aterm.toml` holds now.
fn saved_connections() -> Result<Vec<Connection>, String> {
    crate::app_config::net_config_now()
        .map(|net| net.map(|net| net.connections).unwrap_or_default())
        .map_err(|reason| {
            format!("aterm.toml does not load ({reason}), so no connection can be read")
        })
}

/// The record named `name` among `connections`, or why there is none.
fn pick_connection(name: &str, connections: Vec<Connection>) -> Result<Connection, String> {
    if connections.is_empty() {
        return Err(format!(
            "no connection '{name}': aterm.toml has no [[net.connections]]"
        ));
    }
    let names = dialable_names(&connections);
    connections
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| {
            if names.is_empty() {
                format!(
                    "no connection '{name}': no [[net.connections]] name in aterm.toml is \
                     usable ([A-Za-z0-9_-] only)"
                )
            } else {
                format!(
                    "unknown connection '{name}' (configured: {})",
                    names.join(", ")
                )
            }
        })
}

fn dialable_names(connections: &[Connection]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    connections
        .iter()
        .map(|connection| connection.name.clone())
        .filter(|name| valid_connection_name(name) && seen.insert(name.clone()))
        .collect()
}

/// A connection name indexes the macOS Keychain account AND (off-macOS) a
/// `<name>.token` file path, so it is restricted to a safe alphabet: a non-empty
/// run of `[A-Za-z0-9_-]`. This rejects whitespace (so `dial-list` and `dial`
/// agree) and any `.`/`..`/`/` that could traverse out of `~/.config/aterm/net`.
pub(crate) fn valid_connection_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Pure syntax check for the outbound endpoint accepted by the dial path. DNS
/// names remain valid without resolving them in Manual's background analysis;
/// malformed/missing ports, raw unbracketed IPv6, whitespace, and impossible
/// port zero are rejected before a user waits for a network attempt.
pub(crate) fn valid_dial_endpoint(endpoint: &str) -> bool {
    if endpoint.is_empty() || endpoint != endpoint.trim() {
        return false;
    }
    if let Ok(address) = endpoint.parse::<std::net::SocketAddr>() {
        return address.port() != 0;
    }
    let Some((host, port)) = endpoint.rsplit_once(':') else {
        return false;
    };
    if host.is_empty()
        || host.len() > 253
        || host.contains(':')
        || host.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '/' | '\\' | '[' | ']')
        })
    {
        return false;
    }
    let Ok(port) = port.parse::<u16>() else {
        return false;
    };
    port != 0
}

/// Parse the configured cert `fingerprint` (64-char SHA-256 hex, optionally
/// `sha256:`-prefixed) into the 32-byte pin [`aterm_net::tls::client_config`] wants.
pub(crate) fn parse_fingerprint(s: &str) -> Option<[u8; 32]> {
    let hex = s.trim();
    let hex = hex.strip_prefix("sha256:").unwrap_or(hex);
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let b = hex.as_bytes();
    let mut i = 0;
    while i < 32 {
        let hi = char::from(b[2 * i]).to_digit(16)?;
        let lo = char::from(b[2 * i + 1]).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
        i += 1;
    }
    Some(out)
}

/// The per-user home for `~` expansion: `HOME`, else (Windows sessions normally
/// have no `HOME`) `USERPROFILE`.
fn home_dir() -> Option<std::path::PathBuf> {
    if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Some(std::path::PathBuf::from(h));
    }
    #[cfg(windows)]
    if let Some(p) = std::env::var_os("USERPROFILE").filter(|p| !p.is_empty()) {
        return Some(std::path::PathBuf::from(p));
    }
    None
}

/// Expand a leading `~/` (and, on Windows, `~\`) to the user's home. Shared with
/// [`crate::net_listen`] for the TLS cert/key paths.
pub(crate) fn expand_tilde(path: &str) -> std::path::PathBuf {
    let rest = path.strip_prefix("~/");
    #[cfg(windows)]
    let rest = rest.or_else(|| path.strip_prefix("~\\"));
    if let Some(rest) = rest
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    std::path::PathBuf::from(path)
}

/// Resolve a connection's drive token: macOS Keychain first (falling back to a
/// `token_file` if set), a 0600 `token_file` elsewhere. The error string is a
/// friendly, actionable provisioning hint (it never contains the secret).
pub(crate) fn resolve_token(conn: &Connection) -> Result<EdgeToken, String> {
    #[cfg(target_os = "macos")]
    {
        // macOS errSecItemNotFound — the ONLY error that means "fall through to a
        // token_file"; any other Keychain error (locked, access denied, …) is
        // surfaced rather than silently downgraded to the file.
        use keychain::ERR_SEC_ITEM_NOT_FOUND;
        match keychain::get_generic_password(KEYCHAIN_SERVICE, &conn.name) {
            Ok(bytes) => {
                let hex = std::str::from_utf8(&bytes).map_err(|_| {
                    format!("Keychain token for '{}' is not valid UTF-8", conn.name)
                })?;
                return EdgeToken::from_hex(hex.trim()).ok_or_else(|| {
                    format!("Keychain token for '{}' is not 64-char hex", conn.name)
                });
            }
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => {} // not stored → try a file
            Err(e) => {
                // `e` is the Security framework's own sentence for the status
                // (`User interaction is not allowed.`): it names the cause.
                return Err(format!(
                    "Keychain could not read the drive token for '{}': {e}",
                    conn.name
                ));
            }
        }
    }
    if let Some(path) = &conn.token_file {
        return token_from_file(path);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(directory) = config_net_dir() {
        return resolve_conventional_file_token(conn, &directory);
    }
    Err(provision_hint(&conn.name))
}

/// The conventional `<directory>/<name>.token` that `dial-token` writes off
/// macOS. Every error names the real path; only a file that does not exist gets
/// the provisioning hint — one that exists and is refused (its mode, its
/// contents) says what to fix instead. Compiled on macOS for its tests.
#[cfg(any(test, not(target_os = "macos")))]
fn resolve_conventional_file_token(
    conn: &Connection,
    directory: &std::path::Path,
) -> Result<EdgeToken, String> {
    let path = directory.join(format!("{}.token", conn.name));
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "no drive token for '{}' ({} does not exist); run: {}",
            conn.name,
            path.display(),
            provision_command(&conn.name)
        )),
        _ => token_from_path(&path, &path.display().to_string()),
    }
}

/// The connection's explicit `token_file`: its errors name the key it set.
fn token_from_file(path: &str) -> Result<EdgeToken, String> {
    token_from_path(&expand_tilde(path), path).map_err(|e| format!("token_file {e}"))
}

/// Read the token at `path`; every error starts with `label` (the path as the
/// reader knows it) and says nothing about which setting named it.
fn token_from_path(path: &std::path::Path, label: &str) -> Result<EdgeToken, String> {
    let hex = read_token_file(path, label)?;
    EdgeToken::from_hex(hex.trim()).ok_or_else(|| format!("{label} does not contain 64-char hex"))
}

/// Upper bound on a `token_file` read. A legit token file is one 64-char hex line
/// (~65 bytes); 4096 is generous headroom while making a mispointed path — a fed
/// FIFO, a device, a runaway file — fail fast instead of growing memory without
/// bound. Read via `take(TOKEN_FILE_MAX + 1)` so hitting the cap is
/// distinguishable from a file of exactly the cap size.
const TOKEN_FILE_MAX: u64 = 4096;

/// Read a 0600 token file's contents. On Unix: open ONCE with `O_NOFOLLOW` (reject
/// a symlink at the final component) + `O_NONBLOCK` (a planted writerless FIFO
/// would otherwise park this open — and the control-handler thread with it —
/// forever; a regular file opened `O_NONBLOCK` reads normally, so the legit case
/// is unchanged), then fstat + read the SAME fd — no re-resolution, so no TOCTOU
/// and the file checked is the file actually read. The fstat must show a REGULAR
/// file: a 0600 FIFO/device passes the mode check below but is not a token file,
/// and reading one to EOF can block or grow unboundedly. The read itself is
/// capped at [`TOKEN_FILE_MAX`].
#[cfg(unix)]
fn read_token_file(p: &std::path::Path, path: &str) -> Result<String, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(p)
        .map_err(|e| format!("{path}: {e}"))?;
    let meta = f.metadata().map_err(|e| format!("{path}: {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "{path} is not a regular file (FIFO/device/directory refused)"
        ));
    }
    let mode = meta.mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "{path} is group/world-accessible (mode {:o}); run `chmod 600 {path}`",
            mode & 0o777
        ));
    }
    let mut hex = String::new();
    f.take(TOKEN_FILE_MAX + 1)
        .read_to_string(&mut hex)
        .map_err(|e| format!("{path}: {e}"))?;
    if hex.len() as u64 > TOKEN_FILE_MAX {
        return Err(format!(
            "{path} exceeds {TOKEN_FILE_MAX} bytes; not a token file"
        ));
    }
    Ok(hex)
}

/// Windows twin: no POSIX mode bits exist to check (the file's confidentiality
/// rests on the containing directory's ACL — disclosed in the module docs), so
/// the enforced posture is "a regular file we did not follow a link to reach":
/// `FILE_FLAG_OPEN_REPARSE_POINT` (the `O_NOFOLLOW` analog) opens a planted
/// symlink/junction ITSELF rather than its target, and the handle's own
/// metadata — same-fd, no re-resolution — is then required to be a plain file.
/// The read is capped at [`TOKEN_FILE_MAX`] like the Unix twin: even a regular
/// file can be a mispointed multi-gigabyte one.
#[cfg(windows)]
fn read_token_file(p: &std::path::Path, path: &str) -> Result<String, String> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(p)
        .map_err(|e| format!("{path}: {e}"))?;
    let meta = f.metadata().map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(format!(
            "{path} is not a regular file (symlink/junction refused)"
        ));
    }
    let mut hex = String::new();
    f.take(TOKEN_FILE_MAX + 1)
        .read_to_string(&mut hex)
        .map_err(|e| format!("{path}: {e}"))?;
    if hex.len() as u64 > TOKEN_FILE_MAX {
        return Err(format!(
            "{path} exceeds {TOKEN_FILE_MAX} bytes; not a token file"
        ));
    }
    Ok(hex)
}

#[cfg(not(any(unix, windows)))]
fn read_token_file(p: &std::path::Path, path: &str) -> Result<String, String> {
    aterm_effects::file_feed::read_bounded_regular_utf8(p, TOKEN_FILE_MAX as usize)
        .map_err(|e| format!("{path}: {e}"))
}

/// Provision a connection's drive token: into the macOS Keychain, else a 0600
/// `~/.config/aterm/net/<name>.token` file. Returns a human-readable summary of
/// where it landed (used by the `dial-token` control verb). Validates the token
/// is 64-char hex first.
pub(crate) fn store_token(name: &str, token_hex: &str) -> Result<String, String> {
    if !valid_connection_name(name) {
        // Guards the `<name>.token` join below against path traversal AND keeps the
        // Keychain account clean.
        return Err(
            "connection name must be a non-empty run of [A-Za-z0-9_-] (no spaces, dots, or slashes)"
                .to_owned(),
        );
    }
    let token_hex = token_hex.trim();
    if EdgeToken::from_hex(token_hex).is_none() {
        return Err(format!("token must be 64 hex digits {WHERE_THE_TOKEN_IS}"));
    }
    #[cfg(target_os = "macos")]
    {
        keychain::set_generic_password(KEYCHAIN_SERVICE, name, token_hex.as_bytes())
            .map_err(|e| format!("Keychain store failed: {e}"))?;
        Ok(format!(
            "stored the drive token for '{name}' in the macOS Keychain (service {KEYCHAIN_SERVICE})"
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = config_net_dir()
            .ok_or("cannot resolve the aterm config dir (XDG_CONFIG_HOME/HOME unset; on Windows, APPDATA/USERPROFILE unset)")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let path = dir.join(format!("{name}.token"));
        write_0600(&path, token_hex).map_err(|e| format!("write {}: {e}", path.display()))?;
        // Never overstate the Windows posture: there is no 0600 there, only the
        // exclusive-create + directory-ACL inheritance.
        let posture = if cfg!(unix) {
            "0600"
        } else {
            "exclusive-create; ACL-inherited, not 0600"
        };
        Ok(format!(
            "wrote the drive token for '{name}' to {} ({posture})",
            path.display()
        ))
    }
}

#[cfg(not(target_os = "macos"))]
fn config_net_dir() -> Option<std::path::PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        return Some(std::path::PathBuf::from(x).join("aterm").join("net"));
    }
    // Windows: mirror app_config::config_path — the config tree (and this net/
    // subdir with it) lives under %APPDATA%\aterm, not a HOME-based ~/.config.
    #[cfg(windows)]
    if let Some(a) = std::env::var_os("APPDATA").filter(|a| !a.is_empty()) {
        return Some(std::path::PathBuf::from(a).join("aterm").join("net"));
    }
    Some(home_dir()?.join(".config").join("aterm").join("net"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn write_0600(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.write_all(b"\n")
}

/// Windows twin of `write_0600` (mirrors `control_auth_win::provision_token`):
/// unlink-first + `create_new(true)` — CREATE_NEW atomically refuses ANY
/// pre-existing object at the path, including a planted symlink/junction, so
/// the token only ever lands in a file WE just created. No mode bits exist;
/// the file inherits its directory's ACL.
#[cfg(all(not(unix), not(target_os = "macos")))]
fn write_0600(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.write_all(b"\n")
}

/// Dial a saved connection and relay `client` (an already-authenticated local
/// control connection) to the remote aterm's control socket. `prebuffer` is any
/// bytes the serve loop already read past the `dial <name>` line. BLOCKS until the
/// relay ends; a PRE-relay failure returns an actionable error (the caller answers
/// `ERR dial <err>`). The raw token never crosses the wire — only the
/// channel-bound HMAC — because the REMOTE listener authenticates its own local
/// control socket.
pub(crate) fn dial_relay(
    name: &str,
    client: &aterm_uds::CtlStream,
    prebuffer: &[u8],
) -> Result<(), String> {
    let conn = resolve(name)?;
    let token = resolve_token(&conn)?;
    let pin = parse_fingerprint(&conn.fingerprint)
        .ok_or_else(|| format!("connection '{name}': fingerprint must be 64-char hex"))?;
    let ccfg = aterm_net::tls::client_config(pin);
    let local = client
        .try_clone()
        .map_err(|e| format!("clone client socket: {e}"))?;
    // Session rebind pin (OPTIONAL). Only `expect_nonce` arms the guard: the cert
    // fingerprint is already TLS-enforced above, and `sid` names which remote
    // session's live nonce is compared (absent: any session carrying the nonce).
    // Absent ⇒ `None` ⇒ a byte-identical un-pinned dial. Present ⇒
    // `dial_and_relay_pinned` reads the remote's `sessions bridge` roster and
    // enforces the launch-nonce rebind guard before relaying, failing closed when
    // the pinned session is not reported.
    let endpoint = conn
        .expect_nonce
        .as_ref()
        .map(|nonce| aterm_net::RemoteEndpoint {
            host: conn.host.clone(),
            sid: conn.sid.clone().unwrap_or_default(),
            nonce: nonce.clone(),
            fingerprint: conn.fingerprint.clone(),
        });
    // The connect probe: the remote's control server must answer `version`
    // before the caller's verb is sent, so a remote whose control socket
    // accepts nothing is reported in seconds rather than holding this lane
    // and the caller for the verb's whole deadline.
    let probe = aterm_net::drive::ConnectProbe {
        request: b"version\n",
        within: crate::proxy::FORWARD_ACCEPT_DEADLINE,
        is_refusal: crate::proxy::is_connection_refusal,
    };
    aterm_net::drive::dial_and_relay_probed(
        &conn.host, ccfg, "dial", "drive", &token, prebuffer, local, endpoint, probe,
    )
    .map_err(|e| dial_error(&conn, &e))
}

/// A failed dial's words: the host, then what happened. A refused drive token
/// carries its remedy, since the connection's name is known only here.
fn dial_error(conn: &Connection, e: &std::io::Error) -> String {
    if aterm_net::is_token_refused(e) {
        format!(
            "{}: the remote refused the drive token for '{}'; re-provision it: {}",
            conn.host,
            conn.name,
            provision_command(&conn.name)
        )
    } else {
        format!("{}: {e}", conn.host)
    }
}

/// `dial` with no drive token for `name`.
fn provision_hint(name: &str) -> String {
    format!(
        "no drive token for '{name}'; run: {}",
        provision_command(name)
    )
}

/// The one command that provisions `name`'s drive token, and where its value
/// is: the listener checks its persistent drive token (`net_listen`'s lookup) —
/// the per-launch control token only when that file is unusable. Its startup
/// line prints the token (minted this launch, or per-launch) or names the file
/// holding it, so that is where the person looks.
fn provision_command(name: &str) -> String {
    format!("aterm ctl dial-token {name} <token> {WHERE_THE_TOKEN_IS}")
}

/// Where a client finds the remote's drive token, true for every way the
/// listener can come by it.
const WHERE_THE_TOKEN_IS: &str =
    "(the remote's listener prints it, or the file holding it, at startup)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_names_are_validated_against_traversal_and_whitespace() {
        assert!(valid_connection_name("work-box"));
        assert!(valid_connection_name("work_box2"));
        for bad in [
            "", "work box", "..", ".", "../etc", "a/b", "a.token", ".hidden", "x\ny",
        ] {
            assert!(!valid_connection_name(bad), "{bad:?} must be rejected");
        }
        // store_token rejects a bad name BEFORE any Keychain/file write (no side effect).
        let tok = "ab".repeat(32);
        assert!(
            store_token("../../tmp/x", &tok).is_err(),
            "traversal name rejected"
        );
        assert!(store_token("", &tok).is_err(), "empty name rejected");
    }

    #[test]
    fn fingerprint_parses_hex_and_sha256_prefix_and_rejects_junk() {
        let hex = "ab".repeat(32); // 64 chars
        let bare = parse_fingerprint(&hex).unwrap();
        let pref = parse_fingerprint(&format!("sha256:{hex}")).unwrap();
        assert_eq!(bare, pref);
        assert_eq!(bare[0], 0xab);
        assert!(parse_fingerprint("tooshort").is_none());
        assert!(
            parse_fingerprint(&"zz".repeat(32)).is_none(),
            "non-hex rejected"
        );
    }

    #[test]
    fn dial_endpoint_syntax_and_name_catalog_are_bounded_and_unambiguous() {
        for valid in [
            "work.example:7100",
            "localhost:1",
            "127.0.0.1:7100",
            "[::1]:7100",
        ] {
            assert!(valid_dial_endpoint(valid), "{valid}");
        }
        for invalid in [
            "",
            "work.example",
            ":7100",
            "work.example:0",
            "work.example:not-a-port",
            "::1:7100",
            " work.example:7100",
            "work/example:7100",
        ] {
            assert!(!valid_dial_endpoint(invalid), "{invalid}");
        }

        let connection = |name: &str| Connection {
            name: name.to_string(),
            host: "127.0.0.1:7100".to_string(),
            fingerprint: "00".repeat(32),
            token_file: None,
            sid: None,
            expect_nonce: None,
        };
        assert_eq!(
            dialable_names(&[
                connection("work"),
                connection("bad name"),
                connection("work"),
                connection("home"),
            ]),
            ["work", "home"],
            "first configured name wins and output order remains stable"
        );
    }

    /// `dial` and `dial-list` say why no connection matched, and never claim
    /// "no [[net.connections]]" when the table is there: its only names are
    /// unusable, or the file does not parse at all.
    #[test]
    fn no_match_is_said_as_the_case_it_is() {
        let connection = |name: &str| Connection {
            name: name.to_string(),
            host: "127.0.0.1:7100".to_string(),
            fingerprint: "00".repeat(32),
            token_file: None,
            sid: None,
            expect_nonce: None,
        };
        assert_eq!(
            pick_connection("work", Vec::new()).err().as_deref(),
            Some("no connection 'work': aterm.toml has no [[net.connections]]")
        );
        assert_eq!(
            pick_connection("work", vec![connection("work.box")])
                .err()
                .as_deref(),
            Some(
                "no connection 'work': no [[net.connections]] name in aterm.toml is usable \
                 ([A-Za-z0-9_-] only)"
            )
        );
        assert_eq!(
            pick_connection("work", vec![connection("home"), connection("lab")])
                .err()
                .as_deref(),
            Some("unknown connection 'work' (configured: home, lab)")
        );
        assert_eq!(
            pick_connection("work", vec![connection("work")])
                .map(|c| c.name)
                .ok()
                .as_deref(),
            Some("work")
        );
        assert_eq!(
            resolve("work.box").err().as_deref(),
            Some("'work.box' is not a usable connection name ([A-Za-z0-9_-] only)")
        );

        assert_eq!(
            dial_list_text(&[]),
            "(no connections — add [[net.connections]] to aterm.toml)"
        );
        assert_eq!(
            dial_list_text(&[connection("work.box")]),
            "(no usable connection name — [[net.connections]] names are [A-Za-z0-9_-] only)"
        );
        assert_eq!(
            dial_list_text(&[connection("work"), connection("home")]),
            "work home"
        );

        // A file that does not parse says so, on one line, with the line it
        // broke on — the stand-in config it used to fall back to has no `[net]`.
        let broken = "[[net.connections]]\nname = \"work\"\nhost = \n";
        let reason = crate::app_config::net_config_from_text(broken)
            .err()
            .expect("a broken file does not parse");
        assert!(reason.starts_with("line 3: "), "{reason}");
        assert!(!reason.contains('\n'), "{reason}");
        let whole = "[[net.connections]]\nname = \"work\"\nhost = \"h:1\"\nfingerprint = \"x\"\n";
        let net = crate::app_config::net_config_from_text(whole)
            .unwrap()
            .expect("the table is read");
        assert_eq!(dialable_names(&net.connections), ["work"]);
    }

    /// The conventional `<name>.token` (what `dial-token` writes off macOS): each
    /// refusal names the real path — never a label like `stored token for 'work'`
    /// where a path belongs — and only a missing file is told to provision.
    #[test]
    #[cfg(unix)]
    fn a_conventional_token_file_refusal_names_its_real_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "aterm-conventional-token-words-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let connection = Connection {
            name: "work".to_string(),
            host: "127.0.0.1:7100".to_string(),
            fingerprint: "00".repeat(32),
            token_file: None,
            sid: None,
            expect_nonce: None,
        };
        let path = dir.join("work.token");
        let shown = path.display().to_string();

        let missing = resolve_conventional_file_token(&connection, &dir).unwrap_err();
        assert_eq!(
            missing,
            format!(
                "no drive token for 'work' ({shown} does not exist); run: aterm ctl dial-token \
                 work <token> (the remote's listener prints it, or the file holding it, at \
                 startup)"
            )
        );

        std::fs::write(&path, "cd".repeat(32)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let open = resolve_conventional_file_token(&connection, &dir).unwrap_err();
        assert_eq!(
            open,
            format!("{shown} is group/world-accessible (mode 644); run `chmod 600 {shown}`")
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            resolve_conventional_file_token(&connection, &dir)
                .unwrap()
                .to_hex(),
            "cd".repeat(32)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `dial-token` and the missing-token hint send the person to the remote
    /// listener's startup line — it prints the token it checks, or names the
    /// file holding it — never to the remote's control token, which a listener
    /// with a usable drive token file answers DENIED.
    #[test]
    fn the_provisioning_words_point_at_the_listeners_startup_line() {
        let bad = store_token("work", "not-hex").unwrap_err();
        assert_eq!(
            bad,
            "token must be 64 hex digits (the remote's listener prints it, or the file holding \
             it, at startup)"
        );
        let hint = provision_hint("work");
        assert!(
            hint.starts_with("no drive token for 'work'; run: aterm ctl dial-token work <token>"),
            "{hint}"
        );
        assert!(!hint.contains("control"), "{hint}");
        let conn = Connection {
            name: "work".to_string(),
            host: "work.example:7100".to_string(),
            fingerprint: "00".repeat(32),
            token_file: None,
            sid: None,
            expect_nonce: None,
        };
        let refused = std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            aterm_net::TokenRefused,
        );
        assert_eq!(
            dial_error(&conn, &refused),
            "work.example:7100: the remote refused the drive token for 'work'; re-provision it: \
             aterm ctl dial-token work <token> (the remote's listener prints it, or the file \
             holding it, at startup)"
        );
        let other = std::io::Error::other("connection refused");
        assert_eq!(
            dial_error(&conn, &other),
            "work.example:7100: connection refused"
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn conventional_dial_token_file_roundtrips_without_an_explicit_token_file() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-conventional-dial-token-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let token = "5a".repeat(32);
        write_0600(&dir.join("work-box.token"), &token).unwrap();
        let connection = Connection {
            name: "work-box".to_string(),
            host: "127.0.0.1:7100".to_string(),
            fingerprint: "00".repeat(32),
            token_file: None,
            sid: None,
            expect_nonce: None,
        };
        assert_eq!(
            resolve_conventional_file_token(&connection, &dir)
                .unwrap()
                .to_hex(),
            token
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expand_tilde_resolves_home_and_passes_plain_paths_through() {
        assert_eq!(
            expand_tilde("/abs/x.token"),
            std::path::PathBuf::from("/abs/x.token")
        );
        assert_eq!(
            expand_tilde("rel.token"),
            std::path::PathBuf::from("rel.token")
        );
        // "~x" (no separator) is a literal name, not a home reference.
        assert_eq!(expand_tilde("~x"), std::path::PathBuf::from("~x"));
        // Same home-resolution order as home_dir: HOME, then (Windows) USERPROFILE.
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
        #[cfg(windows)]
        let home = home.or_else(|| std::env::var_os("USERPROFILE").filter(|p| !p.is_empty()));
        if let Some(home) = home {
            let p = expand_tilde("~/net/x.token");
            assert!(p.starts_with(&home), "{p:?} must live under {home:?}");
            #[cfg(windows)]
            assert!(expand_tilde("~\\net\\x.token").starts_with(&home));
        }
    }

    /// Windows twin of the Unix token-file test: `write_0600` (exclusive-create,
    /// ACL-inherited) roundtrips through `token_from_file`, rotation succeeds over
    /// our own previous file (unlink-first), junk contents are rejected, and a
    /// non-regular file at the path is refused.
    #[test]
    #[cfg(windows)]
    fn windows_token_file_roundtrips_and_rejects_non_regular() {
        let dir = std::env::temp_dir().join(format!("aterm-tok-win-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tok = "cd".repeat(32);

        let good = dir.join("good.token");
        write_0600(&good, &tok).unwrap();
        assert_eq!(
            token_from_file(good.to_str().unwrap()).unwrap().to_hex(),
            tok
        );

        let tok2 = "ef".repeat(32);
        write_0600(&good, &tok2).unwrap();
        assert_eq!(
            token_from_file(good.to_str().unwrap()).unwrap().to_hex(),
            tok2
        );

        let junk = dir.join("junk.token");
        write_0600(&junk, "not-hex").unwrap();
        assert!(token_from_file(junk.to_str().unwrap()).is_err());

        let d = dir.join("dir.token");
        std::fs::create_dir_all(&d).unwrap();
        assert!(token_from_file(d.to_str().unwrap()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Unix reader must not hang or balloon on a mispointed `token_file`: a
    /// planted 0600 FIFO once parked the control-handler thread forever at
    /// `open()` (no `O_NONBLOCK`) and a fed FIFO/huge file grew memory without
    /// bound (uncapped `read_to_string`). Runs on macOS too — the Keychain is
    /// preferred there, but the file reader compiles (and is reachable via
    /// `token_file`) on every Unix.
    #[test]
    #[cfg(unix)]
    fn token_file_rejects_fifo_and_caps_oversized_reads() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-tok-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A writerless 0600 FIFO: O_NONBLOCK makes the open return immediately,
        // and the same-fd file-type check refuses it before any read.
        let fifo = dir.join("fifo.token");
        let cpath = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `cpath` is a valid NUL-terminated path; mkfifo returns 0 or -1,
        // which the assert checks.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let err = token_from_file(fifo.to_str().unwrap()).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");

        // A 0600 REGULAR file over the cap is refused, not slurped.
        let big = dir.join("big.token");
        std::fs::write(&big, "a".repeat(TOKEN_FILE_MAX as usize + 1)).unwrap();
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o600)).unwrap();
        let err = token_from_file(big.to_str().unwrap()).unwrap_err();
        assert!(err.contains("exceeds"), "{err}");

        // Exactly cap-sized is NOT mistaken for over-cap (it fails later, on hex
        // parse) — the take(cap + 1) sentinel only fires past the cap.
        let atcap = dir.join("atcap.token");
        std::fs::write(&atcap, "a".repeat(TOKEN_FILE_MAX as usize)).unwrap();
        std::fs::set_permissions(&atcap, std::fs::Permissions::from_mode(0o600)).unwrap();
        let err = token_from_file(atcap.to_str().unwrap()).unwrap_err();
        assert!(err.contains("64-char hex"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn token_file_must_be_owner_only_and_64_hex() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aterm-tok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tok = "cd".repeat(32);

        // 0644 -> refused for being group/world readable.
        let bad = dir.join("bad.token");
        std::fs::write(&bad, &tok).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(token_from_file(bad.to_str().unwrap()).is_err());

        // 0600 + valid hex -> accepted.
        let good = dir.join("good.token");
        std::fs::write(&good, &tok).unwrap();
        std::fs::set_permissions(&good, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            token_from_file(good.to_str().unwrap()).unwrap().to_hex(),
            tok
        );

        // 0600 but junk contents -> rejected.
        let junk = dir.join("junk.token");
        std::fs::write(&junk, "not-hex").unwrap();
        std::fs::set_permissions(&junk, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(token_from_file(junk.to_str().unwrap()).is_err());
    }
}
