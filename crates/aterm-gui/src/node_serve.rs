// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Foreground runtime for `systems.alab.aterm-node` (`aterm node serve`,
//! `docs/DESIGN-alab-swarm-2026-10-27.md` §3.3, §7, §12.3 C20–C23).
//!
//! Holds `<state_dir>/node-job.lock` so interactive windows yield port 7100, serves fleet `Join`
//! and `Book` as every member does even when no window is open, serves `Facts`, `MailCode` and
//! `Census`, relays `Survey` and `Drive` to any live window's `wf-<pid>.sock` relay, answers windowless `Survey` (`window=0`, `instances=0`), wakes a background window on
//! windowless `Drive` `spawn` (or answers `"ERR logged out\n"` when no GUI console session is
//! active, §7.4), admits by this Mac's membership log, tends `_alab._tcp` on every network, and
//! browses it for the places its heartbeat dials (`docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1,
//! F17).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::node_service::{AlabTcpAdvertiser, NodeJobLock};
use aterm_net::fleet::{FleetOp, FleetScope};
use aterm_uds::{CtlListener, CtlStream};

use super::{
    CENSUS_REFRESH_EVERY, FACTS_HEARTBEAT_EVERY, HeartbeatClock, Serving, Wanted, admit_driver_at,
    denied_under, fleet_event, fleet_slots, grants_of, heartbeat_every, holds_in, level_of,
    lifetime, lists_driver_at, maybe_refresh_local_census_at, push_facts_to_issuer,
    receive_pushed_facts_at, refuse_driver, register_member_at, retry, serve_census_request_at,
    serve_mail_code_request_at, share_book_at, share_units, spawn_member_heartbeat_at,
    update_denied_nodes_cache,
};

const RELAY_PREFIX: &str = "wf-";
const RELAY_SUFFIX: &str = ".sock";
const RELAY_HEADER_MAX: usize = 1024;
#[cfg(not(test))]
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// A unit test's window relays are its own, which answer, and its hand-overs run on node-job
/// threads a per-thread bound would not reach: they wait the hang detector.
#[cfg(test)]
const RELAY_CONNECT_TIMEOUT: Duration = aterm_spec::wait::MUST_HAPPEN;
/// A peer's fleet handshake, and each read and write of a served exchange and its window
/// hand-over, within aterm-net's `HANDSHAKE_WITHIN`; a unit test's dialers and windows are its
/// own, which answer, so theirs wait the hang detector.
#[cfg(not(test))]
const HANDSHAKE: Duration = aterm_net::fleet::HANDSHAKE_WITHIN;
#[cfg(test)]
const HANDSHAKE: Duration = aterm_spec::wait::MUST_HAPPEN;
const DEFAULT_WAKE_TIMEOUT: Duration = Duration::from_secs(15);
const LAUNCH_COOLDOWN: Duration = Duration::from_secs(5);
/// How long `open -g -a` may take to hand the app to Launch Services before the spawn reports it
/// stuck, so a wedged `open` never holds the fleet connection that asked for a window.
#[cfg(target_os = "macos")]
const OPEN_WITHIN: Duration = Duration::from_secs(30);
const DEFAULT_FLEET_LISTEN: &str = "0.0.0.0:7100";

static WINDOW_RELAY_STARTED: AtomicBool = AtomicBool::new(false);
static WINDOW_RELAY_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Run `open` (described as `what`) to its end within `within`: past it, the run is killed and
/// reported stuck, and a non-zero exit is reported with what it wrote to stderr.
#[cfg(any(target_os = "macos", test))]
fn run_open_within(
    open: &mut aterm_uds::spawnfd::Command,
    what: &str,
    within: Duration,
) -> Result<(), String> {
    let out = aterm_uds::deadline::output_until(open, Instant::now() + within)
        .map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what} exited with {}: {}",
            out.status,
            aterm_log::sanitize_record(String::from_utf8_lossy(&out.stderr).trim())
        ))
    }
}

/// Seam for launching a background `aterm` window when a remote `spawn` arrives on a windowless
/// Mac (`docs/DESIGN-alab-swarm-2026-10-27.md` §7.4, C20).
pub trait WindowLauncher: Send + Sync {
    /// Whether a graphical login session is active for this user (`false` -> `"ERR logged out\n"`).
    fn is_logged_in(&self) -> bool;

    /// Launch a background `aterm` window so its `wf-<pid>.sock` relay comes online in `relay_dir`.
    ///
    /// # Errors
    /// Returns a human-readable error if spawning the background window failed.
    fn launch_window(&self, state_dir: &Path, relay_dir: &Path) -> Result<(), String>;
}

/// Production [`WindowLauncher`] checking `/dev/console` ownership on macOS (or `DISPLAY` /
/// `WAYLAND_DISPLAY` elsewhere) and running `open -g -a <bundle>` (or `aterm --window`).
pub struct LiveWindowLauncher {
    last_launch: Mutex<Option<Instant>>,
}

impl Default for LiveWindowLauncher {
    fn default() -> Self {
        Self {
            last_launch: Mutex::new(None),
        }
    }
}

impl WindowLauncher for LiveWindowLauncher {
    fn is_logged_in(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata("/dev/console")
                .is_ok_and(|meta| meta.uid() == crate::control_auth::our_uid())
        }
        #[cfg(not(target_os = "macos"))]
        {
            std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
        }
    }

    fn launch_window(&self, _state_dir: &Path, _relay_dir: &Path) -> Result<(), String> {
        {
            let mut guard = self
                .last_launch
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            if guard.is_some_and(|at| now.saturating_duration_since(at) < LAUNCH_COOLDOWN) {
                return Ok(());
            }
            *guard = Some(now);
        }

        let exe =
            std::env::current_exe().map_err(|e| format!("cannot read current executable: {e}"))?;
        #[cfg(target_os = "macos")]
        if let Some(bundle) = macos_app_bundle_of(&exe) {
            let mut open = aterm_uds::spawnfd::command("/usr/bin/open");
            open.args(["-g", "-a"]).arg(&bundle);
            return run_open_within(
                &mut open,
                &format!("open -g -a {}", bundle.display()),
                OPEN_WITHIN,
            );
        }

        use aterm_uds::spawnfd::Stdio;
        let _child = aterm_uds::spawnfd::command(&exe)
            .arg("--window")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{} --window: {e}", exe.display()))?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn macos_app_bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos_dir = exe.parent()?;
    if macos_dir.file_name().and_then(|n| n.to_str()) != Some("MacOS") {
        return None;
    }
    let contents_dir = macos_dir.parent()?;
    if contents_dir.file_name().and_then(|n| n.to_str()) != Some("Contents") {
        return None;
    }
    let bundle = contents_dir.parent()?;
    bundle
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        .then(|| bundle.to_path_buf())
}

/// Parsed greeting frame sent by `aterm node serve` over `<relay_dir>/wf-<pid>.sock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RelayHeader {
    Survey {
        content: bool,
        drive: bool,
        peer: Option<SocketAddr>,
    },
    Drive {
        driver: crate::fabric::Driver,
        key: Option<aterm_net::fleet::NodeKey>,
        peer: Option<SocketAddr>,
    },
    /// A Mac with no fleet secret asking to join ALab from `peer`: the window's members desk
    /// serves it once the node job writes `go` (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10).
    Ask { peer: SocketAddr },
}

impl RelayHeader {
    fn encode(&self) -> String {
        match self {
            Self::Survey {
                content,
                drive,
                peer,
            } => format!(
                "SURVEY content={} drive={} peer={}\n",
                u8::from(*content),
                u8::from(*drive),
                peer.map_or_else(|| "-".to_owned(), |p| p.to_string()),
            ),
            Self::Drive { driver, key, peer } => match key {
                Some(key) => format!(
                    "DRIVE node={} host={} key={} peer={}\n",
                    aterm_codec::pct::encode(&driver.node),
                    aterm_codec::pct::encode(&driver.host),
                    key.hex(),
                    peer.map_or_else(|| "-".to_owned(), |p| p.to_string()),
                ),
                None => format!(
                    "DRIVE node={} host={} peer={}\n",
                    aterm_codec::pct::encode(&driver.node),
                    aterm_codec::pct::encode(&driver.host),
                    peer.map_or_else(|| "-".to_owned(), |p| p.to_string()),
                ),
            },
            Self::Ask { peer } => format!("ASK peer={peer}\n"),
        }
    }

    pub(crate) fn parse(line: &str) -> Result<Self, &'static str> {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let mut words = trimmed.split_ascii_whitespace();
        let Some(kind) = words.next() else {
            return Err("empty relay header");
        };
        match kind {
            "SURVEY" => {
                let mut content = None;
                let mut drive = None;
                let mut peer = None;
                for word in words {
                    let Some((k, v)) = word.split_once('=') else {
                        return Err("malformed SURVEY token");
                    };
                    match k {
                        "content" => {
                            content = match v {
                                "0" => Some(false),
                                "1" => Some(true),
                                _ => return Err("invalid content flag"),
                            };
                        }
                        "drive" => {
                            drive = match v {
                                "0" => Some(false),
                                "1" => Some(true),
                                _ => return Err("invalid drive flag"),
                            };
                        }
                        "peer" => {
                            peer = Some(if v == "-" {
                                None
                            } else {
                                Some(v.parse::<SocketAddr>().map_err(|_| "invalid peer addr")?)
                            });
                        }
                        _ => return Err("unknown SURVEY key"),
                    }
                }
                Ok(Self::Survey {
                    content: content.ok_or("missing content flag")?,
                    drive: drive.ok_or("missing drive flag")?,
                    peer: peer.ok_or("missing peer field")?,
                })
            }
            "DRIVE" => {
                let mut node = None;
                let mut host = None;
                let mut key = None;
                let mut peer = None;
                for word in words {
                    let Some((k, v)) = word.split_once('=') else {
                        return Err("malformed DRIVE token");
                    };
                    match k {
                        "node" => {
                            let decoded = aterm_codec::pct::decode(v);
                            if decoded.is_empty() {
                                return Err("empty driver node");
                            }
                            node = Some(decoded);
                        }
                        "host" => {
                            let decoded = aterm_codec::pct::decode(v);
                            if decoded.is_empty() {
                                return Err("empty driver host");
                            }
                            host = Some(decoded);
                        }
                        "key" => {
                            key = Some(
                                aterm_net::fleet::NodeKey::from_hex(v)
                                    .ok_or("invalid driver key")?,
                            );
                        }
                        "peer" => {
                            peer = Some(if v == "-" {
                                None
                            } else {
                                Some(v.parse::<SocketAddr>().map_err(|_| "invalid peer addr")?)
                            });
                        }
                        _ => return Err("unknown DRIVE key"),
                    }
                }
                Ok(Self::Drive {
                    driver: crate::fabric::Driver {
                        node: node.ok_or("missing driver node")?,
                        host: host.ok_or("missing driver host")?,
                    },
                    key,
                    peer: peer.ok_or("missing peer field")?,
                })
            }
            "ASK" => match (words.next(), words.next()) {
                (Some(word), None) => Ok(Self::Ask {
                    peer: word
                        .strip_prefix("peer=")
                        .ok_or("missing peer field")?
                        .parse::<SocketAddr>()
                        .map_err(|_| "invalid peer addr")?,
                }),
                _ => Err("malformed ASK header"),
            },
            _ => Err("unknown relay command"),
        }
    }
}

fn relay_socket_path(relay_dir: &Path, pid: u32) -> PathBuf {
    relay_dir.join(format!("{RELAY_PREFIX}{pid}{RELAY_SUFFIX}"))
}

fn parse_relay_filename(name: &str) -> Option<u32> {
    let pid_str = name
        .strip_prefix(RELAY_PREFIX)?
        .strip_suffix(RELAY_SUFFIX)?;
    pid_str.parse::<u32>().ok().filter(|&p| p > 0)
}

fn sweep_dead_relay_sockets(relay_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(relay_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if let Some(pid) = parse_relay_filename(name_str)
            && !crate::control_auth::pid_alive(pid)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Start the current window's `wf-<pid>.sock` relay inside `sock_dir` once `BRIDGE_CONTEXT` is
/// published, so `aterm node serve` can forward `Survey` and `Drive` connections to this window.
pub(crate) fn start_window_fleet_relay(sock_dir: &Path) {
    if WINDOW_RELAY_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    {
        let mut slot = WINDOW_RELAY_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *slot = Some(sock_dir.to_path_buf());
    }
    if let Err(err) = std::thread::Builder::new()
        .name("aterm-window-fleet-relay".to_owned())
        .spawn(run_window_fleet_relay)
    {
        WINDOW_RELAY_STARTED.store(false, Ordering::Release);
        aterm_log::warn!("window fleet relay thread could not start: {err}");
    }
}

fn run_window_fleet_relay() {
    crate::qos::set_self(crate::qos::Role::Responsive);
    let dir = {
        let slot = WINDOW_RELAY_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.clone()
    };
    let Some(dir) = dir else {
        return;
    };
    let pid = std::process::id();
    let _ = serve_window_relay_at(&dir, pid, None, |header, stream| match header {
        RelayHeader::Survey {
            content,
            drive,
            peer,
        } => crate::control::attach_fleet_connection(stream, (content, drive), peer),
        RelayHeader::Drive { driver, key, peer } => {
            if let Some(key) = key {
                super::note_admitted_driver(&driver.node, key);
            }
            crate::control::attach_fleet_drive(stream, peer, driver)
        }
        RelayHeader::Ask { peer } => crate::members_desk::attach_relayed(stream, peer),
    });
}

/// The node job's `ask` handler: each asking Mac's exchange relayed to the window's members
/// desk ([`RelayHeader::Ask`], [`aterm_secrets::members::relay`]), refused while no window runs
/// or while a running one takes no hand-over (then as busy, never as no window).
fn relay_ask(relay_dir: PathBuf) -> aterm_net::fleet::AskHandler {
    Arc::new(
        move |stream: &mut dyn aterm_net::fleet::Duplex, peer: SocketAddr| {
            let mut window = match relay_to_window(&relay_dir, &RelayHeader::Ask { peer }) {
                RelayOpen::Opened(window) => window,
                RelayOpen::NoWindow => {
                    let _ =
                        aterm_secrets::members::refuse(stream, "this Mac's window is not running");
                    return Ok(());
                }
                RelayOpen::Busy(why) => {
                    let _ = aterm_secrets::members::refuse(
                        stream,
                        &format!(
                            "this Mac's window is open and took no fleet connection ({}); ask again",
                            aterm_log::sanitize_record(&why)
                        ),
                    );
                    return Ok(());
                }
            };
            let _ = window.set_read_timeout(Some(HANDSHAKE));
            let _ = window.set_write_timeout(Some(HANDSHAKE));
            if window
                .write_all(b"go\n")
                .and_then(|()| window.flush())
                .is_ok()
                && let Err(error) = aterm_secrets::members::relay(stream, &mut window)
            {
                aterm_log::info!("node: an ask from {peer} ended: {error}");
            }
            aterm_uds::hangup::hang_up(&window);
            Ok(())
        },
    )
}

pub(crate) fn serve_window_relay_at<F>(
    relay_dir: &Path,
    pid: u32,
    stop: Option<&AtomicBool>,
    attach: F,
) -> std::io::Result<()>
where
    F: Fn(RelayHeader, CtlStream) -> bool + Send + Sync + 'static,
{
    crate::control_auth::ensure_control_dir(relay_dir)?;
    sweep_dead_relay_sockets(relay_dir);
    let sock_path = relay_socket_path(relay_dir, pid);
    let _ = std::fs::remove_file(&sock_path);
    let listener = CtlListener::bind(&sock_path)?;
    let _ = aterm_uds::keep_out_of_children(&listener);
    let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
    if stop.is_some() {
        listener.set_nonblocking(true)?;
    }

    struct UnlinkOnDrop(PathBuf);
    impl Drop for UnlinkOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _unlink = UnlinkOnDrop(sock_path);
    let attach = Arc::new(attach);

    loop {
        if stop.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Ok(());
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = aterm_uds::keep_out_of_children(&stream);
                if crate::control_auth::peer_check(&stream).is_err() {
                    aterm_uds::hangup::hang_up(&stream);
                    continue;
                }
                let attach = Arc::clone(&attach);
                let _ = std::thread::Builder::new()
                    .name("aterm-wf-admit".to_owned())
                    .spawn(move || {
                        handle_relay_accept(&mut stream, &*attach);
                    });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

fn handle_relay_accept(stream: &mut CtlStream, attach: &dyn Fn(RelayHeader, CtlStream) -> bool) {
    let _ = stream.set_read_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let line = match read_line_raw(stream, RELAY_HEADER_MAX) {
        Ok(Some(line)) => line,
        Ok(None) | Err(_) => {
            aterm_uds::hangup::hang_up(stream);
            return;
        }
    };
    let header = match RelayHeader::parse(&line) {
        Ok(header) => header,
        Err(why) => {
            let _ = stream.write_all(format!("ERR {why}\n").as_bytes());
            let _ = stream.flush();
            aterm_uds::hangup::hang_up(stream);
            return;
        }
    };
    let Ok(cloned) = stream.try_clone() else {
        aterm_uds::hangup::hang_up(stream);
        return;
    };
    let _ = aterm_uds::keep_out_of_children(&cloned);
    let _ = cloned.set_read_timeout(None);
    let _ = cloned.set_write_timeout(None);
    if !attach(header, cloned) {
        let _ = stream.write_all(b"ERR control server unavailable\n");
        let _ = stream.flush();
        aterm_uds::hangup::hang_up(stream);
        return;
    }
    if stream.write_all(b"OK\n").is_err() || stream.flush().is_err() {
        aterm_uds::hangup::hang_up(stream);
    }
}

/// Read a single `\n`-terminated UTF-8 line byte-by-byte without buffering past `\n`.
fn read_line_raw(stream: &mut impl Read, max_bytes: usize) -> std::io::Result<Option<String>> {
    let mut buf = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    buf.push(b'\n');
                    break;
                }
                if buf.len() >= max_bytes {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "line exceeds maximum length",
                    ));
                }
                buf.push(byte[0]);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn live_relay_candidates(relay_dir: &Path) -> Vec<(u32, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(relay_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if let Some(pid) = parse_relay_filename(name_str) {
            if crate::control_auth::pid_alive(pid) {
                out.push((pid, entry.path()));
            } else {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    out.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    out
}

fn has_live_window_relay(relay_dir: &Path) -> bool {
    !live_relay_candidates(relay_dir).is_empty()
}

/// What a hand-over to this Mac's windows found ([`relay_to_window`]).
enum RelayOpen {
    /// A window took it: the stream to relay on.
    Opened(CtlStream),
    /// No window runs here.
    NoWindow,
    /// A window runs and took none (refused, at capacity, or silent): why, last heard.
    Busy(String),
}

/// Hand `header` to the newest window that takes it. A window that runs and takes none is
/// [`RelayOpen::Busy`], never [`RelayOpen::NoWindow`], so a peer is never told this Mac has no
/// window (nor opened a second one) while one is up.
fn relay_to_window(relay_dir: &Path, header: &RelayHeader) -> RelayOpen {
    let wire = header.encode();
    let mut busy = None;
    for (_pid, path) in live_relay_candidates(relay_dir) {
        let mut why = |what: &str| busy = Some(what.to_owned());
        let deadline = Instant::now() + RELAY_CONNECT_TIMEOUT;
        let Ok(mut stream) = aterm_uds::connect::until(&path, deadline) else {
            why("its relay socket took no connection");
            continue;
        };
        let _ = aterm_uds::keep_out_of_children(&stream);
        if crate::control_auth::peer_check(&stream).is_err() {
            aterm_uds::hangup::hang_up(&stream);
            why("its relay socket is not this account's");
            continue;
        }
        let _ = stream.set_read_timeout(Some(RELAY_CONNECT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(RELAY_CONNECT_TIMEOUT));
        if stream.write_all(wire.as_bytes()).is_err() || stream.flush().is_err() {
            aterm_uds::hangup::hang_up(&stream);
            why("it hung up on the hand-over");
            continue;
        }
        let Some(reply) = read_line_raw(&mut stream, 256).ok().flatten() else {
            aterm_uds::hangup::hang_up(&stream);
            why("it did not answer the hand-over");
            continue;
        };
        let reply = reply.trim_end_matches(['\r', '\n']);
        if reply != "OK" {
            aterm_uds::hangup::hang_up(&stream);
            why(&aterm_log::sanitize_record(reply));
            continue;
        }
        let _ = stream.set_read_timeout(None);
        let _ = stream.set_write_timeout(None);
        return RelayOpen::Opened(stream);
    }
    busy.map_or(RelayOpen::NoWindow, RelayOpen::Busy)
}

/// [`relay_to_window`]'s stream, where a window took it.
fn open_window_relay(relay_dir: &Path, header: &RelayHeader) -> Option<CtlStream> {
    match relay_to_window(relay_dir, header) {
        RelayOpen::Opened(stream) => Some(stream),
        RelayOpen::NoWindow | RelayOpen::Busy(_) => None,
    }
}

/// The answer to a fleet peer whose request a running window here took no hand-over of.
fn answer_busy(served: aterm_net::fleet::Served, why: &str) -> std::io::Result<()> {
    served.answer(|stream| {
        stream.set_write_timeout(Some(HANDSHAKE))?;
        std::io::Write::write_all(
            stream,
            format!(
                "ERR busy: an aterm window is open on this Mac and took no fleet connection ({}); \
                 ask again\n",
                aterm_log::sanitize_record(why)
            )
            .as_bytes(),
        )
    })
}

pub(crate) fn query_window_survey_facts(
    relay_dir: &Path,
) -> Option<aterm_agent::survey_facts::SwarmSurveyFacts> {
    let mut stream = open_window_relay(
        relay_dir,
        &RelayHeader::Survey {
            content: false,
            drive: false,
            peer: None,
        },
    )?;
    let _ = stream.set_read_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RELAY_CONNECT_TIMEOUT));
    if stream.write_all(b"survey\n").is_err() || stream.flush().is_err() {
        aterm_uds::hangup::hang_up(&stream);
        return None;
    }
    let head = read_line_raw(&mut stream, 512).ok().flatten()?;
    let trimmed = head.trim_end_matches(['\r', '\n']);
    let count_str = trimmed
        .strip_prefix("OK ")?
        .split_ascii_whitespace()
        .next()?;
    let count = count_str.parse::<usize>().ok()?.min(64);
    let mut wire = String::with_capacity(head.len() + count * 128);
    wire.push_str(&head);
    for _ in 0..count {
        let Some(line) = read_line_raw(&mut stream, 4096).ok().flatten() else {
            aterm_uds::hangup::hang_up(&stream);
            return None;
        };
        wire.push_str(&line);
    }
    aterm_uds::hangup::hang_up(&stream);
    aterm_agent::survey_facts::SwarmSurveyFacts::from_survey_reply(&wire).ok()
}

/// Configuration for [`run_node_serve_with`] so integration and negative-control tests run
/// hermetically against scratch directories and ephemeral loopback listeners.
pub struct NodeServeConfig {
    /// `<state_dir>` holding `node-job.lock`, `fleet.key`, `fleet-node`, `fleet-peers.toml`, etc.
    pub state_dir: PathBuf,
    /// Socket directory holding `wf-<pid>.sock` window relays.
    pub relay_dir: PathBuf,
    /// User home directory (`~/.secrets`, etc.), if known.
    pub home_dir: Option<PathBuf>,
    /// Path to `aterm.toml`, if known.
    pub config_path: Option<PathBuf>,
    /// Vault paths for the `_alab._tcp` advert (`None` disables `dns-sd` ticks).
    pub vault_paths: Option<aterm_secrets::client::Paths>,
    /// This Mac's membership log, whose verdict admits a handshake, a join and a drive (`None`:
    /// the book's keyring alone).
    pub members: Option<Arc<aterm_agent::fleet_log::LocalLog>>,
    /// The `_alab._tcp` browse whose adverts give the heartbeat its dial hints (`None`: the
    /// process's own, started with the listener).
    pub alab_browse: Option<Arc<aterm_secrets::discover::Watch>>,
    /// Optional pre-bound TCP listener for deterministic port-0 tests.
    pub prebound_listener: Option<TcpListener>,
    /// Poll interval for key rotation, config changes, and `_alab._tcp` reconciliation.
    pub tick_every: Duration,
    /// Maximum wait for a woken window's `wf-<pid>.sock` relay on windowless `spawn`.
    pub wake_timeout: Duration,
    /// Shutdown flag cleared/set by callers or tests to stop the node job cleanly.
    pub stop: Arc<AtomicBool>,
    /// Window launcher seam for windowless `Drive` `spawn` (§7.4).
    pub launcher: Arc<dyn WindowLauncher>,
}

/// Foreground entry point for `aterm node serve` (`systems.alab.aterm-node`, C20–C23).
#[must_use]
pub fn run_node_serve() -> ExitCode {
    let Some(state_dir) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm node serve: state directory is unavailable");
        return ExitCode::FAILURE;
    };
    let relay_dir = aterm_uds::control_socket_dir().unwrap_or_else(|| state_dir.clone());
    let home_dir = aterm_types::dirs::home_dir();
    let config_path = crate::app_config::config_path();
    let isolated =
        std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some();
    let vault_paths = (!isolated)
        .then(|| aterm_secrets::client::Paths::live().ok())
        .flatten();
    let members = vault_paths
        .as_ref()
        .map(|paths| Arc::new(aterm_agent::fleet_log::LocalLog::of(paths)));
    let cfg = NodeServeConfig {
        state_dir,
        relay_dir,
        home_dir,
        config_path,
        vault_paths,
        members,
        alab_browse: None,
        prebound_listener: None,
        tick_every: retry(),
        wake_timeout: DEFAULT_WAKE_TIMEOUT,
        stop: Arc::new(AtomicBool::new(false)),
        launcher: Arc::new(LiveWindowLauncher::default()),
    };
    if !isolated {
        spawn_claims_upkeep(cfg.relay_dir.clone(), Arc::clone(&cfg.stop));
    }
    run_node_serve_with(cfg)
}

/// The account claims' look while no window runs on this Mac (docs/DESIGN-fluid-swarm-2026-10-28.md
/// F19): `look` where no window's relay is live in `relay_dir`, and whether it ran. A window looks
/// every `claims::RECHECK_S` itself, giving up each claim this Mac no longer uses; once it quits or
/// crashes, this job's Commons runtime keeps the Mac heard on the LAN, so with no look here its
/// leases would stand on every other Mac until their row's `until`, up to a day.
fn claims_upkeep(relay_dir: &Path, look: &dyn Fn()) -> bool {
    if has_live_window_relay(relay_dir) {
        return false;
    }
    look();
    true
}

/// Run [`claims_upkeep`] now and every `claims::RECHECK_S` until `stop`.
fn spawn_claims_upkeep(relay_dir: PathBuf, stop: Arc<AtomicBool>) {
    let every = Duration::from_secs(aterm_agent::harness::claims::RECHECK_S.unsigned_abs());
    let spawned = std::thread::Builder::new()
        .name("aterm-node-claims".to_owned())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                claims_upkeep(&relay_dir, &crate::harness_host::claims_look);
                sleep_interruptible(&stop, every);
            }
        });
    if let Err(err) = spawned {
        aterm_log::warn!(
            "aterm node serve: the account claims' look could not start: {err}; with no window \
             open this Mac's claims stand until their rows lapse"
        );
    }
}

/// Run the node job supervision loop with explicit paths and seams.
#[must_use]
pub fn run_node_serve_with(mut cfg: NodeServeConfig) -> ExitCode {
    crate::qos::set_self(crate::qos::Role::Background);
    let lock = match NodeJobLock::try_acquire(&cfg.state_dir) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            aterm_log::info!(
                "aterm node serve: another instance already holds {}/{}",
                cfg.state_dir.display(),
                aterm_agent::node_service::NODE_JOB_LOCK
            );
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            eprintln!(
                "aterm node serve: cannot acquire {}/{}: {err}",
                cfg.state_dir.display(),
                aterm_agent::node_service::NODE_JOB_LOCK
            );
            return ExitCode::FAILURE;
        }
    };
    let _held_lock = lock;
    // Held only while this job serves the port or is about to bind it: a window leaves the port
    // to a job that holds it, and serves it (or says why it cannot) while this job idles.
    let mut port_lock: Option<NodeJobLock> = None;
    let advertiser = Arc::new(AlabTcpAdvertiser::default());
    let browse = cfg.alab_browse.clone().or_else(|| {
        crate::network_moved::hear_alab_tcp(&[advertiser.instance()]);
        crate::network_moved::alab_tcp()
    });
    let denied_cache: Arc<Mutex<Option<Arc<[String]>>>> = Arc::new(Mutex::new(None));

    while !cfg.stop.load(Ordering::Acquire) {
        if fleet_turned_off_or_swarm_disabled(&cfg) {
            advertiser.stop();
            port_lock = None;
            sleep_interruptible(&cfg.stop, cfg.tick_every);
            continue;
        }
        let Some(wanted) = wanted_for_node(&cfg) else {
            advertiser.stop();
            port_lock = None;
            sleep_interruptible(&cfg.stop, cfg.tick_every);
            continue;
        };
        let secret = match aterm_net::fleet::read_key_file(&cfg.state_dir) {
            Ok(secret) => secret,
            Err(_) => {
                advertiser.stop();
                port_lock = None;
                sleep_interruptible(&cfg.stop, cfg.tick_every);
                continue;
            }
        };

        let (listener, is_prebound) = if let Some(prebound) = cfg.prebound_listener.take() {
            let _ = aterm_uds::keep_out_of_children(&prebound);
            if port_lock.is_none() {
                port_lock = NodeJobLock::try_acquire_port(&cfg.state_dir).ok().flatten();
            }
            (prebound, true)
        } else {
            let Ok(addr) = wanted.listen.parse::<SocketAddr>() else {
                advertiser.stop();
                port_lock = None;
                sleep_interruptible(&cfg.stop, cfg.tick_every);
                continue;
            };
            if aterm_net::loopback::check(aterm_net::loopback::Reach::Listen(addr)).is_err() {
                advertiser.stop();
                port_lock = None;
                sleep_interruptible(&cfg.stop, cfg.tick_every);
                continue;
            }
            if port_lock.is_none() {
                match NodeJobLock::try_acquire_port(&cfg.state_dir) {
                    Ok(Some(held)) => port_lock = Some(held),
                    Ok(None) | Err(_) => {
                        sleep_interruptible(&cfg.stop, cfg.tick_every);
                        continue;
                    }
                }
            }
            match TcpListener::bind(addr) {
                Ok(listener) => {
                    let _ = aterm_uds::keep_out_of_children(&listener);
                    (listener, false)
                }
                Err(err) => {
                    if !keeps_port_after(&err) {
                        port_lock = None;
                    }
                    sleep_interruptible(&cfg.stop, cfg.tick_every);
                    continue;
                }
            }
        };

        let Ok(bound_addr) = listener.local_addr() else {
            port_lock = None;
            sleep_interruptible(&cfg.stop, cfg.tick_every);
            continue;
        };
        if let Some(vault_paths) = cfg.vault_paths.as_ref() {
            advertiser.tick(
                vault_paths,
                cfg.config_path.as_deref(),
                bound_addr.port(),
                has_live_window_relay(&cfg.relay_dir),
            );
        }

        // A generation that served nothing (its watcher never started) gives the port back.
        if !serve_one_node_generation(
            &cfg,
            listener,
            is_prebound,
            bound_addr,
            &wanted,
            secret,
            &advertiser,
            &denied_cache,
            browse.as_ref(),
        ) {
            port_lock = None;
        }
    }

    advertiser.stop();
    ExitCode::SUCCESS
}

fn sleep_interruptible(stop: &AtomicBool, total: Duration) {
    let deadline = Instant::now() + total;
    let step = Duration::from_millis(25).min(total);
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(step.min(deadline - now));
    }
}

fn fleet_turned_off_or_swarm_disabled(cfg: &NodeServeConfig) -> bool {
    if aterm_agent::node_service::left_the_fleet(&cfg.state_dir) {
        return true;
    }
    if let Some(config_path) = cfg.config_path.as_ref()
        && let Ok(enabled) = aterm_secrets::swarm::enabled_at(config_path)
        && !enabled
    {
        return true;
    }
    false
}

fn read_config_text(config_path: Option<&Path>) -> Option<String> {
    let path = config_path?;
    match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Some(String::new()),
        Err(_) => None,
    }
}

/// What the node job serves where `[fleet]` names no level: every member drives this Mac, a
/// vault admin's included (docs/DESIGN-fluid-swarm-2026-10-28.md F2).
fn default_node_wanted() -> Wanted {
    Wanted {
        listen: DEFAULT_FLEET_LISTEN.to_owned(),
        allow: Some(aterm_net::fleet::DRIVE_LEVEL.to_owned()),
        drive_from: aterm_net::fleet::DriveGrant::from_word(aterm_net::fleet::ALL_MEMBERS)
            .into_iter()
            .collect(),
    }
}

/// Parse `text` (`aterm.toml`) against `fallback` (`default_node_wanted`):
/// - `None` when `text` is not valid TOML.
/// - `Some(None)` when `[swarm] enabled = false` or `[fleet] listen = ""` explicitly disables the
///   listener.
/// - `Some(Some(wanted))` merging `[fleet]` listener settings with `fallback` when `[fleet]` omits
///   `listen`/`allow`/`drive_from` (e.g. when `sync.identity` wrote only `[fleet] machine`).
fn node_table_in(text: &str, fallback: &Wanted) -> Option<Option<Wanted>> {
    #[derive(serde::Deserialize)]
    struct RawNodeConfig {
        fleet: Option<crate::app_config::FleetConfig>,
        swarm: Option<crate::app_config::SwarmConfig>,
    }
    let config = aterm_toml::from_str::<RawNodeConfig>(text).ok()?;
    if !config.swarm.and_then(|s| s.enabled).unwrap_or(true) {
        return Some(None);
    }
    let Some(fleet) = config.fleet else {
        return Some(Some(fallback.clone()));
    };
    if fleet.listen.is_none() && fleet.allow.is_none() && fleet.drive_from.is_none() {
        return Some(Some(fallback.clone()));
    }
    let listen = match fleet.listen.as_deref() {
        Some("") => return Some(None),
        Some(explicit) => explicit.to_owned(),
        None => fallback.listen.clone(),
    };
    let drive_from = fleet
        .drive_from
        .iter()
        .flatten()
        .filter_map(|word| aterm_net::fleet::DriveGrant::from_word(word))
        .collect();
    Some(Some(Wanted {
        listen,
        allow: fleet.allow,
        drive_from,
    }))
}

fn node_serving_now(running: &Wanted, fallback: &Wanted, now: Option<&str>) -> Serving {
    let scope_of = |wanted: Option<&Wanted>| {
        wanted
            .and_then(|wanted| level_of(wanted.allow.as_deref()))
            .map(|(scope, _)| scope)
    };
    let kept = scope_of(Some(running)).unwrap_or(FleetScope::Facts);
    match now.and_then(|text| node_table_in(text, fallback)) {
        None => Serving {
            scope: kept,
            grants: grants_of(Some(running)),
        },
        Some(named) => Serving {
            scope: scope_of(named.as_ref()).unwrap_or(kept),
            grants: grants_of(named.as_ref()),
        },
    }
}

/// The listener the node job starts now: `[fleet]` merged with the default policy
/// ([`node_table_in`]). An aterm.toml that cannot be read (other than missing) or does not parse
/// starts none: its `[fleet]` may narrow what this Mac grants, so the job never serves the default
/// `drive` grant in its place, as the window serves no table it cannot read.
fn wanted_for_node(cfg: &NodeServeConfig) -> Option<Wanted> {
    let fallback = default_node_wanted();
    let Some(config_path) = cfg.config_path.as_deref() else {
        return Some(fallback);
    };
    let text = read_config_text(Some(config_path))?;
    node_table_in(&text, &fallback)?.filter(|w| level_of(w.allow.as_deref()).is_some())
}

fn denied_nodes_for(
    vault_paths: Option<&aterm_secrets::client::Paths>,
    cache: &Mutex<Option<Arc<[String]>>>,
) -> Arc<[String]> {
    let observed = match vault_paths {
        Some(paths) => aterm_secrets::client::swarm_denied_nodes(paths),
        None => Ok(None),
    };
    let mut guard = cache.lock().unwrap_or_else(PoisonError::into_inner);
    update_denied_nodes_cache(&mut guard, observed)
}

struct NodeOpenEntry {
    id: u64,
    content: bool,
    driver: Option<aterm_net::fleet::DriveGrant>,
    line: CtlStream,
}

#[derive(Default)]
struct NodeOpenTable {
    next_id: AtomicU64,
    entries: Mutex<Vec<NodeOpenEntry>>,
}

struct NodeOpened {
    table: Arc<NodeOpenTable>,
    id: u64,
}

impl Drop for NodeOpened {
    fn drop(&mut self) {
        self.table
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|entry| entry.id != self.id);
    }
}

impl NodeOpenTable {
    fn admit(
        self: &Arc<Self>,
        content: bool,
        driver: Option<aterm_net::fleet::DriveGrant>,
        line: &CtlStream,
        now: impl FnOnce() -> Serving,
        holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
        denied_nodes: &[String],
    ) -> std::io::Result<Option<NodeOpened>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cloned = line.try_clone()?;
        let _ = aterm_uds::keep_out_of_children(&cloned);
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(NodeOpenEntry {
                id,
                content,
                driver: driver.clone(),
                line: cloned,
            });
        let held = NodeOpened {
            table: Arc::clone(self),
            id,
        };
        Ok(now()
            .serves(content, driver.as_ref(), holds, denied_nodes)
            .then_some(held))
    }

    fn cut_unserved(
        &self,
        serving: &Serving,
        holds: &dyn Fn(&str, aterm_net::fleet::NodeKey) -> bool,
        denied_nodes: &[String],
    ) -> usize {
        let mut cut = 0;
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|open| {
                let kept = serving.serves(open.content, open.driver.as_ref(), holds, denied_nodes);
                if !kept {
                    aterm_uds::hangup::hang_up(&open.line);
                    cut += 1;
                }
                kept
            });
        cut
    }
}

/// Whether a job whose bind of the fleet port failed with `err` keeps `node-port.lock`: only
/// while a window still lets the port go (it is in use); any other refusal means this job cannot
/// serve, so the window gets the port back.
fn keeps_port_after(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::AddrInUse
}

#[allow(clippy::too_many_arguments)]
fn serve_one_node_generation(
    cfg: &NodeServeConfig,
    listener: TcpListener,
    is_prebound: bool,
    addr: SocketAddr,
    initial_wanted: &Wanted,
    secret: aterm_net::fleet::FleetSecret,
    advertiser: &Arc<AlabTcpAdvertiser>,
    denied_cache: &Arc<Mutex<Option<Arc<[String]>>>>,
    browse: Option<&Arc<aterm_secrets::discover::Watch>>,
) -> bool {
    let running = Arc::new(AtomicBool::new(true));
    let open_table = Arc::new(NodeOpenTable::default());
    let node = aterm_net::fleet_member::read_node_identity(&cfg.state_dir)
        .ok()
        .flatten();
    if let (Some(vp), Some(me)) = (cfg.vault_paths.as_ref(), node.as_ref()) {
        let initial_log = cfg.members.as_ref().and_then(|m| m.keys());
        let _ =
            aterm_agent::fleet_log::ensure_local_node_key_in_log(vp, me, initial_log.as_deref());
    }
    let proved = node.as_ref().map(|n| (n.node().to_owned(), n.key()));
    let fallback_wanted = default_node_wanted();
    let serving_fn = {
        let running_wanted = initial_wanted.clone();
        let fallback = fallback_wanted.clone();
        let config_path = cfg.config_path.clone();
        move || -> Serving {
            let text = read_config_text(config_path.as_deref());
            node_serving_now(&running_wanted, &fallback, text.as_deref())
        }
    };

    let watch_handle = {
        let running = Arc::clone(&running);
        let open_watch = Arc::clone(&open_table);
        let stop = Arc::clone(&cfg.stop);
        let state_dir = cfg.state_dir.clone();
        let relay_dir = cfg.relay_dir.clone();
        let home_dir = cfg.home_dir.clone();
        let config_path = cfg.config_path.clone();
        let vault_paths = cfg.vault_paths.clone();
        let secret_watch = secret.clone();
        let tick_every = cfg.tick_every;
        let initial_listen = initial_wanted.listen.clone();
        let fallback = fallback_wanted;
        let serving_watch = serving_fn.clone();
        let denied_watch = Arc::clone(denied_cache);
        let adv_watch = Arc::clone(advertiser);
        let members_watch = cfg.members.clone();
        let browse_watch = browse.cloned();
        std::thread::Builder::new()
            .name("aterm-node-watch".to_owned())
            .spawn(move || {
                crate::qos::set_self(crate::qos::Role::Background);
                let mut last_facts_push = Instant::now();
                let mut last_census_refresh = Instant::now();
                let hb_in_flight = Arc::new(AtomicBool::new(false));
                let mut heartbeat =
                    HeartbeatClock::new(None, heartbeat_every(), crate::network_moved::watch());

                while running.load(Ordering::Acquire) {
                    sleep_interruptible(&stop, tick_every);
                    if stop.load(Ordering::Acquire) {
                        running.store(false, Ordering::Release);
                        return;
                    }
                    if aterm_agent::node_service::left_the_fleet(&state_dir) {
                        running.store(false, Ordering::Release);
                        return;
                    }
                    if let Some(cfg_path) = config_path.as_deref()
                        && let Ok(enabled) = aterm_secrets::swarm::enabled_at(cfg_path)
                        && !enabled
                    {
                        running.store(false, Ordering::Release);
                        return;
                    }
                    if let Some(text) = read_config_text(config_path.as_deref())
                        && let Some(now_wanted) = node_table_in(&text, &fallback)
                        && now_wanted.as_ref().map(|w| w.listen.as_str())
                            != Some(initial_listen.as_str())
                    {
                        running.store(false, Ordering::Release);
                        return;
                    }
                    let now_key = aterm_net::fleet::read_key_file(&state_dir);
                    let now_node = aterm_net::fleet_member::read_node_identity(&state_dir)
                        .ok()
                        .flatten()
                        .map(|n| (n.node().to_owned(), n.key()));
                    if now_key.as_ref().ok() != Some(&secret_watch) || now_node != proved {
                        running.store(false, Ordering::Release);
                        return;
                    }

                    let serving = serving_watch();
                    let keys = aterm_net::fleet_member::Keyring::read(&state_dir);
                    let mut log = members_watch.as_ref().and_then(|members| members.keys());
                    let denied = denied_nodes_for(vault_paths.as_ref(), &denied_watch);
                    let _ = open_watch.cut_unserved(
                        &serving,
                        &|n, key| holds_in(&keys, log.as_deref(), n, key),
                        &denied,
                    );

                    if let Some(vp) = vault_paths.as_ref() {
                        adv_watch.tick(
                            vp,
                            config_path.as_deref(),
                            addr.port(),
                            has_live_window_relay(&relay_dir),
                        );
                    }

                    if last_facts_push.elapsed() >= FACTS_HEARTBEAT_EVERY {
                        last_facts_push = Instant::now();
                        push_node_facts_heartbeat(
                            &state_dir,
                            &relay_dir,
                            home_dir.as_deref(),
                            config_path.as_deref(),
                            &secret_watch,
                        );
                    }
                    if last_census_refresh.elapsed() >= CENSUS_REFRESH_EVERY {
                        last_census_refresh = Instant::now();
                        maybe_refresh_local_census_at(&state_dir, home_dir.as_deref());
                    }
                    if let Some(woke) = heartbeat.due(Instant::now()) {
                        super::say_heartbeat_woke("node job", woke);
                        if log.as_deref().is_some_and(|keys| {
                            proved.as_ref().is_some_and(|(_, key)| !keys.admits(key))
                        }) && let Some(vp) = vault_paths.as_ref()
                            && let Some(me) =
                                aterm_net::fleet_member::read_node_identity(&state_dir)
                                    .ok()
                                    .flatten()
                            && aterm_agent::fleet_log::ensure_local_node_key_in_log(
                                vp,
                                &me,
                                log.as_deref(),
                            )
                        {
                            log = members_watch.as_ref().and_then(|members| members.keys());
                        }
                        spawn_member_heartbeat_at(
                            &state_dir,
                            addr,
                            secret_watch.clone(),
                            &hb_in_flight,
                            (denied, log, browse_watch.clone()),
                        );
                    }
                }
            })
    };
    // With no watcher, nothing would end this generation when `[fleet]`, the key or the node
    // change: serve nothing, and let the outer loop try again.
    let watch_handle = match watch_handle {
        Ok(handle) => Some(handle),
        Err(err) => {
            aterm_log::warn!(
                "aterm node serve: the watch thread did not start ({err}); not serving"
            );
            sleep_interruptible(&cfg.stop, cfg.tick_every);
            return false;
        }
    };

    let initial_serving = serving_fn();
    let sharing = (!is_prebound)
        .then(|| share_units(addr, &running))
        .flatten();
    let listening = aterm_net::fleet::Listening {
        secret: secret.clone(),
        scope: initial_serving.scope,
        node,
        serves: initial_serving.ops(),
        slots: fleet_slots(),
        per_source: aterm_net::fleet::per_source(fleet_slots()),
        lifetime: lifetime(),
    };
    let port = addr.port();
    let state_dir = cfg.state_dir.clone();
    let relay_dir = cfg.relay_dir.clone();
    let home_dir = cfg.home_dir.clone();
    let config_path = cfg.config_path.clone();
    let vault_paths = cfg.vault_paths.clone();
    let launcher = Arc::clone(&cfg.launcher);
    let wake_timeout = cfg.wake_timeout;
    let join_secret = Arc::new(secret);
    let members = cfg.members.clone();
    let members_for_gate = cfg.members.clone();
    let denied_for_gate = Arc::clone(denied_cache);
    let denied_for_serve = Arc::clone(denied_cache);
    let vault_for_gate = vault_paths.clone();
    let serving_for_offer = serving_fn.clone();
    let open_for_serve = Arc::clone(&open_table);
    let ask = relay_ask(relay_dir.clone());

    aterm_net::fleet::serve_offering_with_ask_within(
        &listener,
        (
            &listening,
            &move || {
                let s = serving_for_offer();
                (s.scope, s.ops())
            },
            &move || {
                let log = members_for_gate.as_ref().and_then(|members| members.keys());
                denied_under(
                    &denied_nodes_for(vault_for_gate.as_ref(), &denied_for_gate),
                    log.as_deref(),
                )
            },
        ),
        Some(ask),
        &running,
        HANDSHAKE,
        move |served, op| {
            let peer = served.peer_addr();
            let denied = denied_nodes_for(vault_paths.as_ref(), &denied_for_serve);
            let log = members.as_ref().and_then(|members| members.keys());
            if op == FleetOp::Join {
                let proof = served.peer_node().cloned();
                let st = state_dir.clone();
                let secret = Arc::clone(&join_secret);
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    register_member_at(
                        stream,
                        &st,
                        (peer, proof.as_ref()),
                        (addr, &secret),
                        (&denied, log.as_deref()),
                    )
                });
            }
            if op == FleetOp::Book {
                let st = state_dir.clone();
                return served.answer(|stream| {
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    share_book_at(stream, &st, peer, (&denied, log.as_deref()), addr)
                });
            }
            if op == FleetOp::Facts {
                let proof = served.peer_node().cloned();
                let st = state_dir.clone();
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    receive_pushed_facts_at(
                        stream,
                        &st,
                        proof.as_ref(),
                        peer,
                        (&denied, log.as_deref()),
                    )
                });
            }
            if op == FleetOp::MailCode {
                let proof = served.peer_node().cloned();
                let st = state_dir.clone();
                let hm = home_dir.clone();
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    serve_mail_code_request_at(stream, &st, hm.as_deref(), proof.as_ref(), peer)
                });
            }
            if op == FleetOp::Census {
                let st = state_dir.clone();
                let hm = home_dir.clone();
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    serve_census_request_at(stream, &st, hm.as_deref(), peer)
                });
            }
            if op == FleetOp::Drive {
                let serving = serving_fn();
                let (driver, grant) = match admit_driver_at(
                    &served,
                    &state_dir,
                    &serving.grants,
                    &denied,
                    log.as_deref(),
                ) {
                    Ok(admitted) => admitted,
                    Err((why, ungranted)) => {
                        return refuse_driver(served, peer, &why, ungranted);
                    }
                };
                let keys = aterm_net::fleet_member::Keyring::read(&state_dir);
                let driver_key = grant.key;
                let relayed = relay_to_window(
                    &relay_dir,
                    &RelayHeader::Drive {
                        driver: driver.clone(),
                        key: Some(driver_key),
                        peer,
                    },
                );
                if let RelayOpen::Busy(why) = relayed {
                    return answer_busy(served, &why);
                }
                if let RelayOpen::Opened(window_stream) = relayed {
                    let Some(_open) = open_for_serve.admit(
                        true,
                        Some(grant),
                        &window_stream,
                        &serving_fn,
                        &|n, key| holds_in(&keys, log.as_deref(), n, key),
                        &denied,
                    )?
                    else {
                        aterm_uds::hangup::hang_up(&window_stream);
                        return refuse_driver(
                            served,
                            peer,
                            "[fleet] took its grant back while it was admitted",
                            true,
                        );
                    };
                    return served.relay_answers(window_stream);
                }

                let (near, far) = CtlStream::pair()?;
                let _ = aterm_uds::keep_out_of_children(&near);
                let _ = aterm_uds::keep_out_of_children(&far);
                let Some(_open) = open_for_serve.admit(
                    true,
                    Some(grant),
                    &near,
                    &serving_fn,
                    &|n, key| holds_in(&keys, log.as_deref(), n, key),
                    &denied,
                )?
                else {
                    return refuse_driver(
                        served,
                        peer,
                        "[fleet] took its grant back while it was admitted",
                        true,
                    );
                };
                spawn_windowless_drive(
                    near,
                    state_dir.clone(),
                    relay_dir.clone(),
                    (driver, driver_key),
                    peer,
                    Arc::clone(&launcher),
                    wake_timeout,
                )?;
                return served.relay_answers(far);
            }

            let serving = serving_fn();
            let content =
                served.scope() == FleetScope::Content && serving.scope == FleetScope::Content;
            let drive = !serving.grants.is_empty()
                && lists_driver_at(
                    &served,
                    &state_dir,
                    &serving.grants,
                    &denied,
                    log.as_deref(),
                );
            let relayed = relay_to_window(
                &relay_dir,
                &RelayHeader::Survey {
                    content,
                    drive,
                    peer,
                },
            );
            if let RelayOpen::Busy(why) = relayed {
                return answer_busy(served, &why);
            }
            if let RelayOpen::Opened(window_stream) = relayed {
                let Some(_open) = open_for_serve.admit(
                    content,
                    None,
                    &window_stream,
                    &serving_fn,
                    &|_, _| false,
                    &[],
                )?
                else {
                    aterm_uds::hangup::hang_up(&window_stream);
                    return Err(std::io::Error::other(
                        "[fleet] lowered its level while it was admitted",
                    ));
                };
                return served.relay_answers(window_stream);
            }

            let (near, far) = CtlStream::pair()?;
            let _ = aterm_uds::keep_out_of_children(&near);
            let _ = aterm_uds::keep_out_of_children(&far);
            let Some(_open) =
                open_for_serve.admit(content, None, &near, &serving_fn, &|_, _| false, &[])?
            else {
                return Err(std::io::Error::other(
                    "[fleet] lowered its level while it was admitted",
                ));
            };
            spawn_windowless_survey(
                near,
                state_dir.clone(),
                home_dir.clone(),
                config_path.clone(),
                (content, drive),
            )?;
            served.relay_answers(far)
        },
        fleet_event,
    );

    running.store(false, Ordering::Release);
    if let Some(sharing) = sharing {
        let _ = sharing.join();
    }
    if let Some(watch) = watch_handle {
        let _ = watch.join();
    }
    if let Some(vault_paths) = cfg.vault_paths.as_ref()
        && !cfg.stop.load(Ordering::Acquire)
    {
        advertiser.tick(
            vault_paths,
            cfg.config_path.as_deref(),
            port,
            has_live_window_relay(&cfg.relay_dir),
        );
    }
    true
}

fn push_node_facts_heartbeat(
    state_dir: &Path,
    relay_dir: &Path,
    home_dir: Option<&Path>,
    config_path: Option<&Path>,
    secret: &aterm_net::fleet::FleetSecret,
) {
    let window = has_live_window_relay(relay_dir);
    let node = aterm_net::fleet_member::read_node_id(state_dir)
        .ok()
        .flatten();
    let now_s = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    let facts = if window {
        query_window_survey_facts(relay_dir).map(|mut live| {
            if live.node.is_none() {
                live.node.clone_from(&node);
            }
            live
        })
    } else {
        None
    }
    .unwrap_or_else(|| {
        aterm_agent::survey_facts::SwarmSurveyFacts::collect_at(
            state_dir,
            home_dir,
            config_path,
            0,
            window,
            node,
            now_s,
        )
    });
    let at_ms = aterm_time::unix_millis().unwrap_or(0);
    let host = aterm_uds::process::lan_host_name();
    let instances = usize::from(window);
    let payload =
        facts.to_survey_reply(std::process::id(), host.as_deref(), at_ms, instances, false);
    if let Err(why) = push_facts_to_issuer(state_dir, secret, &payload) {
        aterm_log::debug!(
            "node job: survey facts heartbeat push skipped: {}",
            aterm_log::sanitize_record(&why)
        );
    }
}

fn spawn_windowless_survey(
    mut near: CtlStream,
    state_dir: PathBuf,
    home_dir: Option<PathBuf>,
    config_path: Option<PathBuf>,
    (content, drive): (bool, bool),
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-node-survey".to_owned())
        .spawn(move || {
            let empty_store = crate::session_store::new_store();
            let most = if content {
                crate::survey::SurveyLevel::Content
            } else {
                crate::survey::SurveyLevel::Facts
            };
            loop {
                let line =
                    match read_line_raw(&mut near, aterm_types::control_verbs::MAX_REQUEST_LINE) {
                        Ok(Some(line)) => line,
                        Ok(None) => break,
                        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                            let msg = format!(
                                "ERR {}\n",
                                aterm_types::control_verbs::request_line_too_long()
                            );
                            let _ = near.write_all(msg.as_bytes());
                            break;
                        }
                        Err(_) => break,
                    };
                let trimmed = line.trim_end_matches(['\r', '\n']).trim_start();
                if trimmed.is_empty() {
                    let _ = near.write_all(b"ERR usage: empty request\n");
                    continue;
                }
                let (verb, rest) = trimmed
                    .split_once(char::is_whitespace)
                    .map_or((trimmed, ""), |(v, r)| (v, r.trim_start()));
                if verb.starts_with('@') {
                    let _ = near.write_all(
                        b"ERR denied: the fleet serves no selector; nothing is forwarded\n",
                    );
                    break;
                }
                match verb {
                    "version" => {
                        if !rest.is_empty() {
                            let _ = near.write_all(b"ERR usage: version takes no arguments\n");
                        } else {
                            let reply = crate::build_info::control_line();
                            if near.write_all(reply.as_bytes()).is_err() {
                                break;
                            }
                        }
                    }
                    "survey" => {
                        let node = aterm_net::fleet_member::read_node_id(&state_dir)
                            .ok()
                            .flatten();
                        let now_s = aterm_time::unix_secs()
                            .and_then(|s| i64::try_from(s).ok())
                            .unwrap_or(0);
                        let facts = aterm_agent::survey_facts::SwarmSurveyFacts::collect_at(
                            &state_dir,
                            home_dir.as_deref(),
                            config_path.as_deref(),
                            0,
                            false,
                            node,
                            now_s,
                        );
                        let here = crate::survey::Here {
                            host: aterm_uds::process::lan_host_name(),
                            instances: 0,
                            drive,
                            facts: Some(facts),
                        };
                        let reply = crate::survey::cmd_survey(
                            &empty_store,
                            rest,
                            most,
                            aterm_net::fleet::REPLY_MAX,
                            &here,
                        );
                        if near.write_all(reply.as_bytes()).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ =
                            near.write_all(b"ERR denied: the fleet serves only version, survey\n");
                        break;
                    }
                }
                let _ = near.flush();
            }
            aterm_uds::hangup::hang_up(&near);
        })
        .map(|_| ())
}

fn spawn_windowless_drive(
    mut near: CtlStream,
    state_dir: PathBuf,
    relay_dir: PathBuf,
    (driver, driver_key): (crate::fabric::Driver, aterm_net::fleet::NodeKey),
    peer: Option<SocketAddr>,
    launcher: Arc<dyn WindowLauncher>,
    wake_timeout: Duration,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-node-wake-drive".to_owned())
        .spawn(move || {
            let _ = near.set_read_timeout(Some(HANDSHAKE));
            let first_line =
                match read_line_raw(&mut near, aterm_types::control_verbs::MAX_REQUEST_LINE) {
                    Ok(Some(line)) => line,
                    Ok(None) => {
                        aterm_uds::hangup::hang_up(&near);
                        return;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                        let msg = format!(
                            "ERR {}\n",
                            aterm_types::control_verbs::request_line_too_long()
                        );
                        let _ = near.write_all(msg.as_bytes());
                        aterm_uds::hangup::hang_up(&near);
                        return;
                    }
                    Err(_) => {
                        aterm_uds::hangup::hang_up(&near);
                        return;
                    }
                };
            let _ = near.set_read_timeout(None);
            let trimmed = first_line.trim_end_matches(['\r', '\n']).trim_start();
            let verb = trimmed.split_ascii_whitespace().next().unwrap_or_default();
            if verb != "spawn" {
                let _ = near.write_all(b"ERR no window open on this Mac\n");
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            if !launcher.is_logged_in() {
                let _ = near.write_all(b"ERR logged out\n");
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            if let Err(why) = launcher.launch_window(&state_dir, &relay_dir) {
                let _ = near.write_all(
                    format!(
                        "ERR window launch failed: {}\n",
                        aterm_log::sanitize_record(&why)
                    )
                    .as_bytes(),
                );
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }

            let header = RelayHeader::Drive {
                driver,
                key: Some(driver_key),
                peer,
            };
            let deadline = Instant::now() + wake_timeout;
            let mut window_stream = None;
            while Instant::now() < deadline {
                if let Some(stream) = open_window_relay(&relay_dir, &header) {
                    window_stream = Some(stream);
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let Some(mut win) = window_stream else {
                let _ = near.write_all(b"ERR timed out waiting for aterm window to open\n");
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            };
            if win.write_all(first_line.as_bytes()).is_err() || win.flush().is_err() {
                aterm_uds::hangup::hang_up(&win);
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            pump_bidirectional(near, win);
        })
        .map(|_| ())
}

fn pump_bidirectional(mut left: CtlStream, mut right: CtlStream) {
    let (Ok(mut left_read), Ok(mut right_write)) = (left.try_clone(), right.try_clone()) else {
        aterm_uds::hangup::hang_up(&left);
        aterm_uds::hangup::hang_up(&right);
        return;
    };
    let _ = aterm_uds::keep_out_of_children(&left_read);
    let _ = aterm_uds::keep_out_of_children(&right_write);
    let forward = std::thread::Builder::new()
        .name("aterm-node-pump".to_owned())
        .spawn(move || {
            let _ = std::io::copy(&mut left_read, &mut right_write);
            let _ = right_write.shutdown(std::net::Shutdown::Write);
        });
    let _ = std::io::copy(&mut right, &mut left);
    let _ = left.shutdown(std::net::Shutdown::Write);
    if let Ok(handle) = forward {
        let _ = handle.join();
    }
    aterm_uds::hangup::hang_up(&left);
    aterm_uds::hangup::hang_up(&right);
}

#[cfg(test)]
#[path = "node_serve_tests.rs"]
mod tests;
