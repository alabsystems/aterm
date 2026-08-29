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
//!   [`KEYCHAIN_SERVICE`], account = the connection name). The best-experience
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
/// [`resolve_token`]; kept on every target so the module docs' intra-link resolves.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const KEYCHAIN_SERVICE: &str = "aterm-net-drive";

/// Look up a saved connection by name (re-reads `aterm.toml`, so edits need no
/// restart). `None` if there is no `[[net.connections]]` entry with that `name`,
/// or the name is not a valid connection name (so it can never be dialed/stored).
pub(crate) fn resolve(name: &str) -> Option<Connection> {
    if !valid_connection_name(name) {
        return None;
    }
    let config = crate::app_config::load_config();
    config.net?.connections.into_iter().find(|c| c.name == name)
}

/// All DIALABLE saved connection names — only valid names, so `dial-list` never
/// advertises a name `dial` would then reject.
pub(crate) fn names() -> Vec<String> {
    crate::app_config::load_config()
        .net
        .map(|net| dialable_names(net.connections))
        .unwrap_or_default()
}

fn dialable_names(connections: Vec<Connection>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    connections
        .into_iter()
        .map(|connection| connection.name)
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
                return Err(format!(
                    "Keychain error reading the token for '{}' ({e}); is the keychain locked \
                     or access denied?",
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

#[cfg(not(target_os = "macos"))]
fn resolve_conventional_file_token(
    conn: &Connection,
    directory: &std::path::Path,
) -> Result<EdgeToken, String> {
    token_from_path(
        &directory.join(format!("{}.token", conn.name)),
        &format!("stored token for '{}'", conn.name),
    )
    .map_err(|error| format!("{error}; {}", provision_hint(&conn.name)))
}

fn token_from_file(path: &str) -> Result<EdgeToken, String> {
    token_from_path(&expand_tilde(path), path)
}

fn token_from_path(path: &std::path::Path, label: &str) -> Result<EdgeToken, String> {
    let hex = read_token_file(path, label)?;
    EdgeToken::from_hex(hex.trim())
        .ok_or_else(|| format!("token_file {label} does not contain 64-char hex"))
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
        .map_err(|e| format!("token_file {path}: {e}"))?;
    let meta = f
        .metadata()
        .map_err(|e| format!("token_file {path}: {e}"))?;
    if !meta.file_type().is_file() {
        return Err(format!(
            "token_file {path} is not a regular file (FIFO/device/directory refused)"
        ));
    }
    let mode = meta.mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "token_file {path} is group/world-accessible (mode {:o}); run `chmod 600 {path}`",
            mode & 0o777
        ));
    }
    let mut hex = String::new();
    f.take(TOKEN_FILE_MAX + 1)
        .read_to_string(&mut hex)
        .map_err(|e| format!("token_file {path}: {e}"))?;
    if hex.len() as u64 > TOKEN_FILE_MAX {
        return Err(format!(
            "token_file {path} exceeds {TOKEN_FILE_MAX} bytes; not a token file"
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
        .map_err(|e| format!("token_file {path}: {e}"))?;
    let meta = f
        .metadata()
        .map_err(|e| format!("token_file {path}: {e}"))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(format!(
            "token_file {path} is not a regular file (symlink/junction refused)"
        ));
    }
    let mut hex = String::new();
    f.take(TOKEN_FILE_MAX + 1)
        .read_to_string(&mut hex)
        .map_err(|e| format!("token_file {path}: {e}"))?;
    if hex.len() as u64 > TOKEN_FILE_MAX {
        return Err(format!(
            "token_file {path} exceeds {TOKEN_FILE_MAX} bytes; not a token file"
        ));
    }
    Ok(hex)
}

#[cfg(not(any(unix, windows)))]
fn read_token_file(p: &std::path::Path, path: &str) -> Result<String, String> {
    aterm_effects::file_feed::read_bounded_regular_utf8(p, TOKEN_FILE_MAX as usize)
        .map_err(|e| format!("token_file {path}: {e}"))
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
        return Err("token must be 64-char hex (the remote's control token)".to_owned());
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
    let conn = resolve(name).ok_or_else(|| {
        let avail = names();
        if avail.is_empty() {
            format!("unknown connection '{name}' (no [[net.connections]] in aterm.toml)")
        } else {
            format!(
                "unknown connection '{name}' (configured: {})",
                avail.join(", ")
            )
        }
    })?;
    let token = resolve_token(&conn)?;
    let pin = parse_fingerprint(&conn.fingerprint)
        .ok_or_else(|| format!("connection '{name}': fingerprint must be 64-char hex"))?;
    let ccfg = aterm_net::tls::client_config(pin);
    let local = client
        .try_clone()
        .map_err(|e| format!("clone client socket: {e}"))?;
    // Session rebind pin (OPTIONAL). Only `expect_nonce` arms the guard: the cert
    // fingerprint is already TLS-enforced above, and `sid` is a record field the
    // `matches` check does not consult. Absent ⇒ `None` ⇒ a byte-identical un-pinned
    // dial. Present ⇒ `dial_and_relay_pinned` enforces the launch-nonce rebind guard
    // before relaying (and, until the wire echoes the remote's launch identity, fails
    // closed rather than relay unverified).
    let endpoint = conn
        .expect_nonce
        .as_ref()
        .map(|nonce| aterm_net::RemoteEndpoint {
            host: conn.host.clone(),
            sid: conn.sid.clone().unwrap_or_default(),
            nonce: nonce.clone(),
            fingerprint: conn.fingerprint.clone(),
        });
    aterm_net::drive::dial_and_relay_pinned(
        &conn.host, ccfg, "dial", "drive", &token, prebuffer, local, endpoint,
    )
    .map_err(|e| format!("{}: {e}", conn.host))
}

fn provision_hint(name: &str) -> String {
    let dest = if cfg!(target_os = "macos") {
        "stored in the macOS Keychain"
    } else if cfg!(windows) {
        "writes a token file under %APPDATA%\\aterm\\net"
    } else {
        "writes a 0600 file"
    };
    format!(
        "no drive token for '{name}'. Provision it once with: \
         aterm-ctl dial-token {name} <remote-control-token>  \
         ({dest}), or set `token_file` on the connection."
    )
}

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
            dialable_names(vec![
                connection("work"),
                connection("bad name"),
                connection("work"),
                connection("home"),
            ]),
            ["work", "home"],
            "first configured name wins and output order remains stable"
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
