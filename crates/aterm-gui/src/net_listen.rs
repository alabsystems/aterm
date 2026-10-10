// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secure-default-OFF network front-end for the control socket.
//!
//! aterm never opens a network port on its own. ONLY when an operator explicitly
//! sets all three keys of aterm.toml's `[net]` table:
//!
//! * `listen` — the bind address (e.g. `0.0.0.0:7100`),
//! * `cert`   — path to the server certificate (DER),
//! * `key`    — path to its PKCS#8 private key (DER),
//!
//! does [`maybe_spawn`] stand up a TLS listener. The table is the one spelling:
//! the `ATERM_NET_*` environment overrides are gone (2026-09-24). Each accepted connection must
//! present a capability **channel-bound** to the TLS session
//! ([`aterm_net::channel_bind`]) keyed by the persistent network-drive token
//! (`aterm-network-drive.token` beside the `key`); only then
//! is it relayed to the local control socket, where it authenticates again with
//! the ordinary `AUTH <token>` handshake. The TLS capability is the network-
//! specific gate that replaces the local same-uid `SO_PEERCRED` check — which has
//! no network analog — and, being bound to the TLS exporter, resists an active
//! MITM (a relay that terminates one TLS leg holds a different exporter, so a
//! captured tag never transfers).
//!
//! Missing any of the three keys ⇒ this is a no-op. A malformed cert/key or a
//! failed bind is logged and the listener simply does not start — never a panic,
//! never a fallback to an unauthenticated port.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use aterm_net::drive::NetEvent;
use aterm_session::EdgeToken;
use aterm_uds::CtlStream;

/// The single op the network capability authorizes: driving this instance's
/// control socket. A remote driver presents `channel_bind(token, exporter)` for
/// this op; `src` is not checked: authority comes from the token.
const NET_OP: &str = "drive";

/// Upper bound on a cert/key file read. A real DER cert or PKCS#8 key is a few
/// KiB; 1 MiB is generous headroom while making a mispointed path — a device, a
/// runaway file — fail fast instead of growing memory without bound before
/// `server_config` ever gets to reject the bytes. Read via `take(cap + 1)` so
/// hitting the cap is distinguishable from a file of exactly the cap size.
const CRED_FILE_MAX: u64 = 1024 * 1024;

/// The three listener selectors as `[net]` holds them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListenerInputs {
    listen: Option<String>,
    cert: Option<String>,
    key: Option<String>,
}

impl ListenerInputs {
    pub(crate) fn presence(&self) -> [bool; 3] {
        [
            self.listen.is_some(),
            self.cert.is_some(),
            self.key.is_some(),
        ]
    }

    fn is_complete(&self) -> bool {
        self.presence().into_iter().all(|present| present)
    }
}

/// The listener generation startup uses: the `[net]` table's three keys.
pub(crate) fn listener_inputs(config: &crate::app_config::Config) -> ListenerInputs {
    let net = config.net.as_ref();
    ListenerInputs {
        listen: net.and_then(|net| net.listen.clone()),
        cert: net.and_then(|net| net.cert.clone()),
        key: net.and_then(|net| net.key.clone()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenerFailureField {
    Listen,
    Cert,
    Key,
    CertAndKey,
}

/// One non-binding startup-preflight failure. Diagnostics uses the field set to
/// address the concrete TOML token(s); startup prints the same reason and exits
/// before `TcpListener::bind`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListenerPreflightError {
    field: ListenerFailureField,
    config_keys: Vec<&'static str>,
    message: String,
}

impl ListenerPreflightError {
    pub(crate) fn config_keys(&self) -> &[&'static str] {
        &self.config_keys
    }
}

impl std::fmt::Display for ListenerPreflightError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub(crate) struct PreparedListener {
    addr: String,
    bind_addresses: Vec<SocketAddr>,
    key_path: std::path::PathBuf,
    cert_bytes: Vec<u8>,
    key_bytes: Vec<u8>,
}

impl std::fmt::Debug for PreparedListener {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedListener")
            .field("addr", &self.addr)
            .field("bind_addresses", &self.bind_addresses)
            .field("key_path", &self.key_path)
            .field("cert_bytes_len", &self.cert_bytes.len())
            .field("key_bytes_len", &self.key_bytes.len())
            .finish()
    }
}

/// Validate every effective listener input without opening a network socket.
/// Missing fields retain the secure-default no-op. A complete set is bounded-read,
/// parsed as a numeric-IP bind address, and compiled through the exact rustls
/// server-config builder startup uses; only the returned [`PreparedListener`]
/// may proceed to bind. Requiring `IP:port` avoids putting an unbounded system
/// DNS lookup in either startup or Manual's language-service worker.
pub(crate) fn preflight_listener(
    inputs: &ListenerInputs,
) -> Result<Option<PreparedListener>, ListenerPreflightError> {
    let (Some(listen), Some(cert), Some(key)) = (&inputs.listen, &inputs.cert, &inputs.key) else {
        return Ok(None);
    };

    let bind_address = listen
        .parse::<SocketAddr>()
        .map_err(|error| ListenerPreflightError {
            field: ListenerFailureField::Listen,
            config_keys: vec!["net.listen"],
            message: format!(
                "network listener net.listen value {listen:?} is not a numeric IP:port bind address ({error}); no port is bound",
            ),
        })?;
    let bind_addresses = vec![bind_address];

    let cert_path = crate::net_connections::expand_tilde(cert);
    let cert_bytes = read_cred_file(&cert_path).map_err(|error| ListenerPreflightError {
        field: ListenerFailureField::Cert,
        config_keys: vec!["net.cert"],
        message: format!(
            "network listener certificate from net.cert ({cert:?}) is unreadable ({error}); no port is bound",
        ),
    })?;
    let key_path = crate::net_connections::expand_tilde(key);
    let key_bytes = read_cred_file(&key_path).map_err(|error| ListenerPreflightError {
        field: ListenerFailureField::Key,
        config_keys: vec!["net.key"],
        message: format!(
            "network listener private key from net.key ({key:?}) is unreadable ({error}); no port is bound",
        ),
    })?;
    aterm_net::tls::server_config(cert_bytes.clone(), key_bytes.clone()).map_err(|error| {
        ListenerPreflightError {
            field: ListenerFailureField::CertAndKey,
            config_keys: vec!["net.cert", "net.key"],
            message: format!(
                "network listener certificate/key pair from net.cert and net.key is rejected ({error}); no port is bound",
            ),
        }
    })?;

    Ok(Some(PreparedListener {
        addr: listen.clone(),
        bind_addresses,
        key_path,
        cert_bytes,
        key_bytes,
    }))
}

/// Bounded read of a config-supplied cert/key path. A bare `fs::read` trusts the
/// path blindly: a FIFO/device there can block startup or feed bytes without
/// bound. So: open, require the HANDLE (same-fd metadata, no re-resolution) to be
/// a regular file, and cap the read at [`CRED_FILE_MAX`]. On Unix the open itself
/// is `O_NONBLOCK` so a writerless FIFO cannot park the thread at `open()`; a
/// regular file opened `O_NONBLOCK` reads normally, so the legit case is
/// unchanged. Returns only the failure REASON — the caller owns the
/// "cert/key … unreadable; disabled" framing.
fn read_cred_file(p: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    #[cfg(unix)]
    let f = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(p)
            .map_err(|e| e.to_string())?
    };
    #[cfg(not(unix))]
    let f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let meta = f.metadata().map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() {
        return Err("not a regular file (FIFO/device/directory refused)".to_owned());
    }
    let mut bytes = Vec::new();
    f.take(CRED_FILE_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > CRED_FILE_MAX {
        return Err(format!("larger than {CRED_FILE_MAX} bytes; not a cert/key"));
    }
    Ok(bytes)
}

/// Whether this process was launched INSIDE another aterm.
///
/// ROOT-ONLY is the law it serves: only a top-level aterm opens a network
/// surface; otherwise a nested instance reading the shared
/// ~/.config/aterm/aterm.toml could bind a SECOND Owner-control listener, so this
/// explicit check is the guard. Two independent "launched inside aterm" signals so
/// it holds across BOTH spawn paths:
///   * ATERM_PARENT_SESSION_ID — injected on the recursion path (integrated shells);
///   * ATERM_CHILD=1 — the dedicated child marker set for EVERY child in main.rs's
///     baseline env_add, INCLUDING the `-e <cmd>` path that skips recursion
///     provisioning. Replaces the old `TERM_PROGRAM == "aterm"` check: a shell
///     may reset `TERM_PROGRAM` (its rc presents another identity to an app's
///     allowlist), so it is an identity claim, not a fact.
///
/// This reads PROCESS-GLOBAL environment state, so it belongs at the edges. The
/// diagnostics lane takes the answer as a parameter rather than calling this
/// deep inside its projection — see `listener_capability_warnings`.
pub(crate) fn launched_inside_aterm() -> bool {
    std::env::var_os(aterm_types::domain::ENV_PARENT_SESSION_ID).is_some()
        || std::env::var_os("ATERM_CHILD").is_some()
}

/// Bind the inbound TLS listener and spawn its serve loop relaying authorized
/// remote drivers into the local control socket at `sock_path` — but ONLY when the
/// bind address + cert + key are all configured in the `[net]` table of
/// `aterm.toml` (`listen`/`cert`/`key`). Any unset ⇒ no network port (the secure
/// default).
///
/// `token_hex` is this instance's 64-char control token. The listener sends it
/// on the relay's local `AUTH` line; the network capability is keyed by the
/// PERSISTENT drive token instead (R37), and by `token_hex` only when that
/// token's file can be neither read nor created.
pub(crate) fn maybe_spawn(token_hex: &str, sock_path: &str, saved: &crate::app_config::Config) {
    if launched_inside_aterm() {
        return;
    }
    let inputs = listener_inputs(saved);
    if !inputs.is_complete() {
        return; // secure default: no network port
    }

    // The per-launch stand-in for the channel-binding key. 64-char hex => 32 bytes.
    let Some(token) = EdgeToken::from_hex(token_hex) else {
        crate::logging::stderr_line!(
            "aterm-gui: control token is not 32-byte hex; network drive disabled"
        );
        return;
    };

    let prepared = match preflight_listener(&inputs) {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return,
        Err(error) => {
            crate::logging::stderr_line!("aterm-gui: {error}");
            return;
        }
    };
    let PreparedListener {
        addr,
        bind_addresses,
        key_path,
        cert_bytes,
        key_bytes,
    } = prepared;
    // PERSISTENT drive token (R37): the channel-binding secret a `dial` peer presents.
    // Stored beside the operator's TLS key, so a saved dial credential SURVIVES a
    // remote restart — provisioned ONCE, not re-copied after every restart. Falls
    // back to the per-launch control token if the file can't be read or created
    // (network drive still works, it just needs re-provisioning per restart —
    // pre-R37).
    let token_dir = key_path.parent().unwrap_or(std::path::Path::new(""));
    let token_file = token_dir.join(crate::control_auth::NETWORK_DRIVE_TOKEN_FILE);
    let (drive_hex, token_source) =
        match crate::control_auth::load_or_create_network_drive_token(token_dir) {
            Some((hex, true)) => (hex, DriveTokenSource::Minted),
            Some((hex, false)) => (hex, DriveTokenSource::Loaded),
            None => (
                token_hex.to_string(),
                DriveTokenSource::PerLaunch {
                    file_exists: std::fs::symlink_metadata(&token_file).is_ok(),
                },
            ),
        };
    let drive_token = EdgeToken::from_hex(&drive_hex).unwrap_or(token);
    // What every client's `fingerprint` must say, printed in the spelling it takes.
    let fingerprint =
        aterm_codec::hex::encode_lower(&aterm_net::tls::cert_fingerprint(&cert_bytes));
    // Preflight already compiled this exact byte pair. Rebuilding the cheap
    // rustls value here avoids exposing rustls as a second direct GUI dependency.
    let config = match aterm_net::tls::server_config(cert_bytes, key_bytes) {
        Ok(c) => c,
        Err(e) => {
            crate::logging::stderr_line!(
                "aterm-gui: server cert/key rejected ({e}); network drive disabled"
            );
            return;
        }
    };
    let listener = match TcpListener::bind(bind_addresses.as_slice()) {
        Ok(l) => l,
        Err(e) => {
            crate::logging::stderr_line!("aterm-gui: network drive bind failed at {addr}: {e}");
            return;
        }
    };

    let sock_path = sock_path.to_owned();
    // The local control socket is token-gated; the listener authenticates it on the
    // driver's behalf with a bare `AUTH <token>` line, so the raw token NEVER
    // crosses the network (only the channel-bound HMAC does). The remote driver then
    // speaks raw control verbs over the already-authenticated relay.
    let auth_line = format!("AUTH {token_hex}\n");
    note(
        &format!("network drive listening on {addr}; certificate sha256:{fingerprint}"),
        false,
    );
    // The raw token reaches stderr ONLY when a client must be provisioned with it
    // (a freshly minted file, or this launch's stand-in) — it is a full-authority
    // bearer secret, so echoing it on EVERY restart would multiply its exposure
    // (CWE-532) — and aterm.log never: there the line names the file instead.
    crate::logging::stderr_line!(
        "aterm-gui: {}",
        drive_token_line(token_source, &token_file, Some(&drive_hex))
    );
    let logged = drive_token_line(token_source, &token_file, None);
    if matches!(token_source, DriveTokenSource::PerLaunch { .. }) {
        aterm_log::warn!("{logged}");
    } else {
        aterm_log::info!("{logged}");
    }
    std::thread::spawn(move || {
        // Process-lifetime listener: aterm has no partial "stop just the network
        // drive" teardown, so this flag stays true for the life of the process.
        // It is a REAL kill-switch (`serve` polls it on a timer — see ACCEPT_POLL),
        // not dead control; it is simply never cleared here.
        let running = Arc::new(AtomicBool::new(true));
        aterm_net::drive::serve(
            &listener,
            &config,
            // Authority is `drive_token`, channel-bound: the PERSISTENT drive token,
            // or the per-launch control token only when that file is unusable
            // (`DriveTokenSource::PerLaunch`); `op` must be the drive op. With a
            // usable file the per-launch token serves only the local-socket AUTH
            // below, so a saved credential survives a restart (R37).
            move |_src, op| (op == NET_OP).then_some(drive_token),
            // Authorized connections are bridged to the local control socket, which
            // we authenticate here (the driver never sends the raw token).
            move || {
                use std::io::Write;
                let mut s = CtlStream::connect(&sock_path)?;
                s.write_all(auth_line.as_bytes())?;
                s.flush()?;
                Ok(s)
            },
            &running,
            move |ev| {
                listener_event(
                    &ev,
                    &crate::control::control_denials::NET_REFUSALS,
                    std::time::Instant::now(),
                );
            },
        );
    });
}

/// How this launch got its network-drive token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DriveTokenSource {
    /// Minted and saved this launch: clients must be provisioned with it.
    Minted,
    /// Read from the saved file: clients provisioned earlier keep working.
    Loaded,
    /// The file could not be read (`file_exists`) or created: this launch uses
    /// its per-launch token, which changes at every restart.
    PerLaunch { file_exists: bool },
}

/// The listener's startup line about its drive token. `secret` is the token,
/// printed with the one command a client runs when one must be provisioned now;
/// `None` (aterm.log) names only the file.
fn drive_token_line(
    source: DriveTokenSource,
    file: &std::path::Path,
    secret: Option<&str>,
) -> String {
    let file = file.display();
    match source {
        DriveTokenSource::Minted => {
            let provision = secret
                .map(|hex| format!("; on the client run: aterm ctl dial-token <name> {hex}"))
                .unwrap_or_default();
            format!("network drive token (new) saved to {file}{provision}")
        }
        DriveTokenSource::Loaded => {
            format!("network drive token: {file} (delete it and restart to issue a new one)")
        }
        DriveTokenSource::PerLaunch { file_exists } => {
            let provision = secret
                .map(|hex| format!(": aterm ctl dial-token <name> {hex}"))
                .unwrap_or_default();
            if file_exists {
                format!(
                    "network drive token file {file} is unusable; until it is removed and aterm \
                     restarts, clients use this launch's token{provision}"
                )
            } else {
                format!(
                    "network drive token file {file} cannot be created; until aterm can create \
                     it, clients use this launch's token and re-provision after each \
                     restart{provision}"
                )
            }
        }
    }
}

/// One listener event's line, and whether it is a refusal (logged as a
/// warning). Every connection line names the peer's address. `reason`/`error`
/// text can carry peer-influenced bytes (a rustls message about what it sent),
/// so it is sanitized: raw ESC/CR/BEL/C1 could forge a log record boundary or
/// smuggle a terminal escape to whoever `cat`s the log (CWE-117).
fn event_line(event: &NetEvent) -> (String, bool) {
    match event {
        NetEvent::Admitted { peer, .. } => (format!("network drive: {peer} connected"), false),
        NetEvent::Closed { peer, error: None } => {
            (format!("network drive: {peer} disconnected"), false)
        }
        NetEvent::Closed {
            peer,
            error: Some(error),
        } => (
            format!(
                "network drive: {peer} disconnected: {}",
                aterm_log::sanitize_record(error)
            ),
            true,
        ),
        NetEvent::Refused { peer, reason } => (
            format!(
                "network drive: {peer} not admitted: {}",
                aterm_log::sanitize_record(reason)
            ),
            true,
        ),
        NetEvent::Stopped(why) => (
            format!("network drive stopped: {}", aterm_log::sanitize_record(why)),
            true,
        ),
    }
}

/// One listener event, said ([`note`]) — a refusal only when `refusals` says
/// this one is written.
///
/// A refusal is a remote peer's to cause at will and before any token: a
/// connection that opens and closes is one. Each wrote a WARN line here and on
/// stderr, so a peer looping on connect wrote as fast as it could dial —
/// measured on a headless instance, 2000 lines (293 KB) for 2000 connections
/// in 0.1 s — and `aterm.log`'s two 4 MiB files, the containment audit trail
/// and this listener's own record of who was admitted rolled away in seconds.
/// The refusals are thinned as the local socket's handshake refusals are
/// ([`crate::control::control_denials`]), in ONE run for every peer: an address is no
/// identity a remote dialer cannot change (one IPv6 /64 holds 2^64). Every
/// refusal is counted, each line written says how many, and the admitted and
/// closed lines, which only a token holder causes, are written as they come.
fn listener_event(
    event: &NetEvent,
    refusals: &crate::control::control_denials::AuthDenials,
    now: std::time::Instant,
) {
    let (line, refusal) = event_line(event);
    let line = match event {
        NetEvent::Refused { .. } => match refusals.record_network(&line, now) {
            Some(line) => line,
            None => return,
        },
        NetEvent::Admitted { .. } | NetEvent::Closed { .. } | NetEvent::Stopped(_) => line,
    };
    note(&line, refusal);
}

/// A listener line to stderr AND aterm.log: a Dock-launched app's stderr goes
/// nowhere, and these lines are the listener's audit trail (drive.rs's
/// `NetEvent` mirrors the local socket's `log_denial`, which writes aterm.log).
fn note(line: &str, refusal: bool) {
    crate::logging::stderr_line!("aterm-gui: {line}");
    if refusal {
        aterm_log::warn!("{line}");
    } else {
        aterm_log::info!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CERT_DER: &[u8] = include_bytes!("../../aterm-net/src/testdata/cert.der");
    const TEST_KEY_DER: &[u8] = include_bytes!("../../aterm-net/src/testdata/key.pkcs8.der");

    fn test_dir(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "aterm-listener-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn listener_config(
        listen: &str,
        cert: &std::path::Path,
        key: &std::path::Path,
    ) -> crate::app_config::Config {
        aterm_toml::from_str(&format!(
            "[net]\nlisten = {listen:?}\ncert = {:?}\nkey = {:?}\n",
            cert.to_string_lossy(),
            key.to_string_lossy(),
        ))
        .unwrap()
    }

    fn fixture_files(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        std::fs::create_dir_all(root).unwrap();
        let cert = root.join("cert.der");
        let key = root.join("key.der");
        std::fs::write(&cert, TEST_CERT_DER).unwrap();
        std::fs::write(&key, TEST_KEY_DER).unwrap();
        (cert, key)
    }

    /// The cert/key reader must fail fast on a mispointed path instead of
    /// hanging (writerless FIFO, pre-fix: blocked at `open()`) or ballooning
    /// (pre-fix: uncapped `fs::read`): a regular file under the cap round-trips
    /// byte-for-byte, everything else errors.
    #[test]
    fn cred_reads_are_regular_file_only_and_size_capped() {
        let dir = std::env::temp_dir().join(format!("aterm-cred-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let ok = dir.join("cert.der");
        std::fs::write(&ok, b"not-a-real-cert").unwrap();
        assert_eq!(read_cred_file(&ok).unwrap(), b"not-a-real-cert");

        // One byte over the cap is refused, not slurped; exactly AT the cap is
        // fine (the take(cap + 1) sentinel only fires past it).
        let big = dir.join("big.der");
        std::fs::write(&big, vec![0u8; CRED_FILE_MAX as usize + 1]).unwrap();
        let err = read_cred_file(&big).unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        let atcap = dir.join("atcap.der");
        std::fs::write(&atcap, vec![0u8; CRED_FILE_MAX as usize]).unwrap();
        assert_eq!(read_cred_file(&atcap).unwrap().len() as u64, CRED_FILE_MAX);

        // A directory is not a regular file (on Unix the open succeeds and the
        // file-type check refuses it; on Windows the open itself errors).
        assert!(read_cred_file(&dir).is_err());

        #[cfg(unix)]
        {
            // A writerless 0600 FIFO: O_NONBLOCK makes the open return
            // immediately, and the same-fd file-type check refuses it.
            let fifo = dir.join("fifo.der");
            let cpath = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
            // SAFETY: `cpath` is a valid NUL-terminated path; mkfifo returns 0
            // or -1, which the assert checks.
            assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
            let err = read_cred_file(&fifo).unwrap_err();
            assert!(err.contains("not a regular file"), "{err}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listener_preflight_reports_missing_directory_and_oversize_credentials() {
        let root = test_dir("bad-files");
        let (_, key) = fixture_files(&root);
        let missing = root.join("missing.der");
        let error = preflight_listener(&listener_inputs(&listener_config(
            "127.0.0.1:7100",
            &missing,
            &key,
        )))
        .unwrap_err();
        assert_eq!(error.field, ListenerFailureField::Cert);
        assert!(error.message.contains("unreadable"), "{error}");

        let error = preflight_listener(&listener_inputs(&listener_config(
            "127.0.0.1:7100",
            &root,
            &key,
        )))
        .unwrap_err();
        assert_eq!(error.field, ListenerFailureField::Cert);
        assert!(error.message.contains("unreadable"), "{error}");

        let oversize = root.join("oversize.der");
        std::fs::write(&oversize, vec![0u8; CRED_FILE_MAX as usize + 1]).unwrap();
        let error = preflight_listener(&listener_inputs(&listener_config(
            "127.0.0.1:7100",
            &oversize,
            &key,
        )))
        .unwrap_err();
        assert_eq!(error.field, ListenerFailureField::Cert);
        assert!(error.message.contains("larger than"), "{error}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn listener_preflight_rejects_malformed_address_and_tls_pair_without_binding() {
        let root = test_dir("malformed");
        let (cert, key) = fixture_files(&root);
        let error = preflight_listener(&listener_inputs(&listener_config(
            "not a bind address",
            &cert,
            &key,
        )))
        .unwrap_err();
        assert_eq!(error.field, ListenerFailureField::Listen);
        assert!(error.message.contains("bind address"), "{error}");
        assert_eq!(error.config_keys(), ["net.listen"]);

        let error = preflight_listener(&listener_inputs(&listener_config(
            "localhost:7100",
            &cert,
            &key,
        )))
        .unwrap_err();
        assert_eq!(error.field, ListenerFailureField::Listen);
        assert!(error.message.contains("numeric IP:port"), "{error}");

        std::fs::write(&cert, b"not a DER certificate").unwrap();
        let error = preflight_listener(&listener_inputs(&listener_config(
            "127.0.0.1:7100",
            &cert,
            &key,
        )))
        .unwrap_err();
        assert_eq!(error.field, ListenerFailureField::CertAndKey);
        assert!(error.message.contains("rejected"), "{error}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn valid_listener_preflight_compiles_tls_but_never_binds() {
        let root = test_dir("valid");
        let (cert, key) = fixture_files(&root);
        let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = occupied.local_addr().unwrap().to_string();
        let prepared = preflight_listener(&listener_inputs(&listener_config(&addr, &cert, &key)))
            .unwrap()
            .expect("complete valid listener");
        assert_eq!(prepared.addr, addr);
        assert_eq!(prepared.bind_addresses, [occupied.local_addr().unwrap()]);
        assert_eq!(prepared.cert_bytes, TEST_CERT_DER);
        assert_eq!(prepared.key_bytes, TEST_KEY_DER);

        let _ = std::fs::remove_dir_all(root);
    }

    /// The token line names the file every time; the secret appears only where
    /// a client must be provisioned now, and never beside a claim that the
    /// per-launch stand-in is provisioned "once".
    #[test]
    fn the_drive_token_line_names_its_file_and_never_calls_the_stand_in_permanent() {
        let file = std::path::Path::new("/k/aterm-network-drive.token");
        let hex = "ab".repeat(32);
        assert_eq!(
            drive_token_line(DriveTokenSource::Minted, file, Some(&hex)),
            format!(
                "network drive token (new) saved to /k/aterm-network-drive.token; on the client \
                 run: aterm ctl dial-token <name> {hex}"
            )
        );
        assert_eq!(
            drive_token_line(DriveTokenSource::Minted, file, None),
            "network drive token (new) saved to /k/aterm-network-drive.token"
        );
        assert_eq!(
            drive_token_line(DriveTokenSource::Loaded, file, Some(&hex)),
            "network drive token: /k/aterm-network-drive.token (delete it and restart to issue \
             a new one)"
        );
        let stand_in = drive_token_line(
            DriveTokenSource::PerLaunch { file_exists: true },
            file,
            Some(&hex),
        );
        assert_eq!(
            stand_in,
            format!(
                "network drive token file /k/aterm-network-drive.token is unusable; until it is \
                 removed and aterm restarts, clients use this launch's token: aterm ctl \
                 dial-token <name> {hex}"
            )
        );
        assert!(!stand_in.contains("ONCE") && !stand_in.contains("once"));
        assert_eq!(
            drive_token_line(
                DriveTokenSource::PerLaunch { file_exists: false },
                file,
                None
            ),
            "network drive token file /k/aterm-network-drive.token cannot be created; until \
             aterm can create it, clients use this launch's token and re-provision after each \
             restart"
        );
    }

    /// THE AUDIT FLOOD: an unauthenticated peer that opens and closes a
    /// connection a millisecond, from a new address each time, writes the 1st,
    /// 2nd, 4th … of its refusals, each saying how many there were, not one line
    /// per connection (`listener_event`). A refusal after a quiet longer than
    /// the run is written at once, and an admitted peer is always written.
    #[test]
    fn a_flood_of_refused_connections_is_thinned_in_the_log() {
        use std::time::{Duration, Instant};
        crate::test_log::install();
        // TEST-NET-2 addresses: no other test in this crate names them.
        const NET: &str = "198.51.100.";
        let refusals = crate::control::control_denials::AuthDenials::new();
        let t0 = Instant::now();
        for i in 0..1000_u64 {
            let peer: SocketAddr = format!("{NET}{}:{}", 1 + i % 250, 10_000 + i)
                .parse()
                .unwrap();
            listener_event(
                &NetEvent::Refused {
                    peer,
                    reason: "received corrupt message of type InvalidContentType".into(),
                },
                &refusals,
                t0 + Duration::from_millis(i),
            );
        }
        let written = crate::test_log::containing(NET);
        assert_eq!(
            written.len(),
            10,
            "the 1st, 2nd, 4th … 512th of 1000 refusals: {:#?}",
            written.iter().map(|r| &r.message).collect::<Vec<_>>()
        );
        assert!(
            written
                .iter()
                .all(|r| r.level == aterm_log::Level::Warn && r.message.contains("not admitted"))
        );
        assert_eq!(
            written[0].message,
            "network drive: 198.51.100.1:10000 not admitted: received corrupt message of type \
             InvalidContentType",
            "the first refusal is written as it always was"
        );
        let last = &written[9].message;
        assert!(
            last.starts_with("network drive: 198.51.100.12:10511 not admitted: received corrupt")
                && last.ends_with("(512 refused in 511.0ms, 512 since launch)"),
            "{last}"
        );

        // Ten minutes of quiet: the next refusal starts a new run, written.
        let later = t0 + Duration::from_secs(600);
        let peer: SocketAddr = format!("{NET}251:4242").parse().unwrap();
        listener_event(
            &NetEvent::Refused {
                peer,
                reason: "wrong drive token".into(),
            },
            &refusals,
            later,
        );
        let written = crate::test_log::containing(NET);
        assert_eq!(written.len(), 11);
        assert!(
            written[10]
                .message
                .contains("not admitted: wrong drive token (again after "),
            "{}",
            written[10].message
        );
        assert!(written[10].message.ends_with("quiet; 1001 since launch)"));

        // A token holder's connection is never thinned.
        let peer: SocketAddr = format!("{NET}252:4243").parse().unwrap();
        let granted = aterm_net::Granted {
            src: "dial".into(),
            op: "drive".into(),
        };
        listener_event(&NetEvent::Admitted { peer, granted }, &refusals, later);
        assert!(
            crate::test_log::containing(&format!("{NET}252:4243 connected")).len() == 1,
            "an admitted peer is written"
        );
    }

    /// Every connection line names the peer; refusals say why, and a listener
    /// that stops says so rather than reading as a rejected connection.
    #[test]
    fn listener_event_lines_name_the_peer() {
        let peer: SocketAddr = "203.0.113.5:51234".parse().unwrap();
        let granted = aterm_net::Granted {
            src: "dial".into(),
            op: "drive".into(),
        };
        assert_eq!(
            event_line(&NetEvent::Admitted { peer, granted }),
            (
                "network drive: 203.0.113.5:51234 connected".to_owned(),
                false
            )
        );
        assert_eq!(
            event_line(&NetEvent::Closed { peer, error: None }),
            (
                "network drive: 203.0.113.5:51234 disconnected".to_owned(),
                false
            )
        );
        assert_eq!(
            event_line(&NetEvent::Refused {
                peer,
                reason: "wrong drive token".into()
            }),
            (
                "network drive: 203.0.113.5:51234 not admitted: wrong drive token".to_owned(),
                true
            )
        );
        assert_eq!(
            event_line(&NetEvent::Refused {
                peer,
                reason: "evil\u{1b}]0;x\u{7}".into()
            })
            .0,
            "network drive: 203.0.113.5:51234 not admitted: evil\u{fffd}]0;x\u{fffd}",
            "peer-influenced text is sanitized"
        );
        assert_eq!(
            event_line(&NetEvent::Stopped("no poll".into())),
            ("network drive stopped: no poll".to_owned(), true)
        );
    }
}
