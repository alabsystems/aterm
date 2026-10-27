// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! LAN PEER UNITS: a toolset archive taken from a LAN peer that already holds it, racing the
//! release host. A unit is named by the SHA-256 its signed index row carries, so a peer needs
//! no trust: its bytes are hashed as they arrive and land only when they match, and a peer that
//! sends other bytes is passed over while the release host's download goes on. The peers are
//! `[packages] peers` ([`configured`]). Serving is opt-in ([`serve`], `atpkg serve-units`): no
//! listener opens by default, and a store keeps units only while it serves ([`keep`]).

use std::io::{self, BufRead as _, Read, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::store::Layout;

/// The port a peer named without one is reached on, and `serve-units` listens on by default.
pub const DEFAULT_PORT: u16 = 7487;

/// The suffix of a peer download's partial beside its archive (`<asset>.peer.part`).
const PEER_PART_SUFFIX: &str = ".peer.part";

/// The suffix of a build's kept unit (`store/<program>/<build>.unit`, [`keep`]).
pub(crate) const UNIT_SUFFIX: &str = ".unit";

/// The request path a unit is served under: `/unit/<sha256>`.
const UNIT_PATH: &str = "/unit/";

/// How long a peer may take to accept the connection: a LAN peer answers at once or not at all.
const PEER_CONNECT: Duration = Duration::from_millis(750);

/// No byte from a peer for this long ends its transfer, and the release host's goes on.
const PEER_IDLE: Duration = Duration::from_secs(2);

/// The connections [`serve`] answers at once.
const SERVE_CONNECTIONS: usize = 8;

/// The longest request or response head read.
const REQUEST_HEAD_MAX: u64 = 8 * 1024;

/// The files an archive's bytes may arrive in while it downloads, in the order a reader looks:
/// the release host's partial (`<dl>.part`), a LAN peer's ([`race`]), and `dl` itself.
pub(crate) fn arrival_paths(dl: &Path) -> [PathBuf; 3] {
    [
        suffixed(dl, ".part"),
        suffixed(dl, PEER_PART_SUFFIX),
        dl.to_path_buf(),
    ]
}

fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// `[packages] peers`, each as `host:port` (the port [`DEFAULT_PORT`] when a peer names none);
/// an entry that is not a bare host, IPv4 or `[IPv6]` address with an optional port is dropped.
pub(crate) fn configured(peers: &[String]) -> Vec<String> {
    peers.iter().filter_map(|peer| peer_addr(peer)).collect()
}

fn peer_addr(peer: &str) -> Option<String> {
    let peer = peer.trim();
    let (host, port) = match peer.strip_prefix('[') {
        Some(rest) => {
            let (v6, tail) = rest.split_once(']')?;
            v6.parse::<std::net::Ipv6Addr>().ok()?;
            let port = match tail {
                "" => DEFAULT_PORT,
                _ => tail.strip_prefix(':')?.parse().ok()?,
            };
            (format!("[{v6}]"), port)
        }
        None => {
            let (host, port) = match peer.rsplit_once(':') {
                Some((host, port)) => (host, port.parse().ok()?),
                None => (peer, DEFAULT_PORT),
            };
            let host_ok = !host.is_empty()
                && host.len() <= 253
                && host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
            if !host_ok {
                return None;
            }
            (host.to_string(), port)
        }
    };
    (port != 0).then(|| format!("{host}:{port}"))
}

/// Whether `sha256` names a unit: 64 lowercase hex digits.
fn is_unit_name(sha256: &str) -> bool {
    sha256.len() == 64
        && sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Fetch the unit `sha256` (`size` signed bytes) to `dest` from the release host and `peers` at
/// once. `cdn` is the release host's download, which gives up when its flag is set; the peers
/// are asked one after another for the unit, each transfer hashed as it lands in
/// `<dest>.peer.part`. Whichever source holds the whole unit first is `dest`: a peer's only
/// when it sent exactly `size` bytes that hash to `sha256`, and the other source is stopped.
/// With no peer that holds the unit, or a row that signs no size, this is `cdn` alone.
pub(crate) fn race(
    peers: &[String],
    sha256: &str,
    size: u64,
    dest: &Path,
    cdn: impl FnOnce(&Path, &AtomicBool) -> Result<(), String>,
) -> Result<(), String> {
    let sha256 = sha256.to_ascii_lowercase();
    if peers.is_empty() || size == 0 || !is_unit_name(&sha256) {
        return cdn(dest, &AtomicBool::new(false));
    }
    let part = suffixed(dest, PEER_PART_SUFFIX);
    let cdn_stop = AtomicBool::new(false);
    let peer_stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let from_peer = std::thread::Builder::new()
            .name("atpkg-peer-unit".into())
            .spawn_scoped(scope, || {
                let got = from_peers(peers, &sha256, size, &part, &peer_stop);
                if got.is_some() {
                    cdn_stop.store(true, Ordering::Release);
                }
                got
            });
        let from_cdn = cdn(dest, &cdn_stop);
        if from_cdn.is_ok() {
            peer_stop.store(true, Ordering::Release);
        }
        let peer = from_peer.ok().and_then(|t| t.join().ok()).flatten();
        match (from_cdn, peer) {
            (Ok(()), _) => {
                let _ = std::fs::remove_file(&part);
                Ok(())
            }
            (Err(_), Some(peer)) => {
                std::fs::rename(&part, dest).map_err(|e| {
                    let _ = std::fs::remove_file(&part);
                    format!("keep the unit LAN peer {peer} sent: {e}")
                })?;
                println!(
                    "atpkg: took {sha256:.12}… from LAN peer {peer} (its bytes match the signed digest)"
                );
                Ok(())
            }
            (Err(e), None) => {
                let _ = std::fs::remove_file(&part);
                Err(e)
            }
        }
    })
}

/// Ask `peers` in turn for the unit until one sends the whole of it with the signed digest:
/// that peer, its bytes at `part`. `None` when none did, or `stop` was set first.
fn from_peers(
    peers: &[String],
    sha256: &str,
    size: u64,
    part: &Path,
    stop: &AtomicBool,
) -> Option<String> {
    for peer in peers {
        if stop.load(Ordering::Acquire) {
            break;
        }
        match from_peer(peer, sha256, size, part, stop) {
            Ok(()) => return Some(peer.clone()),
            Err(PeerMiss::Other(got)) => {
                eprintln!(
                    "atpkg: LAN peer {peer} sent bytes that hash to {got:.12}…, not the signed \
                     unit {sha256:.12}…; passed over"
                );
            }
            Err(PeerMiss::Unavailable) => {}
        }
        let _ = std::fs::remove_file(part);
    }
    None
}

/// Why one peer's transfer did not land.
enum PeerMiss {
    /// No answer, no unit, a short or oversized body, or a stop.
    Unavailable,
    /// The whole body arrived and hashes to this, not the signed digest.
    Other(String),
}

fn from_peer(
    peer: &str,
    sha256: &str,
    size: u64,
    part: &Path,
    stop: &AtomicBool,
) -> Result<(), PeerMiss> {
    let mut conn = connect(peer).ok_or(PeerMiss::Unavailable)?;
    let unavailable = |_| PeerMiss::Unavailable;
    conn.set_read_timeout(Some(PEER_IDLE))
        .map_err(unavailable)?;
    conn.set_write_timeout(Some(PEER_IDLE))
        .map_err(unavailable)?;
    write!(
        conn,
        "GET {UNIT_PATH}{sha256} HTTP/1.1\r\nHost: {peer}\r\nConnection: close\r\n\r\n"
    )
    .map_err(unavailable)?;
    let mut body = io::BufReader::new(conn);
    let declared = response_length(&mut body).ok_or(PeerMiss::Unavailable)?;
    if declared != size {
        return Err(PeerMiss::Unavailable);
    }
    let mut file = std::fs::File::create(part).map_err(unavailable)?;
    let mut digest = crate::tree::BulkSha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    while got < declared {
        if stop.load(Ordering::Acquire) {
            return Err(PeerMiss::Unavailable);
        }
        let want = usize::try_from(declared - got).map_or(buf.len(), |rest| rest.min(buf.len()));
        let n = match body.read(buf.get_mut(..want).ok_or(PeerMiss::Unavailable)?) {
            Ok(0) | Err(_) => return Err(PeerMiss::Unavailable),
            Ok(n) => n,
        };
        let bytes = buf.get(..n).ok_or(PeerMiss::Unavailable)?;
        got = got.saturating_add(n as u64);
        digest.update(bytes);
        file.write_all(bytes).map_err(unavailable)?;
    }
    let got = digest.finalize_hex();
    if got.eq_ignore_ascii_case(sha256) {
        Ok(())
    } else {
        Err(PeerMiss::Other(got))
    }
}

/// A connection to `peer` (`host:port`), each of its addresses tried within [`PEER_CONNECT`].
fn connect(peer: &str) -> Option<TcpStream> {
    use std::net::ToSocketAddrs as _;
    peer.to_socket_addrs()
        .ok()?
        .find_map(|addr| TcpStream::connect_timeout(&addr, PEER_CONNECT).ok())
}

/// Read a response head from `conn`: the body's `Content-Length` when the status is 200 and
/// the head declares one, within [`REQUEST_HEAD_MAX`].
fn response_length(conn: &mut impl io::BufRead) -> Option<u64> {
    let mut head = conn.take(REQUEST_HEAD_MAX);
    let mut line = String::new();
    head.read_line(&mut line).ok()?;
    let ok = line.split_ascii_whitespace().nth(1) == Some("200");
    let mut length = None;
    loop {
        line.clear();
        if head.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok();
        }
    }
    ok.then_some(length).flatten()
}

/// `<prefix>/units`: the units a serving store keeps, each a link named by its SHA-256 to a
/// build's kept unit (`<build>.unit`). Its presence is the opt-in [`serve`] records.
fn units_dir(layout: &Layout) -> PathBuf {
    layout.prefix.join("units")
}

/// When this store serves units ([`units_dir`] is there), keep `dl` — the download a build at
/// `build_dir` was staged from, whose signed row names `sha256` — as that unit: linked beside
/// the build (`<build>.unit`, which goes when the build does) and under `units/`. The bytes are
/// hashed first; a file that does not hash to `sha256` is not kept.
pub(crate) fn keep(layout: &Layout, dl: &Path, sha256: &str, build_dir: &Path) {
    let units = units_dir(layout);
    let sha256 = sha256.to_ascii_lowercase();
    if !is_unit_name(&sha256) || !std::fs::symlink_metadata(&units).is_ok_and(|m| m.is_dir()) {
        return;
    }
    sweep(&units);
    let named = units.join(&sha256);
    if std::fs::symlink_metadata(&named).is_ok() {
        return;
    }
    if !crate::tree::file_sha256(dl).is_ok_and(|got| got == sha256) {
        return;
    }
    let Some(beside) = crate::store::sidecar_path(build_dir, UNIT_SUFFIX) else {
        return;
    };
    let _ = std::fs::remove_file(&beside);
    if std::fs::hard_link(dl, &beside).is_ok() && std::fs::hard_link(&beside, &named).is_err() {
        let _ = std::fs::remove_file(&beside);
    }
}

/// Remove every kept unit no build holds any more (`units/` entries with no other link).
fn sweep(units: &Path) {
    let Ok(entries) = std::fs::read_dir(units) else {
        return;
    };
    for entry in entries.flatten() {
        if let Ok(meta) = std::fs::symlink_metadata(entry.path())
            && (!meta.is_file() || links(&meta) < 2)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(unix)]
fn links(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    meta.nlink()
}

#[cfg(not(unix))]
fn links(_meta: &std::fs::Metadata) -> u64 {
    // No link count to read: a kept unit stands until `serve-units --off` sweeps it.
    2
}

/// Stop serving: remove `units/` and every build's kept unit, so this store keeps none.
///
/// # Errors
/// The `units/` directory could not be removed.
pub fn serve_off(layout: &Layout) -> io::Result<()> {
    let units = units_dir(layout);
    if std::fs::symlink_metadata(&units).is_ok() {
        crate::store::remove_tree(&units)?;
    }
    let store = layout.prefix.join("store");
    for program in std::fs::read_dir(store).into_iter().flatten().flatten() {
        for entry in std::fs::read_dir(program.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name();
            if name
                .to_str()
                .and_then(|n| n.strip_suffix(UNIT_SUFFIX))
                .is_some_and(|build| crate::store::parse_build_name(build).is_some())
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(())
}

/// Serve this store's kept units on `listen` until the process ends — the opt-in
/// `atpkg serve-units`. Starting records the opt-in ([`units_dir`]), so the store keeps each
/// unit it installs from then on ([`keep`]). `GET /unit/<sha256>` answers a kept unit's bytes;
/// anything else is answered 404, 400 or 405 and closed. `ready` hears the bound address.
///
/// # Errors
/// The opt-in could not be recorded, or `listen` could not be bound.
pub fn serve(
    layout: &Layout,
    listen: SocketAddr,
    ready: impl FnOnce(SocketAddr),
) -> io::Result<()> {
    let units = units_dir(layout);
    crate::platform::make_dirs_born(&units, 0o755)?;
    sweep(&units);
    let listener = TcpListener::bind(listen)?;
    ready(listener.local_addr()?);
    let live = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(conn) = conn else {
            continue;
        };
        if live.fetch_add(1, Ordering::AcqRel) >= SERVE_CONNECTIONS {
            live.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let (dir, held) = (units.clone(), Arc::clone(&live));
        let spawned = std::thread::Builder::new()
            .name("atpkg-serve-unit".into())
            .spawn(move || {
                let _ = answer(conn, &dir);
                held.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            live.fetch_sub(1, Ordering::AcqRel);
        }
    }
    Ok(())
}

/// Answer one connection's request ([`serve`]).
fn answer(conn: TcpStream, units: &Path) -> io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(10)))?;
    conn.set_write_timeout(Some(Duration::from_secs(60)))?;
    let mut reader = io::BufReader::new(conn.try_clone()?.take(REQUEST_HEAD_MAX));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_ascii_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    // The rest of the head is read and ignored: the request line says everything.
    let mut header = String::new();
    while reader.read_line(&mut header)? > 0 && !header.trim_end().is_empty() {
        header.clear();
    }
    let mut conn = conn;
    if method != "GET" {
        return status(&mut conn, "405 Method Not Allowed");
    }
    let Some(sha256) = target.strip_prefix(UNIT_PATH).filter(|s| is_unit_name(s)) else {
        return status(&mut conn, "404 Not Found");
    };
    let path = units.join(sha256);
    let held = std::fs::symlink_metadata(&path)
        .ok()
        .filter(|meta| meta.is_file() && links(meta) >= 2);
    let Some(meta) = held else {
        // A unit no build holds any more is swept as it is missed.
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
            let _ = std::fs::remove_file(&path);
        }
        return status(&mut conn, "404 Not Found");
    };
    let mut file = std::fs::File::open(&path)?;
    write!(
        conn,
        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        meta.len()
    )?;
    io::copy(&mut file, &mut conn)?;
    conn.flush()
}

fn status(conn: &mut TcpStream, line: &str) -> io::Result<()> {
    write!(
        conn,
        "HTTP/1.1 {line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    conn.flush()
}

#[cfg(test)]
#[path = "peers_tests.rs"]
mod tests;
