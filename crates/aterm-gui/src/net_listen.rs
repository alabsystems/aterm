// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Secure-default-OFF network front-end for the control socket.
//!
//! aterm never opens a network port on its own. ONLY when an operator explicitly
//! sets all three of:
//!
//! * `ATERM_NET_LISTEN` — the bind address (e.g. `0.0.0.0:7100`),
//! * `ATERM_NET_CERT`    — path to the server certificate (DER),
//! * `ATERM_NET_KEY`     — path to its PKCS#8 private key (DER),
//!
//! does [`maybe_spawn`] stand up a TLS listener. Each accepted connection must
//! present a capability **channel-bound** to the TLS session
//! ([`aterm_net::channel_bind`]) keyed by THIS instance's control token; only then
//! is it relayed to the local control socket, where it authenticates again with
//! the ordinary `AUTH <token>` handshake. The TLS capability is the network-
//! specific gate that replaces the local same-uid `SO_PEERCRED` check — which has
//! no network analog — and, being bound to the TLS exporter, resists an active
//! MITM (a relay that terminates one TLS leg holds a different exporter, so a
//! captured tag never transfers).
//!
//! Missing any of the three env vars ⇒ this is a no-op. A malformed cert/key or a
//! failed bind is logged and the listener simply does not start — never a panic,
//! never a fallback to an unauthenticated port.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use aterm_net::drive::NetEvent;
use aterm_session::EdgeToken;
use aterm_uds::CtlStream;
// Canonical names (also deny-listed in `ENV_DENY_VARS`, so a nested aterm never
// inherits them — see env_sanitize). Single source of truth.
use aterm_types::domain::{
    ENV_NET_CERT as ENV_CERT, ENV_NET_KEY as ENV_KEY, ENV_NET_LISTEN as ENV_LISTEN,
};

/// The single op the network capability authorizes: driving this instance's
/// control socket. A remote driver presents `channel_bind(token, exporter)` for
/// this op; `src` is informational (logged), authority comes from the token.
const NET_OP: &str = "drive";

/// Upper bound on a cert/key file read. A real DER cert or PKCS#8 key is a few
/// KiB; 1 MiB is generous headroom while making a mispointed path — a device, a
/// runaway file — fail fast instead of growing memory without bound before
/// `server_config` ever gets to reject the bytes. Read via `take(cap + 1)` so
/// hitting the cap is distinguishable from a file of exactly the cap size.
const CRED_FILE_MAX: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenerValueSource {
    Config,
    Environment(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ListenerValue {
    value: String,
    source: ListenerValueSource,
}

impl ListenerValue {
    fn source_label(&self, config_key: &str) -> String {
        match self.source {
            ListenerValueSource::Config => config_key.to_string(),
            ListenerValueSource::Environment(variable) => format!("${variable}"),
        }
    }

    fn authored_config_key(&self, config_key: &'static str) -> Option<&'static str> {
        matches!(self.source, ListenerValueSource::Config).then_some(config_key)
    }
}

/// The three effective listener selectors after applying the startup precedence
/// exactly once. Environment values win even when empty or malformed, matching
/// the values [`maybe_spawn`] will attempt rather than diagnosing shadowed TOML.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ListenerInputs {
    listen: Option<ListenerValue>,
    cert: Option<ListenerValue>,
    key: Option<ListenerValue>,
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

/// Resolve the exact env-over-config listener generation used by startup.
pub(crate) fn listener_inputs(config: &crate::app_config::Config) -> ListenerInputs {
    listener_inputs_with(config, |variable| std::env::var(variable).ok())
}

fn listener_inputs_with(
    config: &crate::app_config::Config,
    mut environment: impl FnMut(&str) -> Option<String>,
) -> ListenerInputs {
    let net = config.net.as_ref();
    let resolve = |environment: &mut dyn FnMut(&str) -> Option<String>,
                   variable: &'static str,
                   configured: Option<&String>| {
        environment(variable)
            .map(|value| ListenerValue {
                value,
                source: ListenerValueSource::Environment(variable),
            })
            .or_else(|| {
                configured.cloned().map(|value| ListenerValue {
                    value,
                    source: ListenerValueSource::Config,
                })
            })
    };
    ListenerInputs {
        listen: resolve(
            &mut environment,
            ENV_LISTEN,
            net.and_then(|net| net.listen.as_ref()),
        ),
        cert: resolve(
            &mut environment,
            ENV_CERT,
            net.and_then(|net| net.cert.as_ref()),
        ),
        key: resolve(
            &mut environment,
            ENV_KEY,
            net.and_then(|net| net.key.as_ref()),
        ),
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
        .value
        .parse::<SocketAddr>()
        .map_err(|error| ListenerPreflightError {
            field: ListenerFailureField::Listen,
            config_keys: listen
                .authored_config_key("net.listen")
                .into_iter()
                .collect(),
            message: format!(
                "network listener {} value {:?} is not a numeric IP:port bind address ({error}); no port is bound",
                listen.source_label("net.listen"),
                listen.value,
            ),
        })?;
    let bind_addresses = vec![bind_address];

    let cert_path = crate::net_connections::expand_tilde(&cert.value);
    let cert_bytes = read_cred_file(&cert_path).map_err(|error| ListenerPreflightError {
        field: ListenerFailureField::Cert,
        config_keys: cert.authored_config_key("net.cert").into_iter().collect(),
        message: format!(
            "network listener certificate from {} ({:?}) is unreadable ({error}); no port is bound",
            cert.source_label("net.cert"),
            cert.value,
        ),
    })?;
    let key_path = crate::net_connections::expand_tilde(&key.value);
    let key_bytes = read_cred_file(&key_path).map_err(|error| ListenerPreflightError {
        field: ListenerFailureField::Key,
        config_keys: key.authored_config_key("net.key").into_iter().collect(),
        message: format!(
            "network listener private key from {} ({:?}) is unreadable ({error}); no port is bound",
            key.source_label("net.key"),
            key.value,
        ),
    })?;
    aterm_net::tls::server_config(cert_bytes.clone(), key_bytes.clone()).map_err(|error| {
        ListenerPreflightError {
            field: ListenerFailureField::CertAndKey,
            config_keys: [
                cert.authored_config_key("net.cert"),
                key.authored_config_key("net.key"),
            ]
            .into_iter()
            .flatten()
            .collect(),
            message: format!(
                "network listener certificate/key pair from {} and {} is rejected ({error}); no port is bound",
                cert.source_label("net.cert"),
                key.source_label("net.key"),
            ),
        }
    })?;

    Ok(Some(PreparedListener {
        addr: listen.value.clone(),
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
/// ~/.config/aterm/aterm.toml could bind a SECOND Owner-control listener. The env
/// deny-list covers only the env selectors, never the shared config file, so this
/// explicit check is the guard. Two independent "launched inside aterm" signals so
/// it holds across BOTH spawn paths:
///   * ATERM_PARENT_SESSION_ID — injected on the recursion path (integrated shells);
///   * ATERM_CHILD=1 — the dedicated child marker set for EVERY child in main.rs's
///     baseline env_add, INCLUDING the `-e <cmd>` path that skips recursion
///     provisioning. Replaces the old `TERM_PROGRAM == "aterm"` check, which broke
///     once TERM_PROGRAM became a configurable identity (honest default "aterm",
///     overridable via ATERM_TERM_PROGRAM, e.g. =ghostty for app allowlists).
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
/// bind address + cert + key are all configured. They are resolved from the env
/// (`ATERM_NET_LISTEN`/`_CERT`/`_KEY`, which WIN) else the `[net]` table of
/// `aterm.toml` (`listen`/`cert`/`key`). Any unresolved ⇒ no network port (the
/// secure default).
///
/// `token_hex` is this instance's 64-char control token; it is the HMAC key for
/// the network capability binding, so a remote driver must hold the same token it
/// would use for the local `AUTH` handshake — no new secret, no weaker gate.
pub fn maybe_spawn(token_hex: &str, sock_path: &str, saved: &crate::app_config::Config) {
    if launched_inside_aterm() {
        return;
    }
    let inputs = listener_inputs(saved);
    if !inputs.is_complete() {
        return; // secure default: no network port
    }

    // The control token IS the channel-binding key. 64-char hex => 32 bytes.
    let Some(token) = EdgeToken::from_hex(token_hex) else {
        eprintln!("aterm-gui: control token is not 32-byte hex; network drive disabled");
        return;
    };

    let prepared = match preflight_listener(&inputs) {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return,
        Err(error) => {
            eprintln!("aterm-gui: {error}");
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
    // back to the per-launch control token if the file can't be created (network
    // drive still works, it just needs re-provisioning per restart — pre-R37).
    let (drive_hex, drive_minted) = key_path
        .parent()
        .and_then(crate::control_auth::load_or_create_network_drive_token)
        .unwrap_or_else(|| {
            eprintln!(
                "aterm-gui: persistent network-drive token unavailable; using the \
                 per-launch token (re-provision the client after each restart)"
            );
            (token_hex.to_string(), true)
        });
    let drive_token = EdgeToken::from_hex(&drive_hex).unwrap_or(token);
    // Preflight already compiled this exact byte pair. Rebuilding the cheap
    // rustls value here avoids exposing rustls as a second direct GUI dependency.
    let config = match aterm_net::tls::server_config(cert_bytes, key_bytes) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-gui: server cert/key rejected ({e}); network drive disabled");
            return;
        }
    };
    let listener = match TcpListener::bind(bind_addresses.as_slice()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("aterm-gui: network drive bind failed at {addr}: {e}");
            return;
        }
    };

    let sock_path = sock_path.to_owned();
    let addr_owned = addr;
    // The local control socket is token-gated; the listener authenticates it on the
    // driver's behalf with a bare `AUTH <token>` line, so the raw token NEVER
    // crosses the network (only the channel-bound HMAC does). The remote driver then
    // speaks raw control verbs over the already-authenticated relay.
    let auth_line = format!("AUTH {token_hex}\n");
    eprintln!(
        "aterm-gui: network drive listening at {addr_owned} \
         (TLS, channel-bound capability, relays to the local control socket)"
    );
    // Reveal the raw PERSISTENT capability token to the operator's log ONLY on the
    // boot that mints it (provisioning time) — it is a full-authority bearer secret,
    // so echoing it on EVERY restart needlessly multiplies its exposure in logs
    // (CWE-532). On later boots emit only a stable fingerprint + the file location.
    if drive_minted {
        eprintln!(
            "aterm-gui: network-drive capability token (provision the client ONCE): {drive_hex}"
        );
        eprintln!("aterm-gui:   aterm-ctl dial-token <name> {drive_hex}");
    } else {
        let fp: String = drive_hex.chars().take(8).collect();
        eprintln!(
            "aterm-gui: network-drive token loaded (fingerprint {fp}…, stored 0600 beside the TLS key); \
             re-run with a fresh key dir to re-provision"
        );
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
            // Authority is the channel-bound PERSISTENT drive token; `op` must be the
            // drive op. (The per-launch `token` is used only for the local-socket
            // AUTH below — it never gates the network handshake now, so a saved
            // credential is not invalidated by a restart, R37.)
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
            move |ev| match ev {
                NetEvent::Relayed(g) => {
                    // `src`/`op` come from the peer's AUTH line (only emptiness is
                    // rejected upstream), so raw ESC/CR/BEL/C1 could survive and forge
                    // a log record boundary or smuggle a terminal escape to whoever
                    // `cat`s the operator's log (CWE-117). Sanitize both before print.
                    eprintln!(
                        "aterm-gui: network drive relayed a verified peer (src={}, op={})",
                        aterm_log::sanitize_record(&g.src),
                        aterm_log::sanitize_record(&g.op)
                    );
                }
                NetEvent::Rejected(why) => {
                    eprintln!(
                        "aterm-gui: network drive rejected a connection: {}",
                        aterm_log::sanitize_record(&why)
                    );
                }
            },
        );
    });
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
        toml::from_str(&format!(
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
    fn effective_listener_inputs_honor_environment_precedence_including_blank_values() {
        let root = test_dir("env-precedence");
        let (cert, key) = fixture_files(&root);
        let config: crate::app_config::Config = toml::from_str(
            "[net]\nlisten = \"config:1\"\ncert = \"config-cert\"\nkey = \"config-key\"\n",
        )
        .unwrap();
        let inputs = listener_inputs_with(&config, |variable| match variable {
            ENV_LISTEN => Some(String::new()),
            ENV_CERT => Some("env-cert".to_string()),
            ENV_KEY => Some("env-key".to_string()),
            _ => None,
        });
        assert_eq!(inputs.listen.as_ref().unwrap().value, "");
        assert_eq!(inputs.cert.as_ref().unwrap().value, "env-cert");
        assert_eq!(inputs.key.as_ref().unwrap().value, "env-key");
        assert!(matches!(
            inputs.listen.as_ref().unwrap().source,
            ListenerValueSource::Environment(ENV_LISTEN)
        ));
        let error = preflight_listener(&inputs).unwrap_err();
        assert_eq!(
            error.field,
            ListenerFailureField::Listen,
            "an explicitly blank environment override must not fall through to config"
        );
        assert!(
            error.config_keys().is_empty(),
            "an environment error has no TOML token for Manual to underline"
        );

        let inputs = listener_inputs_with(&config, |variable| match variable {
            ENV_LISTEN => Some("127.0.0.1:7100".to_string()),
            ENV_CERT => Some(cert.to_string_lossy().into_owned()),
            ENV_KEY => Some(key.to_string_lossy().into_owned()),
            _ => None,
        });
        assert!(
            preflight_listener(&inputs).unwrap().is_some(),
            "valid environment values must completely shadow invalid config values"
        );

        let _ = std::fs::remove_dir_all(root);
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
}
