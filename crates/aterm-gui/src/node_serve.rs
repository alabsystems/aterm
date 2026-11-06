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
//! F17). It checks for and stages updates too, and with no window here it runs a staged build by
//! ending its run for launchd to start again, applying the stage at that boot
//! (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13, [`NodeUpdater`]).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::node_service::{AlabTcpAdvertiser, NodeJobLock};
use aterm_net::fleet::heard::{Heard, LiveSight, Sight};
use aterm_net::fleet::link::{
    Incoming, LinkClock, LinkConfig, LinkStream, Neighbours, NoteProven, ServeFn, SystemClock,
};
use aterm_net::fleet::{FleetOp, FleetScope, NodeIdentity};
use aterm_uds::{CtlListener, CtlStream};

use super::{
    CENSUS_REFRESH_EVERY, DENY_LIST_UNREAD, DeniedCache, FACTS_HEARTBEAT_EVERY, HeartbeatClock,
    Serving, Wanted, admit_driver_at, admit_linked_driver_at, denied_under, fleet_event,
    fleet_slots, grants_of, heartbeat_every, holds_in, judge_vault_join_at, level_of, lifetime,
    lists_driver_at, lists_linked_driver_at, maybe_refresh_local_census_at, note_refused_driver,
    push_facts_to_issuer_with_denied, receive_pushed_facts_at, refuse_driver, register_member_at,
    retry, serve_census_request_at, serve_mail_code_request_at, share_book_at, share_units,
    spawn_member_heartbeat_at, update_denied_nodes_cache,
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
    let now = Instant::now();
    let deadline = now
        .checked_add(within)
        .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
    let out =
        aterm_uds::deadline::output_until(open, deadline).map_err(|e| format!("{what}: {e}"))?;
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
        let mut window = aterm_uds::spawnfd::command(&exe);
        window.envs(crate::descriptor_limit::window_launcher_limit());
        let _child = window
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
                            let parsed = match v {
                                "0" => false,
                                "1" => true,
                                _ => return Err("invalid content flag"),
                            };
                            if content.replace(parsed).is_some() {
                                return Err("duplicate SURVEY key");
                            }
                        }
                        "drive" => {
                            let parsed = match v {
                                "0" => false,
                                "1" => true,
                                _ => return Err("invalid drive flag"),
                            };
                            if drive.replace(parsed).is_some() {
                                return Err("duplicate SURVEY key");
                            }
                        }
                        "peer" => {
                            let parsed = if v == "-" {
                                None
                            } else {
                                Some(v.parse::<SocketAddr>().map_err(|_| "invalid peer addr")?)
                            };
                            if peer.replace(parsed).is_some() {
                                return Err("duplicate SURVEY key");
                            }
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
                            if decoded.is_empty() || decoded.chars().any(char::is_control) {
                                return Err("invalid driver node");
                            }
                            if node.replace(decoded).is_some() {
                                return Err("duplicate DRIVE key");
                            }
                        }
                        "host" => {
                            let decoded = aterm_codec::pct::decode(v);
                            if decoded.is_empty() || decoded.chars().any(char::is_control) {
                                return Err("invalid driver host");
                            }
                            if host.replace(decoded).is_some() {
                                return Err("duplicate DRIVE key");
                            }
                        }
                        "key" => {
                            let parsed = aterm_net::fleet::NodeKey::from_hex(v)
                                .ok_or("invalid driver key")?;
                            if key.replace(parsed).is_some() {
                                return Err("duplicate DRIVE key");
                            }
                        }
                        "peer" => {
                            let parsed = if v == "-" {
                                None
                            } else {
                                Some(v.parse::<SocketAddr>().map_err(|_| "invalid peer addr")?)
                            };
                            if peer.replace(parsed).is_some() {
                                return Err("duplicate DRIVE key");
                            }
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
    (!pid_str.is_empty()
        && !pid_str.starts_with('0')
        && pid_str.bytes().all(|b| b.is_ascii_digit()))
    .then_some(())?;
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
            // Service repair is handled by the node before relay, where its configured
            // vault and state can be checked. A later command on this relayed connection
            // must not reinterpret a custom node context as the window's live context.
            crate::control::attach_fleet_drive(stream, peer, driver, key, false)
        }
        RelayHeader::Ask { peer } => crate::members_desk::attach_relayed(stream, peer),
    });
}

/// The node job's `ask` handler: each asking Mac's exchange relayed to the window's members
/// desk ([`RelayHeader::Ask`], [`aterm_secrets::members::relay`]). With no window, `automatic`
/// serves the node's current automatic LAN policy; without one it refuses. A running window
/// that takes no hand-over is still reported as busy.
fn relay_ask(
    relay_dir: PathBuf,
    automatic: Option<aterm_net::fleet::AskHandler>,
) -> aterm_net::fleet::AskHandler {
    Arc::new(
        move |stream: &mut dyn aterm_net::fleet::Duplex, peer: SocketAddr| {
            let mut window = match relay_to_window(&relay_dir, &RelayHeader::Ask { peer }) {
                RelayOpen::Opened(window) => window,
                RelayOpen::NoWindow => {
                    if let Some(automatic) = &automatic {
                        return automatic(stream, peer);
                    }
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
    let listener = aterm_uds::kept_out(|| CtlListener::bind(&sock_path))?;
    let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
    // An accept holds every spawn of this process back until its connection is marked
    // (`aterm_uds::kept_out`), so it never waits there: the listener is nonblocking, polled every
    // 15 ms for a stop flag, and with none to poll the wait for a connection is
    // `aterm_uds::accept`'s, which holds nothing.
    listener.set_nonblocking(true)?;

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
        let accepted = if stop.is_some() {
            aterm_uds::kept_out(|| listener.accept())
        } else {
            aterm_uds::accept(&listener)
        };
        match accepted {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
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
    let Ok(cloned) = aterm_uds::kept_out(|| stream.try_clone()) else {
        aterm_uds::hangup::hang_up(stream);
        return;
    };
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
        // Kept out of children from its creation (`aterm_uds::connect::until`).
        let Ok(mut stream) = aterm_uds::connect::until(&path, deadline) else {
            why("its relay socket took no connection");
            continue;
        };
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
        std::io::Write::write_all(stream, busy_line(why).as_bytes())
    })
}

/// What a survey is answered while a window is open and took no fleet connection.
fn busy_line(why: &str) -> String {
    format!(
        "ERR busy: an aterm window is open on this Mac and took no fleet connection ({}); ask \
         again\n",
        aterm_log::sanitize_record(why)
    )
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
    (!count_str.is_empty()
        && (count_str == "0" || !count_str.starts_with('0'))
        && count_str.bytes().all(|b| b.is_ascii_digit()))
    .then_some(())?;
    const SURVEY_FACTS_MAX_LINES: usize = 4096;
    const SURVEY_FACTS_WIRE_MAX: usize = 1 << 20;
    let count = count_str.parse::<usize>().ok()?;
    if count > SURVEY_FACTS_MAX_LINES {
        aterm_uds::hangup::hang_up(&stream);
        return None;
    }
    let mut wire = String::with_capacity(head.len() + count.min(64) * 128);
    wire.push_str(&head);
    for _ in 0..count {
        let Some(line) = read_line_raw(&mut stream, 4096).ok().flatten() else {
            aterm_uds::hangup::hang_up(&stream);
            return None;
        };
        if wire.len() + line.len() > SURVEY_FACTS_WIRE_MAX {
            aterm_uds::hangup::hang_up(&stream);
            return None;
        }
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
    /// Vault paths for the `_alab._tcp` advert, the deny list and headless automatic admission
    /// (`None`: an isolated run, which reads no vault, so its deny list names no one and `dns-sd`
    /// ticks and automatic admission are off; a live run that finds no vault does not start,
    /// [`node_vault_paths`]).
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
    /// Whether this job may use the local network now
    /// ([`aterm_types::macos_setup::lan_first_use::may_use`]): not while System State's Local
    /// Network step is still to come, which asks macOS's question first. Until then the job
    /// advertises, listens and dials nothing on the LAN.
    pub lan_may_go: fn() -> bool,
    /// This run's updater and what its boot found (`None`: a run that updates nothing, as a
    /// lab's or a test's on scratch state), [`NodeUpdates`].
    pub updates: Option<NodeUpdates>,
    /// What the mesh's neighbour links run as, and whether it is on ([`NodeMesh`]).
    pub mesh: NodeMesh,
}

/// The toolset store a `Units` stream is answered from, read for each one (`None`: none).
pub type UnitsFn = Arc<dyn Fn() -> Option<atpkg::store::Layout> + Send + Sync>;

/// What is told each generation's neighbour table as it starts ([`NodeMesh::on_neighbours`]).
pub type OnNeighbours = Arc<dyn Fn(&Arc<Neighbours>) + Send + Sync>;

/// What a node service's mesh is (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4, §11 B5, B6):
/// with it on, the service holds neighbour links ([`Neighbours`]), serves the ops only the mesh
/// carries, seeds this Mac's home-day ledger as it starts
/// (`aterm_secrets::home_lan::seed_at`), and notes each peer the mesh proves, at each handshake
/// its listener takes or its table dials and each link's re-proof, in the heard ledger of its
/// network place ([`Heard`]); with it off, its listener serves, and notes, as before.
#[derive(Clone)]
pub struct NodeMesh {
    /// The mesh is on: `aterm_types::mesh_switch::MESH`, read once into this field.
    pub on: bool,
    /// The clock each link's re-proof reads.
    pub clock: Arc<dyn LinkClock>,
    /// What the heard ledger's noting call reads of this Mac (B6): the wall clock, the default
    /// route and the interface holding an address.
    pub sight: Arc<dyn Sight>,
    /// Told each node-proven answer of a link, a re-proof once the heard ledger noted it: a
    /// test's tap.
    pub watch: NoteProven,
    /// The toolset store whose units a `Units` stream takes, read for each one (`None`: none).
    pub units: UnitsFn,
    /// Told each generation's neighbour table as it starts: where the overlay (B7a) attaches,
    /// and a test reaches the table.
    pub on_neighbours: Option<OnNeighbours>,
    /// How the job keeps the fabric's `bus.log` with the mesh on (§11 C2): the root aterm-link
    /// resolves, whether the fabric is on there, and the broker job it retires. `None` keeps no
    /// `bus.log` (a test of the links alone, or a root that does not resolve).
    pub bus: Option<aterm_mesh::logs::BusKeeperConfig>,
    /// The clock the vault pass and its timer read.
    pub pass_now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// The vault pass's pace.
    pub pass_pace: aterm_secrets::client::NodePassPace,
    /// Starts the converge pass's runner after a version the vault pass applied.
    pub pass_converge: Arc<dyn Fn() -> Option<aterm_uds::spawnfd::Child> + Send + Sync>,
}

impl NodeMesh {
    /// The node job's: the switch as this build was compiled, the system's clock, this Mac's own
    /// sight of its network, and the store atpkg is configured with.
    #[must_use]
    pub fn live() -> Self {
        Self {
            on: aterm_types::mesh_switch::MESH,
            clock: Arc::new(SystemClock::new()),
            sight: Arc::new(LiveSight),
            watch: Arc::new(|_| {}),
            units: Arc::new(|| {
                atpkg::manager_enabled()
                    .then(atpkg::store::resolve_configured)
                    .flatten()
            }),
            on_neighbours: None,
            bus: live_bus(),
            pass_now_ms: Arc::new(|| aterm_time::unix_millis().unwrap_or(0)),
            pass_pace: aterm_secrets::client::NodePassPace::LIVE,
            pass_converge: Arc::new(|| {
                let exe = std::env::current_exe().ok()?;
                aterm_secrets::converge::start(&exe, aterm_secrets::converge::SUBSCRIBER)
                    .map_err(|error| {
                        aterm_log::warn!("node: could not start the converge pass: {error}");
                    })
                    .ok()
            }),
        }
    }

    /// The mesh off, whatever the switch says: a test of today's listener.
    #[cfg(test)]
    pub(crate) fn off() -> Self {
        Self {
            on: false,
            clock: Arc::new(SystemClock::new()),
            sight: Arc::new(LiveSight),
            watch: Arc::new(|_| {}),
            units: Arc::new(|| None),
            on_neighbours: None,
            bus: None,
            pass_now_ms: Arc::new(|| aterm_time::unix_millis().unwrap_or(0)),
            pass_pace: aterm_secrets::client::NodePassPace::LIVE,
            pass_converge: Arc::new(|| None),
        }
    }
}

/// The node job's keeper of `bus.log` (§11 C2), resolved only where the mesh is on: the fabric
/// root of this login (`aterm_link::enable::Paths`), looked at every [`retry`] beside each poke.
/// A root that does not resolve keeps no `bus.log`, and says so.
fn live_bus() -> Option<aterm_mesh::logs::BusKeeperConfig> {
    if !aterm_types::mesh_switch::MESH {
        return None;
    }
    aterm_mesh::logs::BusKeeperConfig::live(retry())
        .inspect_err(|e| {
            aterm_log::warn!("aterm node serve: keeps no bus.log: its fabric root: {e}");
        })
        .ok()
}

impl NodeMesh {
    /// The keeper of `bus.log` this job starts: with the mesh on and a root resolved, `None`
    /// otherwise, or where its thread cannot start (said).
    fn keep_bus(&self) -> Option<aterm_mesh::logs::BusKeeper> {
        let cfg = self.bus.clone().filter(|_| self.on)?;
        aterm_mesh::logs::BusKeeper::start(cfg)
            .inspect_err(|e| {
                aterm_log::warn!("aterm node serve: bus.log's keeper did not start: {e}")
            })
            .ok()
    }
}

/// How often the node job's updater looks again, beside each notice its checker gives, for a
/// newer build it may end its run for: a window that quits with a stage waiting is seen within
/// it.
const UPDATE_LOOK_EVERY: Duration = Duration::from_secs(60);

/// How long after a start that found a newer build and did not run it (its boot apply refused)
/// the job may end its run for that same build again: a refusal that passes (a held apply lock,
/// a roster not yet re-confirmed) is retried within the hour, and one that stands costs one
/// restart an hour, never a loop of them.
const RETRY_LEFT_AFTER: Duration = Duration::from_secs(60 * 60);

/// The node job's updater (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13, A4). The job checks
/// and stages as a window does, whether or not a window runs. With no window here, it runs what
/// it staged by ending its run, so launchd starts it again, and that start's boot applies the
/// stage before the job binds its listener. With a window, the window applies its own. A test
/// injects its own updater, so it needs no launchd job, no bundle and no network; production's
/// is `LiveNodeUpdater` (macOS).
pub trait NodeUpdater: Send + Sync {
    /// Start the background check, which stages each newer build it finds and calls `noticed`
    /// after each stage, and once it sees a build another process staged or installed.
    fn start_checker(&self, noticed: Box<dyn Fn() + Send>);
    /// The newer build a start of this job would run: one staged and ready for a boot apply, or
    /// one another process already installed over this job's image.
    fn newer_build(&self) -> Option<u64>;
    /// `[update] auto_apply`: whether a newer build goes in by itself (a setting that cannot be
    /// read reads as no).
    fn auto_apply(&self) -> bool;
    /// The boot apply, which first counts this start against any boot trial of this build:
    /// where a stage is ready it swaps the stage in and execs the build it installed, and never
    /// returns. Otherwise it returns what it did.
    fn apply_at_boot(&self) -> String;
    /// Confirm this build's boot trial; `false` asks again.
    fn confirm_trial(&self) -> bool;
    /// End this run so it is started again, saying `why`.
    fn restart(&self, why: &str);
    /// The clock [`RETRY_LEFT_AFTER`] is read on.
    fn now(&self) -> Instant;
    /// How often the updater looks again without a notice.
    fn look_every(&self) -> Duration {
        UPDATE_LOOK_EVERY
    }
}

/// What a node run's boot found ([`apply_at_node_boot`]): when it started, and the newer build
/// it found waiting and did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeBoot {
    started: Instant,
    left: Option<u64>,
}

impl NodeBoot {
    /// A boot that found nothing waiting, at `started`.
    #[must_use]
    pub const fn nothing_at(started: Instant) -> Self {
        Self {
            started,
            left: None,
        }
    }
}

/// A node run's updater and what its boot found ([`NodeServeConfig::updates`]).
#[derive(Clone)]
pub struct NodeUpdates {
    updater: Arc<dyn NodeUpdater>,
    boot: NodeBoot,
    confirmed: Arc<AtomicBool>,
}

impl NodeUpdates {
    /// `updater` for a run whose boot found `boot`.
    #[must_use]
    pub fn new(updater: Arc<dyn NodeUpdater>, boot: NodeBoot) -> Self {
        Self {
            updater,
            boot,
            confirmed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Confirm this build's boot trial, until it is: the job asks once it has run a tick in its
    /// steady state (its listener serving, or holding the LAN as its configuration says).
    fn settle_trial(&self) {
        if !self.confirmed.load(Ordering::Acquire) && self.updater.confirm_trial() {
            self.confirmed.store(true, Ordering::Release);
        }
    }
}

/// Whether a window runs here, as the node job's updater asks it (§4.13): a `wf-<pid>.sock` in
/// `relay_dir` that takes a connect from this account. A relay file nothing listens on (its
/// window crashed, or its pid went to another process) is no window. It never asks
/// WindowServer.
fn a_window_relay_answers(relay_dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(relay_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .file_name()
            .to_str()
            .and_then(parse_relay_filename)
            .is_some()
            && relay_answers(&entry.path())
    })
}

/// Whether the relay socket at `path` takes a connect from this account; the connection is hung
/// up at once, which the relay reads as a hand-over that never came.
fn relay_answers(path: &Path) -> bool {
    let Ok(stream) = aterm_uds::connect::until(path, Instant::now() + RELAY_CONNECT_TIMEOUT) else {
        return false;
    };
    let ours = crate::control_auth::peer_check(&stream).is_ok();
    aterm_uds::hangup::hang_up(&stream);
    ours
}

/// The node job's boot (§4.13, A4), before its Commons runtime, listener or checker starts. The
/// boot apply runs unless `[update] auto_apply` is off (Install applies a stage then) or a window
/// runs here (it applies its own), whatever this job's look at the stage finds, since the apply
/// reads the stage itself. So each windowless start counts against a boot trial of its build,
/// and a build that crash-loops under launchd is reverted and the restored build re-execed
/// (`ImageReplacement::Replaceable`). A boot that applies a stage never returns.
pub(crate) fn apply_at_node_boot(updater: &dyn NodeUpdater, relay_dir: &Path) -> NodeBoot {
    let started = updater.now();
    let newer = updater.newer_build();
    let held = if !updater.auto_apply() {
        Some("[update] auto_apply is off, so Install applies it")
    } else if a_window_relay_answers(relay_dir) {
        Some("a window runs here, which applies it")
    } else {
        None
    };
    if let Some(why) = held {
        if let Some(build) = newer {
            aterm_log::info!(
                "aterm node serve: build {build} waits here and this start leaves it: {why}"
            );
        }
        return NodeBoot::nothing_at(started);
    }
    let did = updater.apply_at_boot();
    let Some(build) = newer else {
        return NodeBoot::nothing_at(started);
    };
    aterm_log::warn!(
        "aterm node serve: this start did not run build {build}, which waits here ({did}); it \
         ends its run for that build again in {} min at the soonest",
        RETRY_LEFT_AFTER.as_secs() / 60
    );
    NodeBoot {
        started,
        left: Some(build),
    }
}

/// The build a node run ends for now, so its next start runs it (§4.13): a newer build waits,
/// this run's boot did not already find it and fail to run it (unless [`RETRY_LEFT_AFTER`] has
/// passed since), `[update] auto_apply` is on, and no window runs here. Each question is asked
/// only once those before it say go, so the relays are dialled last.
fn restart_for(updater: &dyn NodeUpdater, boot: NodeBoot, relay_dir: &Path) -> Option<u64> {
    let build = updater.newer_build()?;
    if boot.left.is_some_and(|left| left >= build)
        && updater.now().saturating_duration_since(boot.started) < RETRY_LEFT_AFTER
    {
        return None;
    }
    (updater.auto_apply() && !a_window_relay_answers(relay_dir)).then_some(build)
}

/// Start `updates`' checker and its look (§4.13): at each notice from the checker, and every
/// [`NodeUpdater::look_every`], [`restart_for`] decides. Once it names a build the run ends
/// through [`NodeUpdater::restart`] and the look ends; it ends too once `stop` is set.
fn spawn_node_updates(updates: &NodeUpdates, relay_dir: PathBuf, stop: Arc<AtomicBool>) {
    let (noticed, notices) = std::sync::mpsc::sync_channel::<()>(1);
    let updater = Arc::clone(&updates.updater);
    let boot = updates.boot;
    updater.start_checker(Box::new(move || {
        let _ = noticed.try_send(());
    }));
    let spawned = std::thread::Builder::new()
        .name("aterm-node-updates".to_owned())
        .spawn(move || {
            let every = updater.look_every();
            while !stop.load(Ordering::Acquire) {
                if let Some(build) = restart_for(&*updater, boot, &relay_dir) {
                    updater.restart(&format!(
                        "build {build} waits here (staged, or installed over this image) and no \
                         window runs: this run ends so launchd starts the job again, and that \
                         start runs build {build}"
                    ));
                    return;
                }
                if notices.recv_timeout(every)
                    == Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                {
                    // The checker let its notices go (automatic checks are off): look on the
                    // clock alone, for a build another process staged or installed.
                    std::thread::sleep(every);
                }
            }
        });
    if let Err(err) = spawned {
        aterm_log::warn!(
            "aterm node serve: the updater's look could not start ({err}); this job stages, and \
             runs a newer build only once something else restarts it"
        );
    }
}

/// Production's [`NodeUpdater`]: aterm-update's checker and boot apply for this installed copy,
/// whose run ends by setting `stop` (`run_node_serve` then returns, the process exits, and
/// launchd's `KeepAlive` starts the job again).
#[cfg(target_os = "macos")]
struct LiveNodeUpdater {
    stop: Arc<AtomicBool>,
}

#[cfg(target_os = "macos")]
impl NodeUpdater for LiveNodeUpdater {
    fn start_checker(&self, noticed: Box<dyn Fn() + Send>) {
        aterm_update::spawn_background_check_with_settings(
            crate::running_build_number(),
            Arc::new(crate::app_config::update_check_settings_setting),
            None,
            Some(Box::new(move |_build, _version| noticed())),
            None,
        );
    }

    fn newer_build(&self) -> Option<u64> {
        aterm_update::newer_build_for_a_start(crate::running_build_number())
    }

    fn auto_apply(&self) -> bool {
        crate::app_config::update_check_settings_setting().is_some_and(|s| s.auto_apply)
    }

    fn apply_at_boot(&self) -> String {
        let outcome = aterm_update::apply_staged_if_ready_preserving_fds_exact(
            crate::running_build_number(),
            crate::build_info::git_commit(),
            &[],
            &[],
            false,
            aterm_update::ImageReplacement::Replaceable,
            aterm_update::TrialWitness::Headless,
        );
        format!("{outcome:?}")
    }

    fn confirm_trial(&self) -> bool {
        aterm_update::confirm_boot_health_exact(
            crate::running_build_number(),
            crate::build_info::git_commit(),
            aterm_update::TrialWitness::Headless,
        )
    }

    fn restart(&self, why: &str) {
        aterm_log::info!("aterm node serve: {why}");
        self.stop.store(true, Ordering::Release);
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Whether this `aterm node serve` runs on scratch state (a lab instance, a test, CI), which
/// reads no vault and updates nothing.
fn node_run_isolated() -> bool {
    std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some()
}

/// The socket directory holding this Mac's `wf-<pid>.sock` window relays.
fn node_relay_dir(state_dir: &Path) -> PathBuf {
    aterm_uds::control_socket_dir().unwrap_or_else(|| state_dir.to_path_buf())
}

/// This Mac's updater for a run that `stop` ends: production's on macOS, and none on scratch
/// state (a lab instance, a test, CI) or on another platform.
fn live_node_updater(stop: &Arc<AtomicBool>) -> Option<Arc<dyn NodeUpdater>> {
    if node_run_isolated() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(LiveNodeUpdater {
            stop: Arc::clone(stop),
        }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = stop;
        None
    }
}

/// [`apply_at_node_boot`] for `aterm node serve` (launchd's `systems.alab.aterm-node`), first
/// thing, with this Mac's updater ([`live_node_updater`]).
#[must_use]
pub fn apply_node_update_at_boot() -> NodeBoot {
    // A boot asks no restart: no run has started for one to end.
    let no_run = Arc::new(AtomicBool::new(false));
    match (live_node_updater(&no_run), aterm_types::dirs::state_dir()) {
        (Some(updater), Some(state_dir)) => {
            apply_at_node_boot(&*updater, &node_relay_dir(&state_dir))
        }
        _ => NodeBoot::nothing_at(Instant::now()),
    }
}

/// The vault a node job reads its deny list and membership log from: none in an isolated run (a
/// test, a lab's scratch state, CI), which reads no vault and so has a deny list that names no
/// one; otherwise this user's (`live`).
///
/// # Errors
/// A live run whose vault cannot be found (no home directory): a job that cannot read its deny
/// list must not serve as though the list named no one.
fn node_vault_paths(
    isolated: bool,
    live: impl FnOnce() -> aterm_secrets::Result<aterm_secrets::client::Paths>,
) -> Result<Option<aterm_secrets::client::Paths>, String> {
    if isolated {
        return Ok(None);
    }
    live().map(Some).map_err(|error| {
        format!("no vault to read the deny list from ({error}), so this job does not serve")
    })
}

/// Foreground entry point for `aterm node serve` (`systems.alab.aterm-node`, C20–C23), after
/// its boot ([`apply_node_update_at_boot`]) found `boot`.
#[must_use = "return or inspect the node service's exit status"]
pub fn run_node_serve(boot: NodeBoot) -> ExitCode {
    crate::descriptor_limit::raise_descriptor_limit_for_node();
    let Some(state_dir) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm node serve: state directory is unavailable");
        return ExitCode::FAILURE;
    };
    let relay_dir = node_relay_dir(&state_dir);
    let home_dir = aterm_types::dirs::home_dir();
    let config_path = crate::app_config::config_path();
    let isolated = node_run_isolated();
    let vault_paths = match node_vault_paths(isolated, aterm_secrets::client::Paths::live) {
        Ok(paths) => paths,
        Err(why) => {
            eprintln!("aterm node serve: {why}");
            return ExitCode::FAILURE;
        }
    };
    let members = vault_paths
        .as_ref()
        .map(|paths| Arc::new(aterm_agent::fleet_log::LocalLog::of(paths)));
    let stop = Arc::new(AtomicBool::new(false));
    let updates = live_node_updater(&stop).map(|updater| NodeUpdates::new(updater, boot));
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
        stop,
        launcher: Arc::new(LiveWindowLauncher::default()),
        lan_may_go: aterm_types::macos_setup::lan_first_use::may_use,
        updates,
        mesh: NodeMesh::live(),
    };
    crate::board_keeper::start(cfg.state_dir.clone());
    if !isolated {
        spawn_claims_upkeep(cfg.relay_dir.clone(), Arc::clone(&cfg.stop));
        arm_claims_off_on_quit(
            cfg.relay_dir.clone(),
            crate::harness_host::claims_off_as_the_node_job_stops,
        );
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

/// The node job's answer to a terminating signal (launchd's SIGTERM as the person logs out,
/// restarts or shuts down, or as the job is stopped): `off` where no window's relay is live in
/// `relay_dir`, and whether it ran (docs/DESIGN-fluid-swarm-2026-10-28.md F19). A window gives up
/// its own claims as the system's quit ends its run or a terminating signal ends it; with none
/// open this job's looks were the Mac's only ones, and a home claim it left would stand on every
/// Mac to its row's `until`, up to a day.
fn claims_off_on_quit(relay_dir: &Path, off: &dyn Fn()) -> bool {
    if has_live_window_relay(relay_dir) {
        return false;
    }
    off();
    true
}

/// Arm the node job's orderly quit: on SIGTERM (or SIGINT or SIGHUP) [`claims_off_on_quit`] with
/// `off` (production passes `harness_host::claims_off_as_the_node_job_stops`), then the job dies
/// of that signal as it did before (`crate::owned_endpoint::arm_orderly_quit`; a second signal
/// ends it at once).
fn arm_claims_off_on_quit(relay_dir: PathBuf, off: fn()) {
    if let Err(err) = crate::owned_endpoint::arm_orderly_quit(move |_sig| {
        claims_off_on_quit(&relay_dir, &off);
        // The quit ends the process without the keepers' drops: all three logs close first, so
        // its weak tier is flushed in full and marked closed (C2).
        aterm_mesh::logs::close_bus_at_quit();
        aterm_mesh::logs::close_mesh_at_quit();
        crate::owned_endpoint::die_of_quit_signal();
    }) {
        aterm_log::warn!(
            "aterm node serve: a terminating signal ends this job at once: its account claims' \
             release could not be armed ({err}); with no window open this Mac's claims stand \
             until their rows lapse"
        );
    }
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
#[must_use = "return or inspect the configured node service's exit status"]
pub fn run_node_serve_with(mut cfg: NodeServeConfig) -> ExitCode {
    if !cfg!(test) {
        crate::qos::set_self(crate::qos::Role::Background);
    }
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
    if !cfg.stop.load(Ordering::Acquire) {
        aterm_agent::service_repair_rpc::start_recovery_in_context(
            cfg.vault_paths.as_ref(),
            &cfg.state_dir,
        );
        // The one instance that holds the job's lock checks and stages, window or not (A4).
        if let Some(updates) = cfg.updates.as_ref() {
            spawn_node_updates(updates, cfg.relay_dir.clone(), Arc::clone(&cfg.stop));
        }
        let _ = spawn_node_vault_pass(&cfg);
    }
    // This Mac's network place is kept while no window runs too (the keeper is one per Mac:
    // whichever process holds its lock). A unit test's scratch state starts none.
    #[cfg(unix)]
    if !cfg!(test) {
        let _ = aterm_netplace::keeper::start(&cfg.state_dir);
    }
    // The first start of a build with the 7-day home condition credits each network the place
    // remembers as the ALab LAN, once, so automatic LAN admission never stops across it (B6). A
    // seed that cannot run now (an unread record, a held lock) runs at the vault's first look.
    if cfg.mesh.on
        && let Err(error) =
            aterm_secrets::home_lan::seed_at(&cfg.state_dir, cfg.mesh.sight.now_ms())
    {
        aterm_log::info!("node: the home-day seed waits: {error}");
    }
    // Held only while this job serves the port or is about to bind it: a window leaves the port
    // to a job that holds it, and serves it (or says why it cannot) while this job idles.
    let mut port_lock: Option<NodeJobLock> = None;
    let advertiser = Arc::new(AlabTcpAdvertiser::at_state(
        aterm_agent::node_service::random_alab_instance_name(),
        &cfg.state_dir,
    ));
    let mut discovery = aterm_agent::node_service::MeshDiscovery::new(
        aterm_agent::node_service::DiscoveryPolicy::process_discovery().mesh,
    );
    discovery.restore(&cfg.state_dir);
    let discovery = Arc::new(Mutex::new(discovery));
    let mut browse = cfg.alab_browse.clone().or_else(|| {
        crate::network_moved::hear_alab_tcp(&[advertiser.instance()]);
        crate::network_moved::alab_tcp()
    });
    let denied_cache: Arc<Mutex<DeniedCache>> = Arc::new(Mutex::new(DeniedCache::new()));
    let mut waits_for_step = false;
    let mut went_round = false;
    // The fabric's bus.log, hosted here with the mesh on (C2), whatever the generations do:
    // local clients reach it with no fleet key, no Local Network step and no node key. Held
    // before the mesh logs, so it closes after them.
    let bus_keeper = cfg.mesh.keep_bus();
    let mut mesh_logs = MeshHeld::default();
    mesh_logs.bus = bus_keeper.as_ref().map(|keeper| Arc::clone(keeper.slot()));

    while !cfg.stop.load(Ordering::Acquire) {
        // Round again, the job has held the LAN for a tick as its configuration says, or served
        // a generation: either is its steady state, so its boot trial is confirmed. A serving
        // generation's watcher confirms it after the listener's first tick.
        if std::mem::replace(&mut went_round, true)
            && let Some(updates) = cfg.updates.as_ref()
        {
            updates.settle_trial();
        }
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
        // Asked once a generation is due (the job is wanted and holds the fleet's key), right
        // before it advertises, listens and dials.
        if !(cfg.lan_may_go)() {
            if !std::mem::replace(&mut waits_for_step, true) {
                aterm_log::info!(
                    "aterm node serve: waits for System State's Local Network step, which asks \
                     macOS first, before it advertises, listens or dials on the local network"
                );
            }
            advertiser.stop();
            port_lock = None;
            sleep_interruptible(&cfg.stop, cfg.tick_every);
            continue;
        }

        if browse.is_none() {
            browse = discovery
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .start(advertiser.instance());
        }
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
            match aterm_uds::kept_out(|| TcpListener::bind(addr)) {
                Ok(listener) => (listener, false),
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

        // One read of the node identity for the generation and the mesh logs it serves, so the
        // two never pair one key's links with another key's logs.
        let node = aterm_net::fleet_member::read_node_identity(&cfg.state_dir);
        mesh_logs.listening(bound_addr.port());
        let mesh_now = mesh_logs.get(&cfg, &node);
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
            &discovery,
            node.ok().flatten(),
            mesh_now.as_ref(),
        ) {
            port_lock = None;
        }
    }

    advertiser.stop();
    ExitCode::SUCCESS
}

/// The vault pass (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13, B10), with the mesh on and a
/// vault this job reads: it needs no listener, so it runs while the job idles for the LAN, and it
/// posts what it signed only where the LAN may be used. The vault's subscriber keeps running
/// beside it until D4; the state lock orders the two.
fn spawn_node_vault_pass(cfg: &NodeServeConfig) -> Option<std::thread::JoinHandle<()>> {
    let (true, Some(paths)) = (cfg.mesh.on, cfg.vault_paths.clone()) else {
        return None;
    };
    let stop = Arc::clone(&cfg.stop);
    let state_dir = cfg.state_dir.clone();
    let config_path = cfg.config_path.clone();
    let lan_may_go = cfg.lan_may_go;
    let now_ms = Arc::clone(&cfg.mesh.pass_now_ms);
    let start_converge = Arc::clone(&cfg.mesh.pass_converge);
    let pace = cfg.mesh.pass_pace;
    std::thread::Builder::new()
        .name("node-vault-pass".into())
        .spawn(move || {
            aterm_secrets::client::run_node_pass(&aterm_secrets::client::NodePassHost {
                paths: &paths,
                stop: &stop,
                held: &|| sharing_held(&state_dir, config_path.as_deref()),
                may_post: &lan_may_go,
                start_converge: &*start_converge,
                now_ms: &*now_ms,
                log: &|line| aterm_log::info!("node: {line}"),
                pace,
            });
        })
        .map_err(|error| aterm_log::warn!("node: the vault pass did not start: {error}"))
        .ok()
}

/// One generation's neighbour links: the table, its re-proof timer, and the mesh overlay over it,
/// torn down in order however the generation ends. The overlay and its relays are told to stop,
/// then the links close, which ends every relay still reading one; only then are they joined.
struct GenerationLinks {
    table: Arc<Neighbours>,
    timer: Option<std::thread::JoinHandle<()>>,
    attached: Option<aterm_mesh::Attached>,
    /// The mail pushes over the links (§11 C3), told to stop first, joined after the links close.
    mail: Option<Arc<aterm_mesh::mail::MailLinks>>,
}

impl Drop for GenerationLinks {
    fn drop(&mut self) {
        if let Some(mail) = &self.mail {
            mail.stop_soon();
        }
        if let Some(attached) = &self.attached {
            attached.stop_soon();
        }
        self.table.stop();
        drop(self.mail.take());
        drop(self.attached.take());
        if let Some(timer) = self.timer.take() {
            let _ = timer.join();
        }
    }
}

/// What the node job's listener serves each connection: [`node_serving_now`] of the running table
/// `running` (`fallback` where it names none) and the aterm.toml at `config_path` as it is on disk
/// (none: the empty one), its grants those that admit now in the fleet state at `state` under the
/// vault `vault` ([`super::in_effect`], as the window's listener reads them).
fn node_serving(
    (config_path, state, vault): (
        Option<PathBuf>,
        PathBuf,
        Option<aterm_secrets::client::Paths>,
    ),
    running: Wanted,
    fallback: Wanted,
) -> impl Fn() -> Serving + Clone + Send + Sync + 'static {
    move || -> Serving {
        let text = if config_path.is_none() {
            Some(String::new())
        } else {
            read_config_text(config_path.as_deref())
        };
        let serving = node_serving_now(&running, &fallback, text.as_deref());
        Serving {
            grants: super::in_effect(serving.grants, &state, vault.as_ref()),
            ..serving
        }
    }
}

/// The mesh logs the node job hosts (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B7a): opened
/// once the mesh is on and this Mac holds its node key, kept across generations (a sealed log's
/// reopen reads it whole) while that key stays, and closed as the job ends, however it ends.
/// Presence (B7c) runs on them while they are held: this Mac's row every 30 seconds, and every
/// seat's standing read from the rows; it stops before they close.
#[derive(Default)]
struct MeshHeld {
    mesh: Option<Arc<aterm_mesh::Mesh>>,
    presence: Option<aterm_mesh::presence::Presence>,
    /// The node listener's port this generation bound, which this Mac's row names.
    port: Arc<AtomicU16>,
    /// The last reason an open failed, journaled once.
    said: Option<String>,
    /// The job's bus.log slot (C2), which each mesh opened here joins to its revocation pass.
    bus: Option<Arc<aterm_mesh::logs::BusSlot>>,
}

impl MeshHeld {
    /// The node listener's port this generation bound: this Mac's next row names it.
    fn listening(&self, port: u16) {
        self.port.store(port, Ordering::Release);
    }

    /// The mesh under `node`, the node identity this generation read: opened now where it is not
    /// yet; `None` with the mesh off, with no node key, or where it cannot open. A key that changed
    /// (the Mac rejoined) or went (it left) closes the mesh first: its records sign, and its
    /// grants name, the key it opened with. A key that could not be read keeps the mesh as it is
    /// and opens none (the generation serves no links without one). A mesh opened under a key
    /// other than `node` (the file changed between the two reads) is closed at once; the
    /// generation's watcher sees the change too, and the next generation opens it.
    fn get(
        &mut self,
        cfg: &NodeServeConfig,
        node: &std::io::Result<Option<NodeIdentity>>,
    ) -> Option<Arc<aterm_mesh::Mesh>> {
        if !cfg.mesh.on {
            return None;
        }
        let Ok(node) = node else {
            return self.mesh.clone();
        };
        let key = node.as_ref().map(|identity| *identity.key().bytes());
        if let Some(mesh) = self.mesh.as_ref()
            && key != Some(mesh.key())
        {
            aterm_log::info!("node: the node key changed, so the mesh logs reopen");
            self.presence = None;
            mesh.close();
            self.mesh = None;
        }
        if self.mesh.is_none()
            && let Some(key) = key
        {
            match open_mesh(cfg) {
                Ok(mesh) if mesh.key() == key => {
                    self.said = None;
                    if let Some(slot) = &self.bus {
                        mesh.join_bus(slot);
                    }
                    self.mesh = Some(mesh);
                }
                Ok(mesh) => mesh.close(),
                // No node key yet: the Mac has not joined; the next generation tries again.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    let why = e.to_string();
                    if self.said.as_deref() != Some(why.as_str()) {
                        aterm_log::warn!("node: the mesh logs did not open: {why}");
                        self.said = Some(why);
                    }
                }
            }
        }
        if self.presence.is_none()
            && let Some(mesh) = self.mesh.as_ref()
        {
            match aterm_mesh::presence::Presence::start(
                mesh,
                presence_config(cfg, &self.port, Arc::downgrade(mesh)),
            ) {
                Ok(presence) => self.presence = Some(presence),
                Err(e) => {
                    let why = format!("presence did not start: {e}");
                    if self.said.as_deref() != Some(why.as_str()) {
                        aterm_log::warn!("node: {why}");
                        self.said = Some(why);
                    }
                }
            }
        }
        self.mesh.clone()
    }
}

impl Drop for MeshHeld {
    fn drop(&mut self) {
        self.presence = None;
        if let Some(mesh) = self.mesh.take() {
            mesh.close();
        }
    }
}

/// What this Mac's presence row reads before each row: its build, its fleet addresses with the
/// node listener's port, the membership log it holds (the vault's store, or with no vault the
/// record store the job was given), and the topics its sessions hold, as its fabric bridge named
/// them on `mesh`'s `bus.log` (C3), and what its lab tables hold as `mesh`'s egress read them
/// (none where the mesh moves no tables).
fn presence_config(
    cfg: &NodeServeConfig,
    port: &Arc<AtomicU16>,
    mesh: std::sync::Weak<aterm_mesh::Mesh>,
) -> aterm_mesh::presence::PresenceConfig {
    use aterm_mesh::presence::{Build, Facts, PresenceConfig, fleet_addresses};
    let (vault, members, port) = (
        cfg.vault_paths.clone(),
        cfg.members.clone(),
        Arc::clone(port),
    );
    let build = Build::running();
    PresenceConfig {
        dir: cfg.state_dir.join(aterm_mesh::logs::MESH_DIR),
        facts: Arc::new(move || Facts {
            build: build.clone(),
            addresses: fleet_addresses(port.load(Ordering::Acquire)),
            membership: match (vault.as_ref(), members.as_ref()) {
                (Some(paths), _) => aterm_mesh::presence::membership_of_vault(paths),
                (None, Some(members)) => {
                    aterm_mesh::presence::membership_of_records(members.file())
                }
                (None, None) => None,
            },
            tables: mesh.upgrade().and_then(|mesh| mesh.tables_held()),
            topics: mesh
                .upgrade()
                .map(|mesh| aterm_mesh::mail::held_topics(&mesh))
                .unwrap_or_default(),
        }),
        observe: None,
    }
}

/// Open the mesh logs in the node job's state dir: the standings are the vault's, read as
/// authority reads them, or with no vault the record store the job was given; the watcher reads
/// them again when the membership log's verdict changes (`LocalLog::keys` hands back one
/// allocation until its store changes).
fn open_mesh(cfg: &NodeServeConfig) -> std::io::Result<Arc<aterm_mesh::Mesh>> {
    let vault = cfg.vault_paths.clone();
    let members = cfg.members.clone();
    let standings: aterm_mesh::logs::ReadStandings =
        Arc::new(move || match (vault.as_ref(), members.as_ref()) {
            (Some(paths), _) => aterm_mesh::Standings::read_vault(paths),
            (None, Some(members)) => aterm_mesh::Standings::read_records(members.file()),
            (None, None) => None,
        });
    let changed = cfg.members.clone().map(|members| {
        let last: Mutex<Option<Arc<aterm_net::fleet_member::MemberKeys>>> = Mutex::new(None);
        Arc::new(move || {
            let now = members.keys();
            let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
            let same = match (last.as_ref(), now.as_ref()) {
                (Some(was), Some(is)) => Arc::ptr_eq(was, is),
                (was, is) => was.is_none() && is.is_none(),
            };
            *last = now;
            !same
        }) as aterm_mesh::logs::Changed
    });
    aterm_mesh::Mesh::open(aterm_mesh::MeshConfig {
        state_dir: cfg.state_dir.clone(),
        backups: if cfg!(test) {
            aterm_uds::backup::Backups::INERT
        } else {
            aterm_uds::backup::Backups::SYSTEM
        },
        clock: Arc::clone(&cfg.mesh.clock),
        wall: Arc::new(std::time::SystemTime::now),
        standings,
        changed,
        watch_every: cfg.tick_every,
        observe: None,
        // The vault's table stores and records file, whose records the mesh moves (B7b).
        stores: cfg
            .vault_paths
            .clone()
            .zip(aterm_types::dirs::aterm_config_path())
            .map(|(paths, config)| {
                Arc::new(aterm_mesh::stores::vault::VaultStores::new(paths, config))
                    as Arc<dyn aterm_mesh::Stores>
            }),
    })
}

/// Where the overlay dials a seat: the node job's address cache, each address held to every
/// fleet dial's guards ([`dial_units_at`]'s).
fn mesh_hints(state_dir: PathBuf) -> aterm_mesh::overlay::Hints {
    Arc::new(move || {
        aterm_agent::node_service::mesh_addresses(&state_dir)
            .into_iter()
            .filter(|(_, at)| aterm_net::fleet::is_fleet_source(at.ip()))
            .filter(|(_, at)| {
                aterm_net::loopback::check(aterm_net::loopback::Reach::Dial(*at)).is_ok()
            })
            .collect()
    })
}

fn sleep_interruptible(stop: &AtomicBool, total: Duration) {
    let now = Instant::now();
    let deadline = now
        .checked_add(total)
        .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
    let step = Duration::from_millis(25).min(total);
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep(step.min(deadline - now));
    }
}

/// Whether the node job holds its LAN uses now: this Mac left the fleet, or its `aterm.toml`'s
/// `[swarm] enabled` does not read on ([`swarm_holds`]).
fn fleet_turned_off_or_swarm_disabled(cfg: &NodeServeConfig) -> bool {
    sharing_held(&cfg.state_dir, cfg.config_path.as_deref())
}

/// [`fleet_turned_off_or_swarm_disabled`] of a job's state folder and `aterm.toml`.
fn sharing_held(state_dir: &Path, config_path: Option<&Path>) -> bool {
    aterm_agent::node_service::left_the_fleet(state_dir) || swarm_holds(config_path)
}

/// Whether `[swarm] enabled` in the `aterm.toml` at `config_path` holds sharing off: unless it
/// reads on, a file that cannot be read holding it off as a clean `false` does
/// (`docs/DESIGN-alab-swarm-2026-10-27.md` §9), as every other reader of the switch takes it.
/// A job told of no file has no switch to read.
fn swarm_holds(config_path: Option<&Path>) -> bool {
    config_path.is_some_and(|path| aterm_secrets::swarm::enabled_at(path) != Ok(true))
}

fn read_config_text(config_path: Option<&Path>) -> Option<String> {
    match aterm_types::dirs::read_aterm_config_text(config_path?) {
        Ok(text) => Some(text.unwrap_or_default()),
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
        None | Some(None) => Serving {
            scope: FleetScope::Facts,
            grants: Default::default(),
        },
        Some(Some(named)) => Serving {
            scope: scope_of(Some(&named)).unwrap_or(kept),
            grants: grants_of(Some(&named)),
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

/// The swarm deny list now, read again only when one of the vault files it reads changed
/// ([`aterm_secrets::client::swarm_denied_nodes_now`]): the listener asks it for each proved
/// dialer and its hand-over, and the watch every tick, so an unchanged vault is never decrypted
/// again. `cache` keeps the last authoritative list through a mid-apply or failed read, and
/// `None` until one was read, under which every proved dialer is refused
/// ([`super::update_denied_nodes_cache`]). A job that reads no vault (`vault_paths` `None`: an
/// isolated run, [`node_vault_paths`]) has a deny list that names no one.
fn denied_nodes_for(
    vault_paths: Option<&aterm_secrets::client::Paths>,
    cache: &Mutex<DeniedCache>,
) -> Option<Arc<[String]>> {
    let observed = match vault_paths {
        Some(paths) => aterm_secrets::client::swarm_denied_nodes_now(paths),
        None => Ok(Some(Vec::new())),
    };
    let (denied, said) = update_denied_nodes_cache(
        &mut cache.lock().unwrap_or_else(PoisonError::into_inner),
        observed,
    );
    match said {
        Some(line) if denied.is_none() => aterm_log::warn!("node job {line}"),
        Some(line) => aterm_log::info!("node job {line}"),
        None => {}
    }
    denied
}

struct NodeOpenEntry {
    id: u64,
    content: bool,
    driver: Option<aterm_net::fleet::DriveGrant>,
    line: CtlStream,
    extra_line: Option<CtlStream>,
    cut: Arc<AtomicBool>,
}

#[derive(Default)]
struct NodeOpenTable {
    next_id: AtomicU64,
    entries: Mutex<Vec<NodeOpenEntry>>,
}

struct NodeOpened {
    table: Arc<NodeOpenTable>,
    id: u64,
    cut: Arc<AtomicBool>,
}

impl NodeOpened {
    fn is_cut(&self) -> bool {
        self.cut.load(Ordering::Acquire)
    }

    fn attach_line(&self, line: &CtlStream) -> bool {
        if self.is_cut() {
            return false;
        }
        let Ok(cloned) = aterm_uds::kept_out(|| line.try_clone()) else {
            return false;
        };
        let mut guard = self
            .table
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.is_cut() {
            return false;
        }
        if let Some(entry) = guard.iter_mut().find(|entry| entry.id == self.id) {
            entry.extra_line = Some(cloned);
            true
        } else {
            false
        }
    }
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
        still_serves: impl FnOnce(bool, Option<&aterm_net::fleet::DriveGrant>) -> bool,
    ) -> std::io::Result<Option<Arc<NodeOpened>>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let cloned = aterm_uds::kept_out(|| line.try_clone())?;
        let cut = Arc::new(AtomicBool::new(false));
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(NodeOpenEntry {
                id,
                content,
                driver: driver.clone(),
                line: cloned,
                extra_line: None,
                cut: Arc::clone(&cut),
            });
        let held = Arc::new(NodeOpened {
            table: Arc::clone(self),
            id,
            cut: Arc::clone(&cut),
        });
        if still_serves(content, driver.as_ref()) && !cut.load(Ordering::Acquire) {
            Ok(Some(held))
        } else {
            cut.store(true, Ordering::Release);
            Ok(None)
        }
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
                    open.cut.store(true, Ordering::Release);
                    aterm_uds::hangup::hang_up(&open.line);
                    if let Some(ref extra) = open.extra_line {
                        aterm_uds::hangup::hang_up(extra);
                    }
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
    denied_cache: &Arc<Mutex<DeniedCache>>,
    browse: Option<&Arc<aterm_secrets::discover::Watch>>,
    discovery: &Arc<Mutex<aterm_agent::node_service::MeshDiscovery>>,
    node: Option<NodeIdentity>,
    mesh_logs: Option<&Arc<aterm_mesh::Mesh>>,
) -> bool {
    // Announcements belong to this listener generation. Stop them before its
    // socket is released, including every early return while starting workers.
    struct Announcing<'a>(&'a AlabTcpAdvertiser);
    impl Drop for Announcing<'_> {
        fn drop(&mut self) {
            self.0.stop();
        }
    }
    let _announcing = Announcing(advertiser);
    let running = Arc::new(AtomicBool::new(true));
    let open_table = Arc::new(NodeOpenTable::default());
    let automatic_joins = match cfg
        .vault_paths
        .clone()
        .map(aterm_agent::vault_join::AutomaticJoins::start)
        .transpose()
    {
        Ok(joins) => joins,
        Err(error) => {
            aterm_log::warn!("node: the automatic admission worker did not start: {error}");
            advertiser.stop();
            sleep_interruptible(&cfg.stop, cfg.tick_every);
            return false;
        }
    };
    let automatic_handle = automatic_joins.as_ref().map(|joins| joins.handle());
    // Where this generation notes each peer the mesh proves (B6): its listener's handshakes, its
    // table's dials and its links' re-proofs, with the mesh on; nowhere with it off.
    let heard = cfg
        .mesh
        .on
        .then(|| Heard::under(&cfg.state_dir, Arc::clone(&cfg.mesh.sight)));
    let node = node.map(|me| me.noting(heard.clone()));
    if let (Some(vp), Some(me)) = (cfg.vault_paths.as_ref(), node.as_ref()) {
        let initial_log = cfg.members.as_ref().and_then(|m| m.keys());
        let _ =
            aterm_agent::fleet_log::ensure_local_node_key_in_log(vp, me, initial_log.as_deref());
    }
    let proved = node.as_ref().map(|n| (n.node().to_owned(), n.key()));
    let fallback_wanted = default_node_wanted();
    let serving_fn = node_serving(
        (
            cfg.config_path.clone(),
            cfg.state_dir.clone(),
            cfg.vault_paths.clone(),
        ),
        initial_wanted.clone(),
        fallback_wanted.clone(),
    );

    // This Mac's membership verdict, read for each handshake and each link's re-proof.
    let deny_now: aterm_net::fleet::DenyFn = {
        let members = cfg.members.clone();
        let vault = cfg.vault_paths.clone();
        let cache = Arc::clone(denied_cache);
        Arc::new(move || {
            let log = members.as_ref().and_then(|members| members.keys());
            denied_under(
                denied_nodes_for(vault.as_ref(), &cache).as_deref(),
                log.as_deref(),
            )
        })
    };
    let neighbours = match (node.as_ref(), cfg.mesh.on) {
        (Some(me), true) => {
            let serve = LinkServe {
                state_dir: cfg.state_dir.clone(),
                relay_dir: cfg.relay_dir.clone(),
                home_dir: cfg.home_dir.clone(),
                config_path: cfg.config_path.clone(),
                vault_paths: cfg.vault_paths.clone(),
                members: cfg.members.clone(),
                denied: Arc::clone(denied_cache),
                open: Arc::clone(&open_table),
                serving: Arc::new(serving_fn.clone()),
                launcher: Arc::clone(&cfg.launcher),
                wake_timeout: cfg.wake_timeout,
                ask: relay_ask(
                    cfg.relay_dir.clone(),
                    automatic_handle.clone().map(automatic_ask),
                ),
                units: Arc::clone(&cfg.mesh.units),
                mesh: mesh_logs.cloned(),
            }
            .serving();
            let note: NoteProven = heard.as_ref().map_or_else(
                || Arc::clone(&cfg.mesh.watch),
                |heard| heard.noting_then(Arc::clone(&cfg.mesh.watch)),
            );
            let table = Neighbours::new(
                me.clone().dialing_mesh(Some(Arc::clone(&deny_now))),
                LinkConfig {
                    verdict: Arc::clone(&deny_now),
                    serve,
                    note,
                    clock: Arc::clone(&cfg.mesh.clock),
                },
            );
            match table.spawn_timer() {
                Ok(timer) => {
                    if let Some(told) = &cfg.mesh.on_neighbours {
                        told(&table);
                    }
                    // The overlay and the relays over this generation's links (B7a).
                    let attached = mesh_logs.and_then(|mesh| {
                        mesh.attach(&table, mesh_hints(cfg.state_dir.clone()))
                            .inspect_err(|e| {
                                aterm_log::warn!("node: the mesh overlay did not start: {e}");
                            })
                            .ok()
                    });
                    // The mail pushes over them (C3), once the mesh hosts its logs.
                    let mail = mesh_logs.and_then(|mesh| {
                        aterm_mesh::mail::MailLinks::start(
                            me,
                            mesh,
                            &table,
                            mesh_hints(cfg.state_dir.clone()),
                        )
                    });
                    Some(GenerationLinks {
                        table,
                        timer: Some(timer),
                        attached,
                        mail,
                    })
                }
                Err(error) => {
                    aterm_log::warn!("node: the neighbour links' timer did not start: {error}");
                    None
                }
            }
        }
        _ => None,
    };
    let neighbours_watch = neighbours.as_ref().map(|links| Arc::clone(&links.table));
    let mail_watch = neighbours
        .as_ref()
        .and_then(|links| links.mail.as_ref().map(Arc::downgrade));

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
        let discovery = Arc::clone(discovery);
        let automatic = automatic_handle.clone();
        let updates_watch = cfg.updates.clone();
        std::thread::Builder::new()
            .name("aterm-node-watch".to_owned())
            .spawn(move || {
                if !cfg!(test) {
                    crate::qos::set_self(crate::qos::Role::Background);
                }
                let mut last_facts_push = Instant::now();
                let mut last_census_refresh = Instant::now();
                let mut last_checkouts_read = None;
                let hb_in_flight = Arc::new(AtomicBool::new(false));
                let mut heartbeat =
                    HeartbeatClock::new(None, heartbeat_every(), crate::network_moved::watch());
                let mut admission_network = crate::network_moved::watch_at_once();

                while running.load(Ordering::Acquire) {
                    sleep_interruptible(&stop, tick_every);
                    if stop.load(Ordering::Acquire) {
                        running.store(false, Ordering::Release);
                        return;
                    }
                    // The listener has answered for a tick: this build's boot trial is
                    // confirmed (A4).
                    if let Some(updates) = &updates_watch {
                        updates.settle_trial();
                    }
                    if admission_network.take_at(Instant::now()).is_some()
                        && let Some(automatic) = &automatic
                    {
                        automatic.refresh();
                    }
                    if aterm_agent::node_service::left_the_fleet(&state_dir) {
                        running.store(false, Ordering::Release);
                        return;
                    }
                    if swarm_holds(config_path.as_deref()) {
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
                    let identity = aterm_net::fleet_member::read_node_identity(&state_dir)
                        .ok()
                        .flatten();
                    let now_node = identity.as_ref().map(|n| (n.node().to_owned(), n.key()));
                    if now_key.as_ref().ok() != Some(&secret_watch) || now_node != proved {
                        running.store(false, Ordering::Release);
                        return;
                    }

                    let serving = serving_watch();
                    let keys = aterm_net::fleet_member::Keyring::read(&state_dir);
                    let mut log = members_watch.as_ref().and_then(|members| members.keys());
                    if let Some(automatic) = &automatic {
                        automatic.observe_membership(log.as_ref());
                    }
                    let denied = denied_nodes_for(vault_paths.as_ref(), &denied_watch);
                    // No deny list read yet: every open drive or content read is cut, as the
                    // handshake refuses each new connection, until one is read.
                    let unread = Serving {
                        scope: FleetScope::Facts,
                        grants: Vec::new(),
                    };
                    let _ = match denied.as_deref() {
                        Some(denied) => open_watch.cut_unserved(
                            &serving,
                            &|n, key| holds_in(&keys, log.as_deref(), n, key),
                            denied,
                        ),
                        None => open_watch.cut_unserved(&unread, &|_, _| false, &[]),
                    };
                    // A link to a key the log took out ends now, not at its next re-proof.
                    if let Some(table) = &neighbours_watch {
                        table.judge();
                    }
                    // The mail pushes route by the log as the mesh applied it (C3).
                    if let Some(mail) = mail_watch.as_ref().and_then(std::sync::Weak::upgrade) {
                        mail.pass();
                    }

                    if let (Some(watch), Some(keys)) = (browse_watch.as_ref(), log.as_deref()) {
                        let mut discovery =
                            discovery.lock().unwrap_or_else(PoisonError::into_inner);
                        discovery.refresh(
                            Instant::now(),
                            || watch.adverts(Duration::from_secs(3)),
                            |hint| {
                                aterm_agent::node_service::prove_discovery_address(
                                    hint,
                                    &secret_watch,
                                    identity.as_ref(),
                                    keys,
                                )
                            },
                        );
                        if let Err(error) = discovery.save(&state_dir) {
                            aterm_log::warn!("node discovery: could not save addresses: {error}");
                        }
                    }
                    if let Some(vp) = vault_paths.as_ref() {
                        adv_watch.tick(
                            vp,
                            config_path.as_deref(),
                            addr.port(),
                            has_live_window_relay(&relay_dir),
                        );
                    }

                    read_checkouts_when_due(
                        home_dir.as_deref(),
                        &mut last_checkouts_read,
                        FACTS_HEARTBEAT_EVERY,
                    );
                    // No deny list read yet: this Mac's facts go to no one, and its heartbeat dials no
                    // one, until one is.
                    if let Some(denied) = denied.as_deref()
                        && last_facts_push.elapsed() >= FACTS_HEARTBEAT_EVERY
                    {
                        last_facts_push = Instant::now();
                        push_node_facts_heartbeat(
                            &state_dir,
                            &relay_dir,
                            home_dir.as_deref(),
                            config_path.as_deref(),
                            &secret_watch,
                            (denied, log.as_deref()),
                        );
                    }
                    if last_census_refresh.elapsed() >= CENSUS_REFRESH_EVERY {
                        last_census_refresh = Instant::now();
                        maybe_refresh_local_census_at(&state_dir, home_dir.as_deref());
                    }
                    // A round still out takes no move: the move waits for the round after it.
                    if let Some(woke) = heartbeat
                        .due_when_free(Instant::now(), hb_in_flight.load(Ordering::Acquire))
                    {
                        super::say_heartbeat_woke("node job", woke);
                        // A record of the vault the fleet state is of, adopted where an older
                        // build left none.
                        aterm_agent::fleet_vault::keep(&state_dir, vault_paths.as_ref());
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
                        match denied {
                            Some(denied) => spawn_member_heartbeat_at(
                                &state_dir,
                                addr,
                                secret_watch.clone(),
                                (&running, &hb_in_flight),
                                (denied, log, browse_watch.clone()),
                            ),
                            None => {
                                aterm_log::debug!("node job heartbeat skipped: {DENY_LIST_UNREAD}")
                            }
                        }
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
        serves: initial_serving.ops_meshed(cfg.mesh.on),
        slots: fleet_slots(),
        per_source: aterm_net::fleet::per_source(fleet_slots()),
        lifetime: lifetime(),
    };
    let state_dir = cfg.state_dir.clone();
    let relay_dir = cfg.relay_dir.clone();
    let home_dir = cfg.home_dir.clone();
    let config_path = cfg.config_path.clone();
    let vault_paths = cfg.vault_paths.clone();
    let launcher = Arc::clone(&cfg.launcher);
    let wake_timeout = cfg.wake_timeout;
    let join_secret = Arc::new(secret);
    let members = cfg.members.clone();
    let denied_for_serve = Arc::clone(denied_cache);
    let serving_for_offer = serving_fn.clone();
    let open_for_serve = Arc::clone(&open_table);
    let serving_generation = Arc::clone(&running);
    let ask = relay_ask(relay_dir.clone(), automatic_handle.map(automatic_ask));
    let mesh = cfg.mesh.on;
    let table_for_serve = neighbours.as_ref().map(|links| Arc::clone(&links.table));
    let mesh_for_serve = mesh_logs.cloned();
    let units_for_serve = Arc::clone(&cfg.mesh.units);
    let ask_for_serve = Arc::clone(&ask);

    aterm_net::fleet::serve_offering_with_ask_within(
        &listener,
        (
            &listening,
            Arc::new(move || {
                let s = serving_for_offer();
                (s.scope, s.ops_meshed(mesh))
            }),
            deny_now,
        ),
        Some(ask),
        &running,
        HANDSHAKE,
        move |served, op| {
            let peer = served.peer_addr();
            // The handshake refused this connection while no deny list was read; one read since
            // is kept, so this is defense in depth: an unread list serves no op.
            let Some(denied) = denied_nodes_for(vault_paths.as_ref(), &denied_for_serve) else {
                if op == FleetOp::Drive {
                    return refuse_driver(served, peer, DENY_LIST_UNREAD, false);
                }
                return Err(std::io::Error::other(DENY_LIST_UNREAD));
            };
            let log = members.as_ref().and_then(|members| members.keys());
            if op == FleetOp::Neighbour {
                let Some(table) = table_for_serve.as_ref() else {
                    return Err(std::io::Error::other("this node holds no neighbour links"));
                };
                let link = served.into_link(table.me(), table.config())?;
                table.adopt(link);
                // The table rings no one: the overlay learns of the new link now.
                if let Some(mesh) = &mesh_for_serve {
                    mesh.poke();
                }
                return Ok(());
            }
            if op == FleetOp::Units {
                let Some(layout) = units_for_serve() else {
                    return Err(std::io::Error::other("this Mac shares no toolset units"));
                };
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(UNITS_WITHIN))?;
                    stream.set_write_timeout(Some(UNITS_WITHIN))?;
                    atpkg::peers::answer_unit(&UnitsOn(Mutex::new(stream)), &layout)
                });
            }
            if op == FleetOp::Desk {
                let Some(from) = peer else {
                    return Err(std::io::Error::other("a desk caller with no address"));
                };
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    ask_for_serve(stream, from)
                });
            }
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
                    share_book_at(
                        stream,
                        &st,
                        peer,
                        (&denied, log.as_deref()),
                        (addr, &serving_generation),
                    )
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
            if op == FleetOp::VaultJoin {
                let judged = judge_vault_join_at(
                    &served,
                    &join_secret,
                    (&state_dir, &serving_fn().grants),
                    &denied,
                    log.as_deref(),
                    vault_paths.as_ref(),
                );
                let vault = vault_paths.clone();
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    let ended = match (judged, vault) {
                        (Ok(judged), Some(paths)) => {
                            let from =
                                peer.map_or(std::net::Ipv4Addr::UNSPECIFIED.into(), |at| at.ip());
                            aterm_agent::vault_join::serve(stream, from, &judged, None, &paths)
                                .map_err(|error| error.to_string())
                        }
                        (Err(why), _) => Err(why),
                        (Ok(_), None) => Err("this node job follows no vault".to_owned()),
                    };
                    if let Err(why) = ended {
                        let _ = aterm_secrets::members::refuse(stream, &why);
                        aterm_log::info!("members: a vault-join from {peer:?} ended: {why}");
                    }
                    Ok(())
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
                let driver_key = grant.key;
                let check_still_serves =
                    |content: bool, grant_ref: Option<&aterm_net::fleet::DriveGrant>| -> bool {
                        let now_serving = serving_fn();
                        let now_keys = aterm_net::fleet_member::Keyring::read(&state_dir);
                        let now_log = members.as_ref().and_then(|m| m.keys());
                        // No deny list read now: nothing is served.
                        denied_nodes_for(vault_paths.as_ref(), &denied_for_serve).is_some_and(
                            |now_denied| {
                                now_serving.serves(
                                    content,
                                    grant_ref,
                                    &|n, key| holds_in(&now_keys, now_log.as_deref(), n, key),
                                    &now_denied,
                                )
                            },
                        )
                    };
                let (near, far) = aterm_uds::kept_out(CtlStream::pair)?;
                let Some(open) =
                    open_for_serve.admit(true, Some(grant), &near, check_still_serves)?
                else {
                    return refuse_driver(
                        served,
                        peer,
                        "[fleet] took its grant back while it was admitted",
                        true,
                    );
                };
                spawn_node_drive(
                    near,
                    Arc::clone(&open),
                    vault_paths.clone(),
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
                let Some(_open) = open_for_serve.admit(content, None, &window_stream, |c, d| {
                    serving_fn().serves(c, d, &|_, _| false, &[])
                })?
                else {
                    aterm_uds::hangup::hang_up(&window_stream);
                    return Err(std::io::Error::other(
                        "[fleet] lowered its level while it was admitted",
                    ));
                };
                return served.relay_answers(window_stream);
            }

            let (near, far) = aterm_uds::kept_out(CtlStream::pair)?;
            let Some(_open) = open_for_serve.admit(content, None, &near, |c, d| {
                serving_fn().serves(c, d, &|_, _| false, &[])
            })?
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
    advertiser.stop();
    drop(neighbours);
    if let Some(sharing) = sharing {
        let _ = sharing.join();
    }
    if let Some(watch) = watch_handle {
        let _ = watch.join();
    }
    drop(automatic_joins);
    true
}

/// The node job's own clock for the home's checkouts ([`crate::survey::refresh_home_repos_in`]):
/// due at its first tick and `every` after the one before (`last`), whether or not any facts go
/// out, as the window's facts heartbeat reads them. A survey of a windowless Mac then finds a read
/// within the read's own refresh period, never only a read that survey started and does not wait
/// for, which `--host any` would judge unable to say.
pub(crate) fn read_checkouts_when_due(
    home: Option<&Path>,
    last: &mut Option<Instant>,
    every: Duration,
) {
    if last.is_some_and(|at| at.elapsed() < every) {
        return;
    }
    *last = Some(Instant::now());
    if let Some(home) = home {
        crate::survey::refresh_home_repos_in(home);
    }
}

fn push_node_facts_heartbeat(
    state_dir: &Path,
    relay_dir: &Path,
    home_dir: Option<&Path>,
    config_path: Option<&Path>,
    secret: &aterm_net::fleet::FleetSecret,
    (denied_nodes, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
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
    if let Err(why) =
        push_facts_to_issuer_with_denied(state_dir, secret, (denied_nodes, log), &payload)
    {
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
                        if let Some(home) = home_dir.as_deref() {
                            crate::survey::refresh_home_repos_in(home);
                        }
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

#[allow(clippy::too_many_arguments)]
fn spawn_node_drive(
    mut near: CtlStream,
    open: Arc<NodeOpened>,
    vault_paths: Option<aterm_secrets::client::Paths>,
    state_dir: PathBuf,
    relay_dir: PathBuf,
    (driver, driver_key): (crate::fabric::Driver, aterm_net::fleet::NodeKey),
    peer: Option<SocketAddr>,
    launcher: Arc<dyn WindowLauncher>,
    wake_timeout: Duration,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-node-drive".to_owned())
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
            if open.is_cut() {
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            let trimmed = first_line.trim_end_matches(['\r', '\n']).trim_start();
            let (verb, rest) = trimmed.split_once(char::is_whitespace)
                .map_or((trimmed, ""), |(verb, rest)| (verb, rest.trim_start()));
            if verb == "vault" {
                let reply = aterm_secrets::client::admin_service::serve_at(
                    vault_paths.as_ref(), &format!("fleet:{}", driver.node), rest,
                );
                let _ = near.write_all(reply.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            // The board is this Mac's, in its state folder, read alike with or without a window
            // (the drive grant that brought this connection here is the one it needs).
            if verb == "board" {
                let reply = if rest.trim().is_empty() {
                    aterm_ctl::board::serve(&state_dir)
                } else {
                    "ERR denied: a drive reads a board whole: `board` (`board accept` is its own \
                     Mac's)\n"
                        .to_owned()
                };
                let _ = near.write_all(reply.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            // Machine upkeep stays in the node, even while windows open and close.
            // Forwarding polls to the newest window would lose their request book.
            if verb == aterm_agent::harness::remote_renew::VERB {
                let reply = aterm_agent::harness::remote_renew::serve(
                    &format!("fleet:{}", driver.node), rest,
                );
                let _ = near.write_all(reply.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            if verb == aterm_agent::service_repair_rpc::VERB {
                let reply = aterm_agent::service_repair_rpc::serve_in_context(
                    &driver.node, &driver_key.hex(), rest, vault_paths.as_ref(), &state_dir,
                );
                let _ = near.write_all(reply.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            let header = RelayHeader::Drive {
                driver,
                key: Some(driver_key),
                peer,
            };
            match relay_to_window(&relay_dir, &header) {
                RelayOpen::Opened(win) => {
                    forward_node_drive(near, win, &open, &first_line);
                    return;
                }
                RelayOpen::Busy(why) => {
                    let reply = format!(
                        "ERR busy: an aterm window is open on this Mac and took no fleet connection ({}); ask again\n",
                        aterm_log::sanitize_record(&why),
                    );
                    let _ = near.write_all(reply.as_bytes());
                    let _ = near.flush();
                    aterm_uds::hangup::hang_up(&near);
                    return;
                }
                RelayOpen::NoWindow => {}
            }
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

            let now = Instant::now();
            let deadline = now
                .checked_add(wake_timeout)
                .unwrap_or_else(|| now + Duration::from_secs(86_400 * 365));
            let mut window_stream = None;
            while Instant::now() < deadline {
                if open.is_cut() {
                    aterm_uds::hangup::hang_up(&near);
                    return;
                }
                if let Some(stream) = open_window_relay(&relay_dir, &header) {
                    window_stream = Some(stream);
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let Some(win) = window_stream else {
                let _ = near.write_all(b"ERR timed out waiting for aterm window to open\n");
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            };
            forward_node_drive(near, win, &open, &first_line);
        })
        .map(|_| ())
}

fn forward_node_drive(near: CtlStream, mut win: CtlStream, open: &NodeOpened, first_line: &str) {
    if !open.attach_line(&win)
        || win.write_all(first_line.as_bytes()).is_err()
        || win.flush().is_err()
    {
        aterm_uds::hangup::hang_up(&win);
        aterm_uds::hangup::hang_up(&near);
        return;
    }
    pump_bidirectional(near, win);
}

/// atpkg's dialer of a fleet member's toolset units over the mesh ([`atpkg::peers::set_units_dialer`],
/// `docs/DESIGN-alab-one-mesh-2026-11-05.md` §11 B5). With the mesh on, it dials the node service
/// that listens at `ip` (by the node job's address cache, [`aterm_agent::node_service::mesh_addresses`])
/// for a `Units` op of its own, as this Mac's node ([`aterm_net::fleet_member::read_node_identity`],
/// which dials the mesh only under the verdict `judge_mesh_dials` gave). `None`, so atpkg dials
/// the member's port 7487, with the mesh off, with no node or no address at `ip`, or where no
/// node service there proves a member's key.
#[must_use]
pub fn dial_units(ip: std::net::IpAddr) -> Option<Box<dyn atpkg::peers::UnitConn>> {
    if !aterm_types::mesh_switch::MESH {
        return None;
    }
    let state = aterm_types::dirs::state_dir()?;
    let me = aterm_net::fleet_member::read_node_identity(&state).ok()??;
    dial_units_at(&aterm_agent::node_service::mesh_addresses(&state), &me, ip)
}

/// How long a dial of a member's node service for units waits for its connect: a LAN peer
/// answers at once or not at all, as at port 7487.
#[cfg(not(test))]
const UNITS_CONNECT: Duration = Duration::from_millis(750);
/// Tests: the hang detector, since every node service a test dials is listening.
#[cfg(test)]
const UNITS_CONNECT: Duration = aterm_spec::wait::MUST_HAPPEN;

/// [`dial_units`] of the node service at `ip` among `addresses`, as `me`. Each address is a dial
/// hint, held to every fleet dial's guards: a fleet source ([`aterm_net::fleet::is_fleet_source`])
/// that this aterm may reach ([`aterm_net::loopback::check`]: a private aterm never dials the
/// person's fleet port), connected through the one funnel ([`aterm_uds::connect_tcp`], kept out of
/// child processes and cut by a debug partition).
fn dial_units_at(
    addresses: &[(String, SocketAddr)],
    me: &aterm_net::fleet::NodeIdentity,
    ip: std::net::IpAddr,
) -> Option<Box<dyn atpkg::peers::UnitConn>> {
    if !me.dials_mesh() {
        return None;
    }
    let ip = ip.to_canonical();
    addresses
        .iter()
        .filter(|(_, at)| at.ip().to_canonical() == ip)
        .filter(|(_, at)| aterm_net::fleet::is_fleet_source(at.ip()))
        .filter(|(_, at)| aterm_net::loopback::check(aterm_net::loopback::Reach::Dial(*at)).is_ok())
        .find_map(|(_, at)| {
            let tcp = aterm_uds::connect_tcp(at, UNITS_CONNECT).ok()?;
            let dialed = aterm_net::fleet::dial_mesh_op(tcp, me, FleetOp::Units, HANDSHAKE).ok()?;
            let stream = dialed.transport.into_stream();
            let sock = stream.sock.try_clone().ok()?;
            Some(Box::new(UnitsDialed {
                conn: Mutex::new(stream),
                sock,
            }) as Box<dyn atpkg::peers::UnitConn>)
        })
}

/// A `Units` connection this Mac dialed, as atpkg reads and writes one: the lock gives atpkg's
/// `&self` calls the `&mut` a TLS stream takes (one download reads it, on one thread), and `sock`
/// is its socket, whose bounds atpkg sets.
struct UnitsDialed<S> {
    conn: Mutex<S>,
    sock: std::net::TcpStream,
}

impl<S: Read + Write + Send> atpkg::peers::UnitConn for UnitsDialed<S> {
    fn read_unit(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.conn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .read(buf)
    }

    fn write_unit(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.conn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(buf)
    }

    fn flush_unit(&self) -> std::io::Result<()> {
        self.conn
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }

    fn set_read_timeout(&self, within: Option<Duration>) -> std::io::Result<()> {
        self.sock.set_read_timeout(within)
    }

    fn set_write_timeout(&self, within: Option<Duration>) -> std::io::Result<()> {
        self.sock.set_write_timeout(within)
    }
}

/// The automatic admission worker's desk, for a caller no window answers ([`relay_ask`]).
fn automatic_ask(
    handle: aterm_agent::vault_join::AutomaticJoinHandle,
) -> aterm_net::fleet::AskHandler {
    Arc::new(
        move |stream: &mut dyn aterm_net::fleet::Duplex, peer: SocketAddr| {
            if let Err(error) = handle.serve(stream, peer.ip()) {
                aterm_log::info!("node: an automatic join from {peer} ended: {error}");
            }
            Ok(())
        },
    )
}

/// A `Units` op's own connection as atpkg reads and writes one ([`atpkg::peers::UnitConn`]).
/// One connection is answered on its own thread, so the lock that gives atpkg's `&self` calls
/// the `&mut` an answering stream takes is never contended.
struct UnitsOn<'a, S>(Mutex<&'a mut S>);

impl<S: aterm_net::fleet::Duplex + Send> atpkg::peers::UnitConn for UnitsOn<'_, S> {
    fn read_unit(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .read(buf)
    }

    fn write_unit(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write(buf)
    }

    fn flush_unit(&self) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
    }

    // The connection's bounds were set before atpkg took it ([`UNITS_WITHIN`]).
    fn set_read_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }

    fn set_write_timeout(&self, _: Option<Duration>) -> std::io::Result<()> {
        Ok(())
    }
}

/// How long a `Units` connection waits for one read or write of its peer.
const UNITS_WITHIN: Duration = Duration::from_secs(30);

/// A neighbour link's `Units` stream as atpkg reads and writes one.
struct UnitsOver<'a>(&'a LinkStream);

impl atpkg::peers::UnitConn for UnitsOver<'_> {
    fn read_unit(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_some(buf)
    }

    fn write_unit(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write_some(buf)
    }

    fn flush_unit(&self) -> std::io::Result<()> {
        Ok(())
    }

    fn set_read_timeout(&self, within: Option<Duration>) -> std::io::Result<()> {
        self.0.set_read_timeout(within);
        Ok(())
    }

    fn set_write_timeout(&self, within: Option<Duration>) -> std::io::Result<()> {
        self.0.set_write_timeout(within);
        Ok(())
    }
}

/// What a node service serves on the streams its neighbour links carry: each op as its own
/// connection is served, judged again for each stream by the `[fleet]` table, the deny list and
/// the membership log read now.
struct LinkServe {
    state_dir: PathBuf,
    relay_dir: PathBuf,
    home_dir: Option<PathBuf>,
    config_path: Option<PathBuf>,
    vault_paths: Option<aterm_secrets::client::Paths>,
    members: Option<Arc<aterm_agent::fleet_log::LocalLog>>,
    denied: Arc<Mutex<DeniedCache>>,
    open: Arc<NodeOpenTable>,
    serving: Arc<dyn Fn() -> Serving + Send + Sync>,
    launcher: Arc<dyn WindowLauncher>,
    wake_timeout: Duration,
    ask: aterm_net::fleet::AskHandler,
    units: UnitsFn,
    /// The mesh logs whose brokers serve `Bus` (B7a); `None` where they did not open.
    mesh: Option<Arc<aterm_mesh::Mesh>>,
}

impl LinkServe {
    fn serving(self) -> ServeFn {
        let serve = Arc::new(self);
        Arc::new(move |incoming| serve.serve(incoming))
    }

    fn serve(&self, incoming: Incoming) {
        let peer = Some(incoming.peer_addr());
        let op = incoming.op();
        // No deny list read now: no stream is served, as no connection is.
        let Some(denied) = denied_nodes_for(self.vault_paths.as_ref(), &self.denied) else {
            return incoming.refuse("unread");
        };
        // `Bus` is each mesh log's broker (B7a), which judges the peer's key itself, and
        // `Inventory` the mesh's digest exchange of the tables it moves (B7b), which does too. A
        // stream for the fabric's `bus.log` (C2) is admitted as a driver is first (§4.11): the deny
        // list, the membership log, the keyring binding and `[fleet]`'s drive grant; the mesh then
        // judges the key against the seats and serves it the ring.
        if op == FleetOp::Bus {
            let Some(mesh) = &self.mesh else {
                return incoming.refuse("unserved");
            };
            if incoming.log() == Some(aterm_net::fleet::link::BusLog::Bus) {
                if mesh.bus().is_none() {
                    return incoming.refuse("unserved");
                }
                let serving = (self.serving)();
                let log = self.members.as_ref().and_then(|members| members.keys());
                if let Err((why, ungranted)) = admit_linked_driver_at(
                    incoming.peer(),
                    &self.state_dir,
                    &serving.grants,
                    &denied,
                    log.as_deref(),
                ) {
                    aterm_log::info!(
                        "aterm node serve: refused {}'s bus.log stream: {why}",
                        incoming.peer().node
                    );
                    let head = if ungranted {
                        aterm_types::control_verbs::DRIVE_UNGRANTED
                    } else {
                        "ERR denied: "
                    };
                    return incoming.refuse(&format!("{head}{why}"));
                }
            }
            return mesh.serve_bus(incoming);
        }
        if op == FleetOp::Inventory {
            return match &self.mesh {
                Some(mesh) => mesh.serve_inventory(incoming),
                None => incoming.refuse("unserved"),
            };
        }
        let serving = (self.serving)();
        if !serving.ops_meshed(true).contains(&op) {
            if op == FleetOp::Drive {
                note_refused_driver(peer, "[fleet] lets no member drive this Mac");
            }
            return incoming.refuse("op");
        }
        let log = self.members.as_ref().and_then(|members| members.keys());
        match op {
            FleetOp::Drive => self.drive(incoming, &serving, &denied, log.as_deref()),
            FleetOp::Survey => self.survey(incoming, &serving, &denied, log.as_deref()),
            FleetOp::MailCode => {
                let proof = incoming.peer().clone();
                let stream = incoming.accept();
                stream.set_read_timeout(Some(HANDSHAKE));
                stream.set_write_timeout(Some(HANDSHAKE));
                let _ = serve_mail_code_request_at(
                    &mut &stream,
                    &self.state_dir,
                    self.home_dir.as_deref(),
                    Some(&proof),
                    peer,
                );
            }
            FleetOp::Units => {
                let Some(layout) = (self.units)() else {
                    return incoming.refuse("unserved");
                };
                let stream = incoming.accept();
                let _ = atpkg::peers::answer_unit(&UnitsOver(&stream), &layout);
            }
            FleetOp::Desk => {
                let from = incoming.peer_addr();
                let stream = incoming.accept();
                stream.set_read_timeout(Some(HANDSHAKE));
                stream.set_write_timeout(Some(HANDSHAKE));
                let _ = (self.ask)(&mut &stream, from);
            }
            _ => incoming.refuse("op"),
        }
    }

    fn survey(
        &self,
        incoming: Incoming,
        serving: &Serving,
        denied: &[String],
        log: Option<&aterm_net::fleet_member::MemberKeys>,
    ) {
        let peer = Some(incoming.peer_addr());
        let content = serving.scope == FleetScope::Content;
        let drive = !serving.grants.is_empty()
            && lists_linked_driver_at(
                incoming.peer(),
                &self.state_dir,
                &serving.grants,
                denied,
                log,
            );
        let still = |c: bool, d: Option<&aterm_net::fleet::DriveGrant>| {
            (self.serving)().serves(c, d, &|_, _| false, &[])
        };
        match relay_to_window(
            &self.relay_dir,
            &RelayHeader::Survey {
                content,
                drive,
                peer,
            },
        ) {
            RelayOpen::Busy(why) => {
                let stream = incoming.accept();
                let _ = (&stream).write_all(busy_line(&why).as_bytes());
            }
            RelayOpen::Opened(window) => match self.open.admit(content, None, &window, still) {
                Ok(Some(open)) => pump_link(incoming.accept(), window, open),
                Ok(None) | Err(_) => {
                    aterm_uds::hangup::hang_up(&window);
                    incoming.refuse("level");
                }
            },
            RelayOpen::NoWindow => {
                let Ok((near, far)) = aterm_uds::kept_out(CtlStream::pair) else {
                    return incoming.refuse("busy");
                };
                let Ok(Some(open)) = self.open.admit(content, None, &near, still) else {
                    return incoming.refuse("level");
                };
                if spawn_windowless_survey(
                    near,
                    self.state_dir.clone(),
                    self.home_dir.clone(),
                    self.config_path.clone(),
                    (content, drive),
                )
                .is_err()
                {
                    return incoming.refuse("busy");
                }
                pump_link(incoming.accept(), far, open);
            }
        }
    }

    fn drive(
        &self,
        incoming: Incoming,
        serving: &Serving,
        denied: &[String],
        log: Option<&aterm_net::fleet_member::MemberKeys>,
    ) {
        let peer = Some(incoming.peer_addr());
        let (driver, grant) = match admit_linked_driver_at(
            incoming.peer(),
            &self.state_dir,
            &serving.grants,
            denied,
            log,
        ) {
            Ok(admitted) => admitted,
            Err((why, ungranted)) => {
                note_refused_driver(peer, &why);
                let head = if ungranted {
                    aterm_types::control_verbs::DRIVE_UNGRANTED
                } else {
                    "ERR denied: "
                };
                return incoming.refuse(&format!("{head}{why}"));
            }
        };
        let driver_key = grant.key;
        let still = |content: bool, grant: Option<&aterm_net::fleet::DriveGrant>| -> bool {
            let now_serving = (self.serving)();
            let now_keys = aterm_net::fleet_member::Keyring::read(&self.state_dir);
            let now_log = self.members.as_ref().and_then(|m| m.keys());
            denied_nodes_for(self.vault_paths.as_ref(), &self.denied).is_some_and(|now_denied| {
                now_serving.serves(
                    content,
                    grant,
                    &|n, key| holds_in(&now_keys, now_log.as_deref(), n, key),
                    &now_denied,
                )
            })
        };
        let Ok((near, far)) = aterm_uds::kept_out(CtlStream::pair) else {
            return incoming.refuse("busy");
        };
        let Ok(Some(open)) = self.open.admit(true, Some(grant), &near, still) else {
            return incoming
                .refuse("ERR denied: [fleet] took its grant back while it was admitted");
        };
        if spawn_node_drive(
            near,
            Arc::clone(&open),
            self.vault_paths.clone(),
            self.state_dir.clone(),
            self.relay_dir.clone(),
            (driver, driver_key),
            peer,
            Arc::clone(&self.launcher),
            self.wake_timeout,
        )
        .is_err()
        {
            return incoming.refuse("busy");
        }
        pump_link(incoming.accept(), far, open);
    }
}

/// Move bytes both ways between a link's `stream` and the local end it is served by, until
/// both directions end, holding `held` (its open-table entry) until then. Each direction's end
/// closes the other side's half, so a request and its answer both arrive whole.
fn pump_link(stream: LinkStream, local: CtlStream, held: Arc<NodeOpened>) {
    let stream = Arc::new(stream);
    let Ok(mut local_read) = aterm_uds::kept_out(|| local.try_clone()) else {
        aterm_uds::hangup::hang_up(&local);
        return;
    };
    let up = {
        let stream = Arc::clone(&stream);
        std::thread::Builder::new()
            .name("aterm-node-link-pump".to_owned())
            .spawn(move || {
                let _ = std::io::copy(&mut local_read, &mut &*stream);
                let _ = stream.finish();
            })
    };
    let mut local = local;
    if std::io::copy(&mut &*stream, &mut local).is_err() {
        // The peer reset the stream, or the link ended: the local side ends with it.
        aterm_uds::hangup::hang_up(&local);
    }
    let _ = local.shutdown(std::net::Shutdown::Write);
    if let Ok(up) = up {
        let _ = up.join();
    }
    aterm_uds::hangup::hang_up(&local);
    drop(held);
}

fn pump_bidirectional(mut left: CtlStream, mut right: CtlStream) {
    let (Ok(mut left_read), Ok(mut right_write)) = (
        aterm_uds::kept_out(|| left.try_clone()),
        aterm_uds::kept_out(|| right.try_clone()),
    ) else {
        aterm_uds::hangup::hang_up(&left);
        aterm_uds::hangup::hang_up(&right);
        return;
    };
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
