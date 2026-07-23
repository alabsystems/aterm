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

use std::net::TcpListener;
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
pub fn maybe_spawn(token_hex: &str, sock_path: &str) {
    // ROOT-ONLY. Only a top-level aterm — NOT one launched inside another aterm —
    // opens a network surface; otherwise a nested instance reading the shared
    // ~/.config/aterm/aterm.toml could bind a SECOND Owner-control listener. The env
    // deny-list covers only the env selectors, never the shared config file, so this
    // explicit check is the guard. Two independent "launched inside aterm" signals so
    // it holds across BOTH spawn paths:
    //   * ATERM_PARENT_SESSION_ID — injected on the recursion path (integrated shells);
    //   * ATERM_CHILD=1 — the dedicated child marker set for EVERY child in main.rs's
    //     baseline env_add, INCLUDING the `-e <cmd>` path that skips recursion
    //     provisioning. Replaces the old `TERM_PROGRAM == "aterm"` check, which broke
    //     once TERM_PROGRAM became a configurable identity (honest default "aterm",
    //     overridable via ATERM_TERM_PROGRAM, e.g. =ghostty for app allowlists).
    let launched_inside_aterm = std::env::var_os(aterm_types::domain::ENV_PARENT_SESSION_ID)
        .is_some()
        || std::env::var_os("ATERM_CHILD").is_some();
    if launched_inside_aterm {
        return;
    }
    // Env wins, then the [net] config table. Unresolved fields ⇒ listener OFF.
    let net = crate::app_config::load_config().net.unwrap_or_default();
    let listen = std::env::var(ENV_LISTEN).ok().or(net.listen);
    let cert_path = std::env::var(ENV_CERT).ok().or(net.cert);
    let key_path = std::env::var(ENV_KEY).ok().or(net.key);
    let (Some(addr), Some(cert_path), Some(key_path)) = (listen, cert_path, key_path) else {
        return; // secure default: no network port
    };

    // The control token IS the channel-binding key. 64-char hex => 32 bytes.
    let Some(token) = EdgeToken::from_hex(token_hex) else {
        eprintln!("aterm-gui: control token is not 32-byte hex; network drive disabled");
        return;
    };

    // The documented sample config uses `~/...` paths — expand them (HOME, else
    // USERPROFILE on Windows) the same way the dial side expands `token_file`.
    let cert = match read_cred_file(&crate::net_connections::expand_tilde(&cert_path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aterm-gui: network-drive cert {cert_path} unreadable ({e}); disabled");
            return;
        }
    };
    let key_expanded = crate::net_connections::expand_tilde(&key_path);
    let key = match read_cred_file(&key_expanded) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("aterm-gui: network-drive key {key_path} unreadable ({e}); disabled");
            return;
        }
    };
    // PERSISTENT drive token (R37): the channel-binding secret a `dial` peer presents.
    // Stored beside the operator's TLS key, so a saved dial credential SURVIVES a
    // remote restart — provisioned ONCE, not re-copied after every restart. Falls
    // back to the per-launch control token if the file can't be created (network
    // drive still works, it just needs re-provisioning per restart — pre-R37).
    let (drive_hex, drive_minted) = std::path::Path::new(&key_expanded)
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
    let config = match aterm_net::tls::server_config(cert, key) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("aterm-gui: server cert/key rejected ({e}); network drive disabled");
            return;
        }
    };
    let listener = match TcpListener::bind(addr.as_str()) {
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
}
