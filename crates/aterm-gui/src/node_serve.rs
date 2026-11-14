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

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_agent::node_service::{AlabTcpAdvertiser, NodeJobLock};
use aterm_metrics::row::{ActivityRow, NeedsRow};
use aterm_net::fleet::heard::{Heard, LiveSight, Sight};
use aterm_net::fleet::link::{
    Incoming, LinkClock, LinkConfig, LinkStream, Neighbours, NoteProven, ServeFn, SystemClock,
};
use aterm_net::fleet::{FleetOp, FleetScope, NodeIdentity};
use aterm_session::SessionId;
use aterm_types::MutexExt as _;
use aterm_types::fleet_access::{DriveKind, FleetAccess};
use aterm_uds::stop::Stop;
use aterm_uds::{CtlListener, CtlStream};

#[path = "node_serve/health.rs"]
mod health;
#[cfg(target_os = "linux")]
#[path = "node_serve/window_start.rs"]
mod window_start;

use super::{
    CENSUS_REFRESH_EVERY, DENY_LIST_UNREAD, DeniedCache, FACTS_HEARTBEAT_EVERY, HeartbeatClock,
    Serving, Wanted, admit_driver_at, admit_linked_driver_at, fleet_event, fleet_slots, grants_of,
    heartbeat_every, holds_in, judge_vault_join_at, level_of, lifetime, lists_driver_at,
    lists_linked_driver_at, maybe_refresh_local_census_at, note_refused_driver,
    push_facts_to_issuer_with_denied, receive_pushed_facts_at, refuse_driver, register_member_at,
    retry, serve_census_request_at, serve_mail_code_request_at, share_book_at, share_units,
    spawn_member_heartbeat_at, update_denied_nodes_cache,
};

const RELAY_PREFIX: &str = "wf-";
const RELAY_SUFFIX: &str = ".sock";
/// A relay header's bound, and a window's answer to one: a drive's carries the sessions its line
/// names (an `await task` may name many), so the line's own bound, its requester line
/// percent-encoded (three bytes a byte at most), and the rest of the header.
const RELAY_HEADER_MAX: usize = aterm_types::control_verbs::MAX_REQUEST_LINE
    + 3 * aterm_types::control_verbs::REQUESTER_LINE_MAX
    + 1024;
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
pub(crate) const HANDSHAKE: Duration = aterm_net::fleet::HANDSHAKE_WITHIN;
#[cfg(test)]
pub(crate) const HANDSHAKE: Duration = aterm_spec::wait::MUST_HAPPEN;
const DEFAULT_WAKE_TIMEOUT: Duration = Duration::from_secs(15);
const LAUNCH_COOLDOWN: Duration = Duration::from_secs(5);
/// How long `open -g -a` may take to hand the app to Launch Services before the spawn reports it
/// stuck, so a wedged `open` never holds the fleet connection that asked for a window.
#[cfg(target_os = "macos")]
const OPEN_WITHIN: Duration = Duration::from_secs(30);
const DEFAULT_FLEET_LISTEN: &str = "0.0.0.0:7100";

static WINDOW_RELAY_STARTED: AtomicBool = AtomicBool::new(false);
static WINDOW_RELAY_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The `applies` [`start_window_fleet_relay`] was given, which its thread reads as it starts.
static WINDOW_RELAY_APPLIES: AtomicBool = AtomicBool::new(false);

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

    /// Whether the window whose relay is `wf-<pid>.sock` runs this node service's own app, the
    /// one [`Self::launch_window`] opens: a drive that names no session goes to it first
    /// ([`WindowRelays::in_order`]).
    fn runs_this_app(&self, pid: u32) -> bool;
}

/// Production [`WindowLauncher`] checking `/dev/console` ownership on macOS (or `DISPLAY` /
/// `WAYLAND_DISPLAY` elsewhere) and running `open -g -a <bundle>` (or `aterm --window`, which on
/// Linux lives in a systemd scope of its own where a systemd user manager runs the node).
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

    /// The kernel's word on the image `pid` loaded: the file now at this node service's own
    /// executable path, which [`Self::launch_window`] opens (the installed app, whose launchd job
    /// runs this service). A dev build's window, or one an update left on the image it replaced,
    /// is not; nor is a window whose image cannot be read.
    fn runs_this_app(&self, pid: u32) -> bool {
        let (Ok(exe), Ok(pid)) = (std::env::current_exe(), i32::try_from(pid)) else {
            return false;
        };
        aterm_uds::process::image_matches_file(pid, &exe) == Some(true)
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

        spawn_window(&exe)
    }
}

/// Start `exe --window` where Launch Services does not take it, in a systemd scope of its own
/// when a systemd user manager runs the node ([`window_start`]), so a stop or restart of the
/// node's unit never ends the window or its shells; else as the node's own child.
#[cfg(target_os = "linux")]
fn spawn_window(exe: &Path) -> Result<(), String> {
    window_start::start(exe, &[std::ffi::OsStr::new("--window")]).map(drop)
}

/// Start `exe --window` as the node's own child where Launch Services does not take it.
#[cfg(not(target_os = "linux"))]
fn spawn_window(exe: &Path) -> Result<(), String> {
    use aterm_uds::spawnfd::Stdio;
    let mut window = aterm_uds::spawnfd::command(exe);
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
        /// The sessions the drive's line names ([`crate::control::drive_line_sessions`]): a
        /// window takes it only if it hosts or keeps every one, and answers `ELSEWHERE` if not
        /// ([`handle_relay_accept`]). Empty: any window takes it.
        sids: Vec<SessionId>,
        /// Who on the driving Mac asked for it, as the drive's first line named them
        /// ([`aterm_types::control_verbs::Requester`]): the node job read that line and hands it
        /// on here, in the header a window of another build refuses as a whole (`unknown DRIVE
        /// key`, `missing requester`), never as a line ahead of the drive's requests that such
        /// a window would serve as one.
        requester: aterm_types::control_verbs::Requester,
    },
    /// A Mac with no fleet secret asking to join ALab from `peer`: the window's members desk
    /// serves it once the node job writes `go` (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10).
    Ask { peer: SocketAddr },
    /// The node job asking whether this process applies this Mac's staged updates now (§4.13): it
    /// answers `yes` or `no` ([`WindowApplies::answer`]) and takes nothing over.
    Applies,
    /// The node job's read of the window's needs (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
    /// §4.14): once the node job writes `go`, the window writes its needs row as one frame
    /// ([`write_needs_frame`]) and another each time it changes, until the node job hangs up.
    Needs,
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
            Self::Drive {
                driver,
                key,
                peer,
                sids,
                requester,
            } => {
                let mut line = format!(
                    "DRIVE node={} host={}",
                    aterm_codec::pct::encode(&driver.node),
                    aterm_codec::pct::encode(&driver.host),
                );
                if let Some(key) = key {
                    line.push_str(&format!(" key={}", key.hex()));
                }
                line.push_str(&format!(
                    " peer={}",
                    peer.map_or_else(|| "-".to_owned(), |p| p.to_string())
                ));
                if !sids.is_empty() {
                    line.push_str(&format!(" sids={}", sid_list(sids)));
                }
                let named = requester.line();
                line.push_str(" requester=");
                aterm_codec::pct::encode_into(&mut line, named.trim_end_matches('\n'));
                line.push('\n');
                line
            }
            Self::Ask { peer } => format!("ASK peer={peer}\n"),
            Self::Applies => "APPLIES\n".to_owned(),
            Self::Needs => "NEEDS\n".to_owned(),
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
                let mut sids = None;
                let mut requester = None;
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
                        "sids" => {
                            let parsed = parse_sid_list(v)
                                .filter(|sids| !sids.is_empty())
                                .ok_or("invalid drive sessions")?;
                            if sids.replace(parsed).is_some() {
                                return Err("duplicate DRIVE key");
                            }
                        }
                        "requester" => {
                            let parsed = aterm_types::control_verbs::Requester::parse(
                                &aterm_codec::pct::decode(v),
                            )?;
                            if requester.replace(parsed).is_some() {
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
                    sids: sids.unwrap_or_default(),
                    requester: requester.ok_or("missing requester")?,
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
            "APPLIES" => match words.next() {
                None => Ok(Self::Applies),
                Some(_) => Err("malformed APPLIES header"),
            },
            "NEEDS" => match words.next() {
                None => Ok(Self::Needs),
                Some(_) => Err("malformed NEEDS header"),
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

/// Whether this process may apply this Mac's staged updates at all, read once for the process: a
/// window on a copy the updater may replace. A headless process starts no checker and reconciles
/// nothing, and a dev bundle, a bare build or a mounted or translocated copy is not installable, so
/// the node job must not wait on one. Off macOS nothing asks (`LiveNodeUpdater`). Where it may,
/// [`WindowApplies::answer`] says whether it does now.
pub(crate) const fn window_applies_updates(headless: bool, installable: Option<bool>) -> bool {
    !headless && !matches!(installable, Some(false))
}

/// How long the node job defers a staged build to a window whose automatic lane may still land it
/// (§4.13): as long as a newer build may wait on this machine before it is overdue. That is many
/// times the automatic lane's own bound (asserted at least four times), so a window that lands its
/// update by itself always has the time. Past it, the window lets the job install the build on
/// disk and switches to it as an installed activation. Its own switch still waits for unsaved
/// work and never loses a session.
pub(crate) const WINDOW_HOLDS_AN_UPDATE_WITHIN: Duration =
    Duration::from_secs(aterm_update::PENDING_UPDATE_OVERDUE_SECS);

/// How far apart the node job's asks may come and still be one run of deferring
/// ([`WindowHold::ask`]). Its looks come every [`UPDATE_LOOK_EVERY`], and a boot that found the
/// build and failed to run it pauses them for [`RETRY_LEFT_AFTER`]; neither pause restarts the
/// window's bound. A longer silence means the job stopped deferring (nothing newer, or no job).
const ASKS_UNBROKEN_WITHIN: Duration =
    Duration::from_secs(2 * aterm_update::PENDING_UPDATE_OVERDUE_SECS);

const _: () = assert!(
    WINDOW_HOLDS_AN_UPDATE_WITHIN.as_secs()
        >= 4 * (crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
            + crate::native_update_auto_intent::SWITCH_ALLOWANCE.as_secs())
        && ASKS_UNBROKEN_WITHIN.as_secs()
            > RETRY_LEFT_AFTER.as_secs() + UPDATE_LOOK_EVERY.as_secs()
        && ASKS_UNBROKEN_WITHIN.as_secs() > WINDOW_HOLDS_AN_UPDATE_WITHIN.as_secs(),
    "a window that lands its update by itself must have its own lane's bound several times over \
     before the node job installs it, and neither the job's look interval nor its hour's pause \
     after a failed boot may restart the window's bound, or a hold renews itself for good"
);

/// What a window's update lane says of this Mac's staged build, as its event loop last saw it
/// ([`WindowApplies::note_lane`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum WindowUpdateLane {
    /// Its automatic lane may land the build by itself: its intent is armed, waiting for a quiet
    /// moment or for unsaved work, or nothing is staged in its view. It holds the node job for
    /// [`WINDOW_HOLDS_AN_UPDATE_WITHIN`].
    Lands = 0,
    /// A person's setting holds updates here (`[update] auto_apply = false` in the window's
    /// config, or the relaunch-nudge seam). It holds the job with no bound. The window owns its
    /// boot trial whatever the setting, and the job, which reads the same setting itself, would
    /// install nothing either.
    PersonHolds = 1,
    /// Its automatic lane will not land the staged download by itself now. The in-session
    /// handoff cannot run here, the lane stood down for that build, or the kept image installs it
    /// only at the next launch. It holds nothing: the job installs the build on disk through the
    /// same verification, under a boot trial of its own, unless those exact bytes are ones a
    /// window here saw fail to start, which no windowless start installs (`aterm_update`'s
    /// window-start failure record). A window that can hand off then switches to it as an
    /// installed activation, and one that cannot runs it at its next launch.
    Leaves = 2,
    /// Its staged build is already installed in its place, so the job has nothing to install; what
    /// is left is this window's switch to it. That boot trial waits on this window's launch, so a
    /// start of the job on those bytes would not count against it (`aterm_update`'s window wait).
    /// A job crash-looping on them would then go unreverted for that wait. It holds the job,
    /// as every window did, until it switches, its lane installs something newer, or it exits.
    Activates = 3,
}

impl WindowUpdateLane {
    /// The lane of a window whose person's setting `holds` updates, and whose staged build, if
    /// any, has `posture` ([`crate::App::apply_posture_for`]) and is an installed activation
    /// where `activation`.
    pub(crate) const fn of(
        holds: bool,
        activation: bool,
        posture: Option<crate::update_words::ApplyPosture>,
    ) -> Self {
        use crate::update_words::ApplyPosture as P;
        if holds {
            return Self::PersonHolds;
        }
        if activation && posture.is_some() {
            return Self::Activates;
        }
        match posture {
            None | Some(P::Automatic) => Self::Lands,
            Some(
                P::ManualByConfig
                | P::VetoedByEnv { .. }
                | P::HandoffDisabled { veto: Some(_), .. },
            ) => Self::PersonHolds,
            Some(
                P::HandoffDisabled { veto: None, .. }
                | P::ManualOnlyLatched { .. }
                | P::TrialSpent
                | P::AtNextLaunch,
            ) => Self::Leaves,
        }
    }

    const fn from_u8(lane: u8) -> Self {
        match lane {
            1 => Self::PersonHolds,
            2 => Self::Leaves,
            3 => Self::Activates,
            _ => Self::Lands,
        }
    }
}

/// How long the node job has deferred a build to this window: the run of its asks this window held,
/// for the staged build its event loop last named.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct WindowHold {
    /// The staged build the run is for, `0` where none is staged in the window's view.
    staged: u64,
    /// The first ask of the run, and the latest.
    since: Option<(Instant, Instant)>,
    /// Whether the log has said this run outlasted the bound.
    told: bool,
}

/// What [`WindowHold::ask`] answers and whether the hold has just run out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HoldAnswer {
    pub(crate) holds: bool,
    /// The hold ran out at this ask, after holding the job this long, and nothing said so yet.
    pub(crate) ran_out: Option<Duration>,
}

impl WindowHold {
    /// Whether the window still holds the node job at its ask at `now`, its lane `lane` and its
    /// staged build `staged`. Only [`WindowUpdateLane::Lands`] runs the clock. Any other lane ends
    /// the run, as do another staged build and a silence longer than [`ASKS_UNBROKEN_WITHIN`].
    pub(crate) fn ask(&mut self, lane: WindowUpdateLane, staged: u64, now: Instant) -> HoldAnswer {
        let holds = |holds| HoldAnswer {
            holds,
            ran_out: None,
        };
        match lane {
            WindowUpdateLane::PersonHolds | WindowUpdateLane::Activates => {
                *self = Self::default();
                holds(true)
            }
            WindowUpdateLane::Leaves => {
                *self = Self::default();
                holds(false)
            }
            WindowUpdateLane::Lands => {
                let first = match self.since {
                    Some((first, latest))
                        if self.staged == staged
                            && now.saturating_duration_since(latest) <= ASKS_UNBROKEN_WITHIN =>
                    {
                        first
                    }
                    _ => {
                        *self = Self {
                            staged,
                            ..Self::default()
                        };
                        now
                    }
                };
                self.since = Some((first, now));
                let held = now.saturating_duration_since(first);
                if held < WINDOW_HOLDS_AN_UPDATE_WITHIN {
                    return holds(true);
                }
                let ran_out = (!self.told).then_some(held);
                self.told = true;
                HoldAnswer {
                    holds: false,
                    ran_out,
                }
            }
        }
    }
}

/// This window's live answer to [`RelayHeader::Applies`] (§4.13). Its event loop publishes its
/// update lane on every turn ([`Self::note_lane`]), and the relay's own threads keep
/// [`WindowHold`]'s clock from the node job's asks. A window whose event loop is stuck therefore
/// still lets the job install within the bound.
pub(crate) struct WindowApplies {
    /// Set once this window's event loop has ended ([`window_stops_applying`]).
    stopped: AtomicBool,
    /// The [`WindowUpdateLane`] its event loop last published.
    lane: AtomicU8,
    /// The staged build its event loop last published, `0` for none.
    staged: AtomicU64,
    hold: Mutex<WindowHold>,
}

impl WindowApplies {
    pub(crate) const fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            lane: AtomicU8::new(WindowUpdateLane::Lands as u8),
            staged: AtomicU64::new(0),
            hold: Mutex::new(WindowHold {
                staged: 0,
                since: None,
                told: false,
            }),
        }
    }

    /// The event loop's word on its lane and its staged build, which it gives on every turn. Two
    /// stores without a lock: an ask between them reads the old lane with the new build, and the
    /// next ask reads both.
    pub(crate) fn note_lane(&self, lane: WindowUpdateLane, staged: Option<u64>) {
        self.staged.store(staged.unwrap_or(0), Ordering::Release);
        self.lane.store(lane as u8, Ordering::Release);
    }

    /// From here on it answers `no` ([`window_stops_applying`]).
    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    /// What the relay answers at `now`, of the `applies` [`window_applies_updates`] read once for
    /// the process.
    pub(crate) fn answer(&self, applies: bool, now: Instant) -> bool {
        if !applies || self.stopped.load(Ordering::Acquire) {
            return false;
        }
        let lane = WindowUpdateLane::from_u8(self.lane.load(Ordering::Acquire));
        let staged = self.staged.load(Ordering::Acquire);
        let answer = self.hold.lock_or_recover().ask(lane, staged, now);
        if let Some(held) = answer.ran_out {
            aterm_log::info!(
                "window fleet relay: the node job has deferred this Mac's waiting build to this \
                 window for {} min without it landing (staged build {staged}, 0 for none in this \
                 window's view): it now says it does not apply it, so the job installs it on disk \
                 and this window switches to it as an installed activation",
                held.as_secs() / 60
            );
        }
        answer.holds
    }
}

/// This process's [`WindowApplies`], which its relay answers from.
pub(crate) static WINDOW_APPLIES: WindowApplies = WindowApplies::new();

/// This window applies nothing from here on, whatever its lane last said: its update lane ended
/// with its event loop, while the relay thread answers through a teardown that can outlast it (a
/// stuck export's wait has no bound). The node job takes over at its next look.
pub(crate) fn window_stops_applying() {
    WINDOW_APPLIES.stop();
}

/// Start the current window's `wf-<pid>.sock` relay inside `sock_dir` after Commit and control
/// activation, so `aterm node serve` can forward `Survey` and `Drive` connections to this window.
/// `applies` is [`window_applies_updates`], read once for the process; what the relay answers
/// within it is live ([`WindowApplies::answer`]).
pub(crate) fn start_window_fleet_relay(sock_dir: &Path, applies: bool) {
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
    WINDOW_RELAY_APPLIES.store(applies, Ordering::Release);
    // The thread is named by its fn, not a closure that calls it, so the main-loop census reads
    // the relay's serving as the thread's, not the event loop's; what it needs goes in statics.
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
    let applies = WINDOW_RELAY_APPLIES.load(Ordering::Acquire);
    let dir = {
        let slot = WINDOW_RELAY_DIR
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.clone()
    };
    let Some(dir) = dir else {
        return;
    };
    let mut readiness = window_node_readiness(applies);
    let pid = std::process::id();
    let attach = move |header: RelayHeader, stream: CtlStream| -> bool {
        match header {
            RelayHeader::Survey {
                content,
                drive,
                peer,
            } => crate::control::attach_fleet_connection(stream, (content, drive), peer, None),
            RelayHeader::Drive {
                driver,
                key,
                peer,
                requester,
                ..
            } => {
                if let Some(key) = key {
                    super::note_admitted_driver(&driver.node, key);
                }
                // Service repair is handled by the node before relay, where its configured
                // vault and state can be checked. A later command on this relayed connection
                // must not reinterpret a custom node context as the window's live context.
                crate::control::attach_fleet_drive(
                    stream,
                    peer,
                    driver,
                    key,
                    false,
                    crate::control::DriveRequester::Relayed(requester),
                )
            }
            RelayHeader::Ask { peer } => crate::members_desk::attach_relayed(stream, peer),
            RelayHeader::Applies => {
                answer_applies(stream, WINDOW_APPLIES.answer(applies, Instant::now()))
            }
            RelayHeader::Needs => crate::control::attach_needs_stream(stream),
        }
    };
    let attach = Arc::new(attach);
    keep_window_relay(
        || {
            let attach = Arc::clone(&attach);
            serve_window_relay_with(
                &dir,
                pid,
                None,
                |sid: &SessionId| crate::control::hosts_or_keeps(sid),
                move |header, stream| attach(header, stream),
                Some(&mut readiness),
                |listener, wait| {
                    aterm_uds::accept(listener, wait, aterm_uds::AcceptedMode::Blocking)
                        .map(|(stream, _)| stream)
                },
            )
        },
        std::thread::sleep,
    );
}

/// Only the committed, installed account window renews its node. Raw isolation markers also
/// stand aside in release builds, whose development state override is intentionally absent.
fn window_node_upkeep_applies(installed: bool, own_account: bool, isolated: bool) -> bool {
    installed && own_account && !isolated
}

/// The committed relay's readiness and maintenance, run by its owner only after it has bound.
/// Keeping these together needs no second worker whose failed start could strand the gate.
pub(crate) struct RelayReadiness {
    ready: Arc<AtomicBool>,
    maintain: Box<dyn FnMut()>,
}

/// Keep a committed relay present while its installed node catches up. Its admission gate
/// opens only after a positive image observation; later reads make no further renewal attempt.
/// A launchd refusal is recorded once and ordinary upkeep can still satisfy this wait later.
fn window_node_readiness(applies: bool) -> RelayReadiness {
    let coordinate = window_node_upkeep_applies(
        applies && aterm_update::running_copy_installable() == Some(true),
        aterm_types::dirs::not_own_account().is_none(),
        node_run_updates_and_claims_nothing(),
    );
    let ready = Arc::new(AtomicBool::new(!coordinate));
    let answer = Arc::clone(&ready);
    let mut attempted = false;
    let mut next = Instant::now();
    let mut pause = Duration::from_secs(1);
    let mut said = None;
    let maintain = Box::new(move || {
        if answer.load(Ordering::Acquire)
            || Instant::now() < next
            || WINDOW_APPLIES.stopped.load(Ordering::Acquire)
        {
            return;
        }
        use aterm_agent::node_service::{WindowNode, renew_for_window_with, window_node_with};
        let read = match (
            aterm_types::dirs::home_dir(),
            aterm_types::dirs::state_dir(),
            std::env::current_exe().and_then(|program| program.canonicalize()),
        ) {
            (Some(home), Some(state), Ok(program)) => {
                let config = aterm_types::dirs::aterm_config_path();
                let supervisor = aterm_agent::node_service::supervisor_here();
                if !std::mem::replace(&mut attempted, true) {
                    renew_for_window_with(
                        &home,
                        &state,
                        config.as_deref(),
                        &program,
                        false,
                        supervisor,
                        |pause| {
                            if WINDOW_APPLIES.stopped.load(Ordering::Acquire) {
                                return false;
                            }
                            std::thread::sleep(pause);
                            !WINDOW_APPLIES.stopped.load(Ordering::Acquire)
                        },
                    )
                } else {
                    // Only observe after the bounded attempt; ordinary upkeep owns retries.
                    window_node_with(
                        &home,
                        &state,
                        config.as_deref(),
                        &program,
                        false,
                        supervisor,
                    )
                }
            }
            _ => Err("the installed node paths are unreadable".to_owned()),
        };
        if matches!(read, Ok(WindowNode::Ready | WindowNode::Unmanaged)) {
            answer.store(true, Ordering::Release);
            return;
        }
        if let Err(why) = read
            && said.as_ref() != Some(&why)
        {
            aterm_log::warn!("window fleet relay: node readiness waits: {why}");
            said = Some(why);
        }
        next = Instant::now() + pause;
        pause = pause.saturating_mul(2).min(RELAY_BACKOFF_MAX);
    });
    RelayReadiness { ready, maintain }
}

/// The longest pause between two binds of a window's relay ([`keep_window_relay`]), and between two
/// accepts that met a fault that can pass ([`serve_window_relay_with`]).
const RELAY_BACKOFF_MAX: Duration = Duration::from_secs(5);
/// The first such pause.
const RELAY_BACKOFF_FIRST: Duration = Duration::from_millis(50);

/// The next pause after `pause`, doubled up to [`RELAY_BACKOFF_MAX`].
fn relay_backoff(pause: Duration) -> Duration {
    pause
        .saturating_mul(2)
        .clamp(RELAY_BACKOFF_FIRST, RELAY_BACKOFF_MAX)
}

/// Serve this window's relay for as long as the window lives: `serve` binds it and serves until it
/// fails, which this says and binds again after a pause (`sleep`) that doubles up to
/// [`RELAY_BACKOFF_MAX`]. A window whose relay ended without a word read as no window to the node
/// job, which then installed an update beside it; one that serves again is seen again. Each fault
/// is said once until another takes its place, so a relay that cannot bind for good (its folder
/// gone, its path held) writes one line, not one each pause. It returns only where `serve` does
/// without a fault: its stop was asked.
fn keep_window_relay(
    mut serve: impl FnMut() -> std::io::Result<()>,
    mut sleep: impl FnMut(Duration),
) {
    let mut pause = Duration::ZERO;
    let mut said: Option<String> = None;
    while let Err(err) = serve() {
        pause = relay_backoff(pause);
        let fault = err.to_string();
        if said.as_deref() != Some(fault.as_str()) {
            aterm_log::warn!(
                "window fleet relay: stopped serving ({fault}); it binds again after a pause of \
                 up to {} s, and says no more until the fault changes",
                RELAY_BACKOFF_MAX.as_secs()
            );
            said = Some(fault);
        }
        sleep(pause);
    }
}

/// Answer [`RelayHeader::Applies`] on `stream`, ahead of the relay's `OK`, which the asker never
/// reads ([`relay_reply_applies`]).
fn answer_applies(mut stream: CtlStream, applies: bool) -> bool {
    let _ = stream.write_all(if applies { b"yes\n" } else { b"no\n" });
    let _ = stream.flush();
    true
}

/// The node job's `ask` handler: each asking Mac's exchange relayed to the window's members
/// desk ([`RelayHeader::Ask`], [`aterm_secrets::members::relay`]). With no window, `automatic`
/// serves the node's current automatic LAN policy; without one it refuses. A running window
/// that takes no hand-over is still reported as busy.
fn relay_ask(
    windows: WindowRelays,
    automatic: Option<aterm_net::fleet::AskHandler>,
) -> aterm_net::fleet::AskHandler {
    Arc::new(
        move |stream: &mut dyn aterm_net::fleet::Duplex, peer: SocketAddr| {
            let mut window = match relay_to_window(&windows, &RelayHeader::Ask { peer }) {
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

/// Serve this window's `wf-<pid>.sock` relay in `relay_dir` until `stop` (or for good): each
/// header node serve hands over is `attach`ed, except a drive naming a session `hosts` says this
/// window neither hosts nor keeps a retired worker's row of, which it answers `ELSEWHERE` so node
/// serve asks the next window ([`handle_relay_accept`]).
#[cfg(test)]
pub(crate) fn serve_window_relay_at<H, F>(
    relay_dir: &Path,
    pid: u32,
    stop: Option<&AtomicBool>,
    hosts: H,
    attach: F,
) -> std::io::Result<()>
where
    H: Fn(&SessionId) -> bool + Send + Sync + 'static,
    F: Fn(RelayHeader, CtlStream) -> bool + Send + Sync + 'static,
{
    serve_window_relay_with(
        relay_dir,
        pid,
        stop,
        hosts,
        attach,
        None,
        |listener, wait| {
            aterm_uds::accept(listener, wait, aterm_uds::AcceptedMode::Blocking).map(|(s, _)| s)
        },
    )
}

/// Whether an accept that failed with `err` may pass while the relay keeps its socket: a connection
/// its peer aborted before the accept, a process or system out of descriptors, buffers or memory
/// for a moment, or a wait the call gave up. Each costs that connection at most; ending the relay
/// for one cost the window its presence to the node job for the rest of its life.
fn accept_fault_passes(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::OutOfMemory
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::Interrupted
    ) || matches!(
        err.raw_os_error(),
        Some(libc::ECONNABORTED | libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

/// Serve the window's relay with its `accept`, which a test makes fail, and optional readiness.
/// Readiness maintenance starts after bind; while it waits, the socket still proves a window
/// exists, and hand-overs are refused before any header is interpreted or command served.
pub(crate) fn serve_window_relay_with<H, F>(
    relay_dir: &Path,
    pid: u32,
    stop: Option<&AtomicBool>,
    hosts: H,
    attach: F,
    mut readiness: Option<&mut RelayReadiness>,
    accept: impl Fn(&CtlListener, aterm_uds::AcceptWait) -> std::io::Result<CtlStream>,
) -> std::io::Result<()>
where
    H: Fn(&SessionId) -> bool + Send + Sync + 'static,
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
    struct UnlinkOnDrop(PathBuf);
    impl Drop for UnlinkOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _unlink = UnlinkOnDrop(sock_path);
    let attach = Arc::new(attach);
    let hosts = Arc::new(hosts);

    let mut pause = Duration::ZERO;
    let mut fault_said = false;
    loop {
        if stop.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Ok(());
        }
        // The pending socket exists before any renewal. Even an old node sees a window
        // here and reports Busy while this owner observes the node, never NoWindow.
        if let Some(readiness) = readiness.as_mut() {
            (readiness.maintain)();
        }
        let pending = readiness
            .as_ref()
            .is_some_and(|readiness| !readiness.ready.load(Ordering::Acquire));
        let accepted = accept(
            &listener,
            if stop.is_some() {
                aterm_uds::AcceptWait::Ready
            } else if pending {
                aterm_uds::AcceptWait::Until(Instant::now() + Duration::from_secs(1))
            } else {
                aterm_uds::AcceptWait::Forever
            },
        );
        if accepted.is_ok() {
            pause = Duration::ZERO;
            fault_said = false;
        }
        match accepted {
            Ok(mut stream) => {
                if crate::control_auth::peer_check(&stream).is_err() {
                    aterm_uds::hangup::hang_up(&stream);
                    continue;
                }
                let attach = Arc::clone(&attach);
                let hosts = Arc::clone(&hosts);
                let ready = readiness
                    .as_ref()
                    .map(|readiness| Arc::clone(&readiness.ready));
                let _ = std::thread::Builder::new()
                    .name("aterm-wf-admit".to_owned())
                    .spawn(move || {
                        handle_relay_accept(
                            &mut stream,
                            &*hosts,
                            &*attach,
                            ready
                                .as_ref()
                                .is_none_or(|ready| ready.load(Ordering::Acquire)),
                        );
                    });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(err) if pending && err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) if accept_fault_passes(&err) => {
                pause = relay_backoff(pause);
                if !std::mem::replace(&mut fault_said, true) {
                    aterm_log::warn!(
                        "window fleet relay: an accept failed ({err}); it keeps its socket and \
                         accepts again within {} ms",
                        RELAY_BACKOFF_MAX.as_millis()
                    );
                }
                std::thread::sleep(pause);
            }
            Err(err) => return Err(err),
        }
    }
}

fn handle_relay_accept(
    stream: &mut CtlStream,
    hosts: &dyn Fn(&SessionId) -> bool,
    attach: &dyn Fn(RelayHeader, CtlStream) -> bool,
    ready: bool,
) {
    let _ = stream.set_read_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let line = match read_line_raw(stream, RELAY_HEADER_MAX) {
        Ok(Some(line)) => line,
        Ok(None) | Err(_) => {
            aterm_uds::hangup::hang_up(stream);
            return;
        }
    };
    if !ready {
        let _ = stream.write_all(b"ERR fleet node updating\n");
        let _ = stream.flush();
        aterm_uds::hangup::hang_up(stream);
        return;
    }
    let header = match RelayHeader::parse(&line) {
        Ok(header) => header,
        Err(why) => {
            let _ = stream.write_all(format!("ERR {why}\n").as_bytes());
            let _ = stream.flush();
            aterm_uds::hangup::hang_up(stream);
            return;
        }
    };
    // A drive goes to the window that hosts its sessions: one hosting not every one says which
    // it does, and node serve asks the next window, or splits a wait across them.
    if let RelayHeader::Drive { sids, .. } = &header {
        let hosted: Vec<SessionId> = sids.iter().filter(|sid| hosts(sid)).cloned().collect();
        if hosted.len() < sids.len() {
            let _ = stream.write_all(elsewhere_line(&hosted).as_bytes());
            let _ = stream.flush();
            aterm_uds::hangup::hang_up(stream);
            return;
        }
    }
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
pub(crate) fn read_line_raw(
    stream: &mut impl Read,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
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
    read_live_relays(relay_dir).unwrap_or_default()
}

/// The live windows' relays in `relay_dir`, newest process first, a dead one's removed; the
/// folder's read error where it cannot be listed.
fn read_live_relays(relay_dir: &Path) -> std::io::Result<Vec<(u32, PathBuf)>> {
    let entries = std::fs::read_dir(relay_dir)?;
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
    Ok(out)
}

/// How many live aterm windows other than this process keep a relay in this one's relay folder
/// (every window with a control socket on this state home keeps one there); `None` where it
/// cannot say: this process keeps none, its folder cannot be listed, or the listing lacks this
/// process's own relay (a folder removed or replaced under it). It lists a folder and removes a
/// dead window's relay, so the main thread never asks it.
pub(crate) fn other_live_windows() -> Option<usize> {
    let dir = WINDOW_RELAY_DIR
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()?;
    others_beside(std::process::id(), &dir)
}

/// The live windows other than process `me` with a relay in `relay_dir`; `None` where the folder
/// cannot be listed or lists no relay of `me`'s.
fn others_beside(me: u32, relay_dir: &Path) -> Option<usize> {
    let live = read_live_relays(relay_dir).ok()?;
    live.iter()
        .any(|(pid, _)| *pid == me)
        .then(|| live.iter().filter(|(pid, _)| *pid != me).count())
}

fn has_live_window_relay(relay_dir: &Path) -> bool {
    !live_relay_candidates(relay_dir).is_empty()
}

/// This Mac's windows as node serve hands a fleet connection to them: the folder of their
/// `wf-<pid>.sock` relays, and the launcher that knows which of them runs this node service's
/// own app ([`WindowLauncher::runs_this_app`]).
#[derive(Clone)]
pub(crate) struct WindowRelays {
    dir: PathBuf,
    launcher: Arc<dyn WindowLauncher>,
}

impl WindowRelays {
    pub(crate) fn new(dir: PathBuf, launcher: Arc<dyn WindowLauncher>) -> Self {
        Self { dir, launcher }
    }

    /// The folder of the relays, where a woken window binds its own.
    fn dir(&self) -> &Path {
        &self.dir
    }

    /// The live relays in the order a hand-over tries them: the windows of this node service's
    /// own app first, then the rest (a dev build's, or one an update left behind), newest first
    /// within each. A second window someone starts on this Mac's state, from another build, so
    /// never takes a drive that names no session from the installed app's.
    fn in_order(&self) -> Vec<(u32, PathBuf)> {
        let mut windows = live_relay_candidates(&self.dir);
        // Stable: newest first stays within each side.
        windows.sort_by_key(|(pid, _)| !self.launcher.runs_this_app(*pid));
        windows
    }
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

/// What this Mac's windows answered a hand-over none of them took ([`hand_over_in_order`]).
struct Declined {
    /// Each window that hosts not every session the drive names ([`RelayHeader::Drive`]'s
    /// `sids`), in hand-over order, with those of them it does host or keep.
    hosting: Vec<(PathBuf, Vec<SessionId>)>,
    /// Why, last heard, a window took none for another reason: one that may host the rest.
    busy: Option<String>,
}

/// What one window answered a hand-over ([`hand_over`]).
enum HandOver {
    /// It took it: the stream to relay on.
    Took(CtlStream),
    /// It hosts or keeps not every session the header names, only these (`ELSEWHERE hosts=…`).
    Elsewhere(Vec<SessionId>),
    /// It took none: why.
    Busy(String),
}

/// The line a window answers a drive header whose sessions it does not all host or keep, naming
/// those it does ([`handle_relay_accept`]).
fn elsewhere_line(hosted: &[SessionId]) -> String {
    format!("ELSEWHERE hosts={}\n", sid_list(hosted))
}

/// `sids` as a relay line carries them: comma-separated, `-` for none.
fn sid_list(sids: &[SessionId]) -> String {
    if sids.is_empty() {
        return "-".to_owned();
    }
    sids.iter()
        .map(SessionId::as_str)
        .collect::<Vec<_>>()
        .join(",")
}

/// [`sid_list`] read back: `None` where an element is no session id a drive can name.
fn parse_sid_list(list: &str) -> Option<Vec<SessionId>> {
    if list == "-" {
        return Some(Vec::new());
    }
    list.split(',')
        .map(|sid| relay_names(sid).then(|| SessionId::new(sid)))
        .collect()
}

/// Whether `sid` is a session id a relay line carries: `s-` and then letters, digits, `-` or
/// `_`, as this Mac mints them (`aterm_session::SessionId`), so no list separator or space can
/// ride in one.
fn relay_names(sid: &str) -> bool {
    sid.strip_prefix("s-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

/// Hand `wire` to the window whose relay is `path`, and read what it answered.
fn hand_over(path: &Path, wire: &str) -> HandOver {
    hand_over_within(path, wire, RELAY_CONNECT_TIMEOUT)
}

/// The same hand-over with its I/O budget supplied by the caller.
fn hand_over_within(path: &Path, wire: &str, within: Duration) -> HandOver {
    let deadline = Instant::now() + within;
    // Kept out of children from its creation (`aterm_uds::connect::until`).
    let Ok(mut stream) = aterm_uds::connect::until(path, deadline) else {
        return HandOver::Busy("its relay socket took no connection".to_owned());
    };
    if crate::control_auth::peer_check(&stream).is_err() {
        aterm_uds::hangup::hang_up(&stream);
        return HandOver::Busy("its relay socket is not this account's".to_owned());
    }
    let _ = stream.set_read_timeout(Some(within));
    let _ = stream.set_write_timeout(Some(within));
    if stream.write_all(wire.as_bytes()).is_err() || stream.flush().is_err() {
        aterm_uds::hangup::hang_up(&stream);
        return HandOver::Busy("it hung up on the hand-over".to_owned());
    }
    let Some(reply) = read_line_raw(&mut stream, RELAY_HEADER_MAX).ok().flatten() else {
        aterm_uds::hangup::hang_up(&stream);
        return HandOver::Busy("it did not answer the hand-over".to_owned());
    };
    let reply = reply.trim_end_matches(['\r', '\n']);
    if reply != "OK" {
        aterm_uds::hangup::hang_up(&stream);
        if let Some(hosted) = reply
            .strip_prefix("ELSEWHERE hosts=")
            .and_then(parse_sid_list)
        {
            return HandOver::Elsewhere(hosted);
        }
        return HandOver::Busy(aterm_log::sanitize_record(reply).into_owned());
    }
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(None);
    HandOver::Took(stream)
}

/// Hand `header` to the first window that takes it, in [`WindowRelays::in_order`]'s order: the
/// installed app's windows first. A drive that names sessions is taken only by a window that
/// hosts or keeps every one; one that names none, a survey's and an ask's, by the first that
/// answers.
fn hand_over_in_order(windows: &WindowRelays, header: &RelayHeader) -> Result<CtlStream, Declined> {
    hand_over_in_order_within(windows, header, RELAY_CONNECT_TIMEOUT)
}

fn hand_over_in_order_within(
    windows: &WindowRelays,
    header: &RelayHeader,
    within: Duration,
) -> Result<CtlStream, Declined> {
    let wire = header.encode();
    let mut declined = Declined {
        hosting: Vec::new(),
        busy: None,
    };
    for (_pid, path) in windows.in_order() {
        match hand_over_within(&path, &wire, within) {
            HandOver::Took(stream) => return Ok(stream),
            HandOver::Elsewhere(hosted) => declined.hosting.push((path, hosted)),
            HandOver::Busy(why) => declined.busy = Some(why),
        }
    }
    Err(declined)
}

/// [`hand_over_in_order`] of a header that names no session. A window that runs and takes none
/// is [`RelayOpen::Busy`], never [`RelayOpen::NoWindow`], so a peer is never told this Mac has
/// no window (nor opened a second one) while one is up.
fn relay_to_window(windows: &WindowRelays, header: &RelayHeader) -> RelayOpen {
    relay_to_window_within(windows, header, RELAY_CONNECT_TIMEOUT)
}

fn relay_to_window_within(
    windows: &WindowRelays,
    header: &RelayHeader,
    within: Duration,
) -> RelayOpen {
    match hand_over_in_order_within(windows, header, within) {
        Ok(stream) => RelayOpen::Opened(stream),
        Err(Declined {
            busy: Some(why), ..
        }) => RelayOpen::Busy(why),
        Err(Declined {
            hosting,
            busy: None,
        }) if !hosting.is_empty() => {
            RelayOpen::Busy("it hosts not the sessions the hand-over names".to_owned())
        }
        Err(Declined { .. }) => RelayOpen::NoWindow,
    }
}

/// [`relay_to_window`]'s stream, where a window took it.
fn open_window_relay(windows: &WindowRelays, header: &RelayHeader) -> Option<CtlStream> {
    match relay_to_window(windows, header) {
        RelayOpen::Opened(stream) => Some(stream),
        RelayOpen::NoWindow | RelayOpen::Busy(_) => None,
    }
}

/// What a survey is answered while a window is open and took no fleet connection.
fn busy_line(why: &str) -> String {
    format!(
        "ERR busy: an aterm window is open on this Mac and took no fleet connection ({}); ask \
         again\n",
        aterm_log::sanitize_record(why)
    )
}

/// This Mac's windows' survey facts, read as one ([`survey_every_window`]): every window's
/// sessions counted, the installed app's window's facts.
pub(crate) fn query_window_survey_facts(
    windows: &WindowRelays,
) -> Option<aterm_agent::survey_facts::SwarmSurveyFacts> {
    let header = RelayHeader::Survey {
        content: false,
        drive: false,
        peer: None,
    };
    let WindowsSurveyed::Answered(wire) = survey_every_window(windows, &header, "survey\n") else {
        return None;
    };
    aterm_agent::survey_facts::SwarmSurveyFacts::from_survey_reply(&wire).ok()
}

/// What a survey of every window here found ([`survey_every_window`]).
enum WindowsSurveyed {
    /// Their answers, merged ([`merge_window_surveys`]), or the installed app's window's one
    /// refusal line where none answered with a survey.
    Answered(String),
    /// No window took the hand-over: why, last heard (or `None`: no window runs).
    Untaken(Option<String>),
}

/// The bounds of one window's survey answer: as many lines as a survey sends at most, in the
/// reply bound a fleet reader takes.
const SURVEY_LINES_MAX: usize = 4096;
const SURVEY_LINE_MAX: usize = aterm_types::control_verbs::MAX_REQUEST_LINE;

/// How long a survey of this Mac waits for its windows' answers: as long as a fleet reader waits
/// at most for one (`aterm ctl survey --timeout`'s bound, 10 s), as the reader's own deadline
/// alone bounded a window's answer when it was relayed whole. A unit test's windows are its own,
/// which answer: they wait the hang detector.
#[cfg(not(test))]
const SURVEY_ANSWER_WITHIN: Duration = Duration::from_secs(10);
#[cfg(test)]
const SURVEY_ANSWER_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

/// How many bytes of a window's counted answer one `read(2)` may take ([`read_counted_through`]).
const SURVEY_READ_BUFFER: usize = 8 * 1024;

/// Read a counted answer (`OK <n>` and `n` lines, within [`SURVEY_LINES_MAX`] lines and
/// `aterm_net::fleet::REPLY_MAX` bytes) from `stream` by `deadline`: `Ok` its whole text, `Err` its
/// one line where it answered otherwise (`None`: nothing whole by then). The answer is read
/// through a buffer ([`read_counted_through`]), which may take bytes that follow it: the caller
/// reads nothing more of `stream` ([`ask_window_survey`] hangs it up).
fn read_counted(stream: &mut CtlStream, deadline: Instant) -> Result<String, Option<String>> {
    read_counted_through(stream, deadline, |stream, left| {
        // macOS refuses the option (`EINVAL`) on a socket its peer closed, whose answer may still
        // wait unread; a closed peer cannot hold the read past it.
        let _ = stream.set_read_timeout(Some(left));
    })
}

/// [`read_counted`] of any reader, at most [`SURVEY_READ_BUFFER`] bytes at a read. Each line
/// starts only while `deadline` is ahead, and `bound` gives `stream` what was left to it then as
/// the longest any read of that line waits: before the line's first read, and not at all for a
/// line the buffer already holds whole, which reads nothing.
fn read_counted_through<S: Read>(
    stream: &mut S,
    deadline: Instant,
    bound: impl FnMut(&mut S, Duration),
) -> Result<String, Option<String>> {
    let mut lines = std::io::BufReader::with_capacity(
        SURVEY_READ_BUFFER,
        LineBound {
            stream,
            bound,
            owed: None,
        },
    );
    counted_answer(|| {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())?;
        lines.get_mut().owed = Some(left);
        read_line_buffered(&mut lines, SURVEY_LINE_MAX)
            .ok()
            .flatten()
    })
}

/// A reader that gives its stream the wait its line is owed ([`read_counted_through`]) before
/// the next read, once.
struct LineBound<'a, S, B> {
    stream: &'a mut S,
    bound: B,
    owed: Option<Duration>,
}

impl<S: Read, B: FnMut(&mut S, Duration)> Read for LineBound<'_, S, B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(left) = self.owed.take() {
            (self.bound)(self.stream, left);
        }
        self.stream.read(buf)
    }
}

/// [`read_line_raw`]'s line of a buffered reader, with its bound and its errors: a `read` is
/// made only where the buffer holds no whole line, and what it took past the line stays in the
/// buffer for the next.
fn read_line_buffered(
    reader: &mut impl std::io::BufRead,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    use std::io::BufRead as _;
    let mut buf = Vec::with_capacity(128);
    // One byte past the bound tells a line that ends there from one that runs on.
    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    reader.take(limit).read_until(b'\n', &mut buf)?;
    if buf.is_empty() {
        return Ok(None);
    }
    if buf.len() > max_bytes && buf.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line exceeds maximum length",
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// A counted answer (`OK <n>` and `n` lines, within [`SURVEY_LINES_MAX`] lines and
/// `aterm_net::fleet::REPLY_MAX` bytes) from `line`, which gives the next line or `None` where
/// none came whole: `Ok` its whole text, `Err` its one line where it answered otherwise (`None`:
/// nothing whole).
fn counted_answer(mut line: impl FnMut() -> Option<String>) -> Result<String, Option<String>> {
    let head = line().ok_or(None)?;
    let trimmed = head.trim_end_matches(['\r', '\n']);
    let Some(count) = trimmed
        .strip_prefix("OK ")
        .and_then(|rest| rest.split_ascii_whitespace().next())
    else {
        return Err(Some(head));
    };
    let count = (!count.is_empty()
        && (count == "0" || !count.starts_with('0'))
        && count.bytes().all(|b| b.is_ascii_digit()))
    .then(|| count.parse::<usize>().ok())
    .flatten()
    .filter(|&n| n <= SURVEY_LINES_MAX)
    .ok_or(None)?;
    let mut wire = head;
    for _ in 0..count {
        let line = line().ok_or(None)?;
        if wire.len() + line.len() > aterm_net::fleet::REPLY_MAX {
            return Err(None);
        }
        wire.push_str(&line);
    }
    Ok(wire)
}

/// What one window answered a survey ([`ask_window_survey`]): its whole answer, or its one line
/// (`Ok`) or why it gave none (`Err`).
type WindowSurvey = Result<String, Result<String, String>>;

/// Ask the window at `path` `request` on a hand-over of `wire`, its answer read by `deadline`.
fn ask_window_survey(path: &Path, wire: &str, request: &str, deadline: Instant) -> WindowSurvey {
    match hand_over(path, wire) {
        HandOver::Took(mut stream) => {
            let _ = stream.set_write_timeout(Some(RELAY_CONNECT_TIMEOUT));
            let answer = if stream
                .write_all(request.as_bytes())
                .and_then(|()| stream.flush())
                .is_ok()
            {
                read_counted(&mut stream, deadline).map_err(|line| {
                    line.ok_or_else(|| "it did not answer the survey whole".to_owned())
                })
            } else {
                Err(Err("it hung up on the survey".to_owned()))
            };
            aterm_uds::hangup::hang_up(&stream);
            answer
        }
        HandOver::Elsewhere(_) => Err(Err("it answered a survey as a drive".to_owned())),
        HandOver::Busy(why) => Err(Err(why)),
    }
}

/// Ask every window here `request` (a `survey [facts|content]` line) on a hand-over of `header`,
/// all at once, and merge their answers in [`WindowRelays::in_order`]'s order, the installed
/// app's window first: a survey of this Mac lists every window's sessions, not the one window
/// that took it.
///
/// Each window has [`SURVEY_ANSWER_WITHIN`] to answer; once one has, the rest have
/// [`RELAY_CONNECT_TIMEOUT`] more, so a stopped window delays a survey of this Mac by no more
/// than that, and is left out (`instances_read=` counts the windows read).
fn survey_every_window(
    windows: &WindowRelays,
    header: &RelayHeader,
    request: &str,
) -> WindowsSurveyed {
    let wire = header.encode();
    let deadline = Instant::now() + SURVEY_ANSWER_WITHIN;
    let order = windows.in_order();
    let (tx, rx) = std::sync::mpsc::channel::<(usize, WindowSurvey)>();
    let windows_n = order.len();
    let mut asked = 0;
    for (at, (_pid, path)) in order.into_iter().enumerate() {
        let (wire, request, tx) = (wire.clone(), request.to_owned(), tx.clone());
        let started = std::thread::Builder::new()
            .name("aterm-node-survey-window".to_owned())
            .spawn(move || {
                let _ = tx.send((at, ask_window_survey(&path, &wire, &request, deadline)));
            });
        asked += usize::from(started.is_ok());
    }
    drop(tx);
    let mut answers: Vec<(usize, WindowSurvey)> = Vec::with_capacity(asked);
    let mut until = deadline;
    while answers.len() < asked {
        let now = Instant::now();
        let Some(left) = until.checked_duration_since(now) else {
            break;
        };
        let Ok((at, answer)) = rx.recv_timeout(left) else {
            break;
        };
        if answer.is_ok() && until == deadline {
            until = deadline.min(now + RELAY_CONNECT_TIMEOUT);
        }
        answers.push((at, answer));
    }
    answers.sort_by_key(|(at, _)| *at);
    let mut surveys = Vec::new();
    let mut refused = None;
    let mut busy = None;
    for (_, answer) in answers {
        match answer {
            Ok(survey) => surveys.push(survey),
            Err(Ok(line)) => {
                refused.get_or_insert(line);
            }
            Err(Err(why)) => busy = Some(why),
        }
    }
    if !surveys.is_empty() {
        return WindowsSurveyed::Answered(merge_window_surveys(
            &surveys,
            aterm_net::fleet::REPLY_MAX,
        ));
    }
    if let Some(line) = refused {
        return WindowsSurveyed::Answered(line);
    }
    // `Untaken(None)` is a Mac with no window; one whose windows were not asked (no thread
    // started) is a read that failed, never a count of none.
    WindowsSurveyed::Untaken(busy.or_else(|| {
        (asked > 0)
            .then(|| "no window answered".to_owned())
            .or_else(|| (windows_n > 0).then(|| "no survey thread started".to_owned()))
    }))
}

/// The windows' survey answers (`OK <n>`, one `instance` line, `session` rows, then the Mac's
/// own lines: accounts, checkouts, …) merged into one answer of this Mac, at most `max` bytes.
/// The first answer (the installed app's window's) gives the `instance` line and the Mac's own
/// lines; its `sessions=` counts every window's sessions, `cut=` the rows sent cut to facts,
/// `instances=` at least every window read,
/// and `instances_read=` how many were, so a reader says `k of n instances read` only where some
/// went unread. Every window's `session` rows follow; past `max`, the rows a window itself would
/// cut first go (the least kept, the latest first), and `sessions=` still counts them, so a reader
/// reports them not sent. One answer is returned as it came.
fn merge_window_surveys(surveys: &[String], max: usize) -> String {
    if let [only] = surveys {
        return only.clone();
    }
    struct Read<'a> {
        instance: Option<&'a str>,
        rows: Vec<&'a str>,
        own: Vec<&'a str>,
    }
    fn read(survey: &str) -> Read<'_> {
        let mut read = Read {
            instance: None,
            rows: Vec::new(),
            own: Vec::new(),
        };
        for line in survey.lines().skip(1) {
            if line.starts_with("instance ") {
                read.instance.get_or_insert(line);
            } else if line.starts_with("session ") {
                read.rows.push(line);
            } else {
                read.own.push(line);
            }
        }
        read
    }
    let reads: Vec<Read<'_>> = surveys.iter().map(|survey| read(survey)).collect();
    let number = |line: Option<&str>, key: &str| -> usize {
        line.and_then(|line| {
            line.split_ascii_whitespace()
                .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
    };
    let sessions: usize = reads
        .iter()
        .map(|read| number(read.instance, "sessions"))
        .sum();
    let cuts: Vec<usize> = reads
        .iter()
        .map(|read| number(read.instance, "cut"))
        .collect();
    let instances = reads
        .iter()
        .map(|read| number(read.instance, "instances"))
        .max()
        .unwrap_or(0)
        .max(reads.len());
    let primary = &reads[0];
    let instance_line = |cut: usize| {
        let mut instance: Vec<String> = primary
            .instance
            .unwrap_or("instance")
            .split_ascii_whitespace()
            .filter(|token| !token.starts_with("instances_read="))
            .map(|token| match token.split_once('=') {
                Some(("sessions", _)) => format!("sessions={sessions}"),
                Some(("cut", _)) => format!("cut={cut}"),
                Some(("instances", _)) => format!("instances={instances}"),
                _ => token.to_owned(),
            })
            .collect();
        instance.push(format!("instances_read={}", reads.len()));
        instance.join(" ")
    };
    // Fitted with every window's cut rows counted, at least as long as the line it ends with.
    let instance = instance_line(cuts.iter().sum());
    // The rows in the order they are cut, as a window cuts its own: the least kept first, the
    // latest (the last window's last row) first among equals.
    let mut rows: Vec<(usize, usize, &str)> = reads
        .iter()
        .enumerate()
        .flat_map(|(window, read)| {
            read.rows
                .iter()
                .enumerate()
                .map(move |(at, row)| (window, at, *row))
        })
        .collect();
    let mut cut_order: Vec<usize> = (0..rows.len()).collect();
    cut_order.sort_by_key(|&at| {
        let (window, row, text) = rows[at];
        (row_kept(text), std::cmp::Reverse((window, row)))
    });
    let own_bytes: usize = primary.own.iter().map(|line| line.len() + 1).sum();
    let head_bytes =
        |sent: usize| format!("OK {}\n{instance}\n", sent + 1 + primary.own.len()).len();
    let mut total: usize = rows.iter().map(|(_, _, row)| row.len() + 1).sum();
    let mut sent = rows.len();
    let mut dropped = vec![false; rows.len()];
    for at in cut_order {
        if head_bytes(sent) + total + own_bytes <= max {
            break;
        }
        dropped[at] = true;
        total -= rows[at].2.len() + 1;
        sent -= 1;
    }
    let mut at = 0;
    rows.retain(|_| {
        let keep = !dropped[at];
        at += 1;
        keep
    });
    // `cut=` counts the rows sent cut to facts: of a window that cut some, each sent without the
    // `said=` every content row carries.
    let cut = rows
        .iter()
        .filter(|(window, _, row)| {
            cuts[*window] > 0 && !row.split_ascii_whitespace().any(|t| t.starts_with("said="))
        })
        .count();
    let mut out = format!(
        "OK {}\n{}\n",
        rows.len() + 1 + primary.own.len(),
        instance_line(cut)
    );
    for (_, _, row) in rows {
        out.push_str(row);
        out.push('\n');
    }
    for line in &primary.own {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// How much a survey keeps a session row, read off its words as the window ranked it when it
/// wrote it (`crate::survey`'s own rank): 2 for one that needs someone or is mid-turn
/// (`attention=`, `needs=`, an agent busy, prompting, asking or at a wall), 1 for another
/// agent's (`program_kind=agent`), 0 for the rest.
fn row_kept(row: &str) -> u8 {
    let word = |key: &str| {
        row.split_ascii_whitespace()
            .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or("-")
    };
    let agent = word("agent");
    if word("attention") != "-"
        || word("needs") != "-"
        || matches!(agent, "busy" | "prompt" | "question")
        || agent.starts_with("wall:")
    {
        2
    } else {
        u8::from(word("program_kind") == "agent")
    }
}

/// Configuration for the node job's supervision loop ([`serve_node_job`]), which production
/// builds in [`run_node_serve_owning`] and integration and negative-control tests build to run
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
    /// (`None`: a job that reads no vault, under a private state root, so its deny list names no
    /// one and `dns-sd` ticks and automatic admission are off; a job on the account's own state
    /// that finds no vault does not start, [`super::vault_under`]).
    pub vault_paths: Option<aterm_secrets::client::Paths>,
    /// This Mac's membership log, whose verdict admits a handshake, a join and a drive (`None`:
    /// the book's keyring alone).
    pub members: Option<Arc<aterm_agent::fleet_log::LocalLog>>,
    /// The `_alab._tcp` browse whose adverts give the heartbeat its dial hints (`None`: the
    /// process's own, started with the listener).
    pub alab_browse: Option<Arc<aterm_secrets::discover::Watch>>,
    /// Optional pre-bound TCP listener for deterministic port-0 tests.
    pub prebound_listener: Option<TcpListener>,
    /// Told the `[fleet] listen` each generation was started for, as it starts serving
    /// (`None`: no one): a test's tap, by which a test waits for a generation that serves the
    /// configuration as it stands, never one its watch is about to end.
    pub on_generation: Option<OnGeneration>,
    /// Poll interval for key rotation, config changes, and `_alab._tcp` reconciliation.
    pub tick_every: Duration,
    /// Maximum wait for a woken window's `wf-<pid>.sock` relay on windowless `spawn`.
    pub wake_timeout: Duration,
    /// The node job's stop, set by its updater or a test to end the run cleanly.
    pub stop: Arc<Stop>,
    /// Window launcher seam for windowless `Drive` `spawn` (§7.4).
    pub launcher: Arc<dyn WindowLauncher>,
    /// Why this job cannot use the local network now, in the LAN broker's words
    /// ([`crate::network_moved::lan_unavailable`]): on macOS aterm's LAN traffic goes through
    /// the root LAN broker, so while it does not answer the job advertises, listens and dials
    /// nothing on the LAN. `None` once it answers.
    pub lan_unavailable: fn() -> Option<String>,
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

/// What is told each generation's `[fleet] listen` as it starts serving
/// ([`NodeServeConfig::on_generation`]).
pub type OnGeneration = Arc<dyn Fn(&str) + Send + Sync>;

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
    /// The vault pass's bell ([`aterm_secrets::client::NodePassHost::rung`]): its host's watch
    /// over the switches it reads rings it, as does a test that moves [`Self::pass_now_ms`].
    /// `None`: the pass looks at its pace.
    pub pass_rung: Option<Arc<aterm_uds::ready::Doorbell>>,
    /// Whether the vault pass hears the LAN broker arrive
    /// ([`aterm_secrets::client::NodePassHost::broker`]): where the job's LAN gate reads it.
    pub pass_broker: bool,
    /// Starts the converge pass's retained request after local progress or a refresh boundary.
    pub pass_converge: Arc<
        dyn Fn(aterm_secrets::converge::RepositoryRequest) -> Option<aterm_uds::spawnfd::Child>
            + Send
            + Sync,
    >,
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
            pass_rung: aterm_uds::ready::Doorbell::new().ok().map(Arc::new),
            pass_broker: true,
            pass_converge: Arc::new(|request| {
                let exe = std::env::current_exe().ok()?;
                aterm_secrets::converge::start_requested(
                    &exe,
                    aterm_secrets::converge::SUBSCRIBER,
                    request,
                )
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
            pass_rung: None,
            pass_broker: false,
            pass_converge: Arc::new(|_| None),
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
/// newer build it may end its run for: a window that quits with a stage waiting, or that stops
/// saying it applies it, is seen within it.
const UPDATE_LOOK_EVERY: Duration = Duration::from_secs(60);

/// How long after a start that found a newer build and did not run it (its boot apply refused)
/// the job may end its run for that same build again: a refusal that passes (a held apply lock,
/// a roster not yet re-confirmed) is retried within the hour, and one that stands costs one
/// restart an hour, never a loop of them.
const RETRY_LEFT_AFTER: Duration = Duration::from_secs(60 * 60);

/// The node job's updater (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13, A4). The job checks
/// and stages as a window does, whether or not a window runs. With no window here, it runs what
/// it staged by ending its run, so launchd starts it again, and that start's boot applies the
/// stage before the job binds its listener. With a window that says it applies it, the window
/// applies its own, and one that says it does not leaves the job to install it. A test
/// injects its own updater, so it needs no launchd job, no bundle and no network; production's
/// is `LiveNodeUpdater` (macOS).
pub trait NodeUpdater: Send + Sync {
    /// Start the background check, which stages each newer build it finds and calls `noticed`
    /// after each stage, and once it sees a build another process staged or installed.
    fn start_checker(&self, noticed: Box<dyn Fn() + Send>);
    /// The newer build a start of this job would run: one staged and ready for a boot apply, or
    /// one another process already installed over this job's image. A staged download whose
    /// exact bytes a window here saw fail to start is none: a windowless start never runs,
    /// installs or supersedes with them (`aterm_update`'s window-start failure record), and a
    /// headless settle of such bytes already installed keeps their window re-trial at the count
    /// it carried (`aterm_update`'s window-trial witness).
    fn newer_build(&self) -> Option<u64>;
    /// `[update] auto_apply`: whether a newer build goes in by itself. Text that names it unreadably
    /// reads as no, as does a read that hits a fault that can pass or does not answer: that look
    /// holds, and the next asks again.
    fn auto_apply(&self) -> bool;
    /// The boot apply, which first counts this start against any boot trial of this build:
    /// where a stage is ready it swaps the stage in and execs the build it installed, and never
    /// returns. Otherwise it returns what it did.
    fn apply_at_boot(&self) -> String;
    /// Count and recover an already-armed trial without admitting any staged update.
    fn recover_trial_at_boot(&self) -> String;
    /// Whether this installed image has a trial still owed to its headless service.
    fn trial_pending(&self) -> bool;
    /// Confirm this build's boot trial; `false` asks again.
    fn confirm_trial(&self) -> bool;
    /// The build installed in this copy's place where it is older than the one this job runs
    /// and this job's image is no longer the installed file: a boot's crash-loop revert put it
    /// back and its re-exec failed, or a person installed an older release over this one.
    fn older_build_installed(&self) -> Option<u64>;
    /// Whether automatic checks (`[update] enabled`) as saved now differ from those this run's
    /// checker started under, which reads the setting once, at its start.
    fn checks_setting_moved(&self) -> bool;
    /// The installed activation a window here failed to switch to, with the newer stage ready to
    /// take its place, `(activation, newer)` ([`aterm_update::failed_activation_waiting`]): a
    /// read of a few small files, `None` where nothing waits.
    fn failed_activation(&self) -> Option<(u64, u64)>;
    /// Retire the activation [`Self::failed_activation`] found `waiting` and install the newer
    /// stage over the window's retained build ([`aterm_update::install_over_failed_activation`]).
    /// The words this run ends with where the install path changed, so the job's next start runs
    /// what it holds; `None` where it stays as it was, which this says once for each reason.
    fn install_over_failed_activation(&self, waiting: (u64, u64)) -> Option<String>;
    /// Look at the checker [`Self::start_checker`] started, and replace one that stopped
    /// stamping its heartbeat, as a window's watchdog replaces its own: with no window here,
    /// nothing else would; it also runs an update lane's start that got no thread to wait on its
    /// launch setting, once that setting answers ([`aterm_update::start_owed_launch_waits`]). Why
    /// this run ends where no replacement can check ([`mind_node_checker`]), once no trial is active
    /// ([`the_checker_ends_the_run`]), else `None`.
    fn mind_checker(&self) -> Option<String>;
    /// Record in the update record that this build's boot trial missed, for `why`, before a
    /// [`Self::restart`] ends the run for it: the revert a later start may make then holds the
    /// build with this reason ([`aterm_update::note_trial_miss`]).
    fn note_trial_miss(&self, why: &str);
    /// End this run so it is started again, saying `why`.
    fn restart(&self, why: &str);
    /// Give back the launch this run counted against its build's boot trial, as a run ends by
    /// choice ([`aterm_update::forgive_own_trial_launch`]): only a crash and a failed trial
    /// ([`NodeUpdates::failed_trial`]) count toward the trial's launch limit, so a build that
    /// serves is never reverted for the job's own restarts.
    fn forgive_launch(&self);
    /// How long `build` has waited on this machine, on a clock the job's restarts do not restart
    /// (`aterm_update::pending_update_waited`, which this ask starts where nothing follows the
    /// build yet). Where that clock does not answer, the wait since this run first asked for the
    /// build ([`waited_or_since`]): every answer bounds, so a silent window never holds for good.
    fn waited_for(&self, build: u64) -> Duration;
    /// The clock [`RETRY_LEFT_AFTER`] is read on.
    fn now(&self) -> Instant;
    /// How often the updater looks again without a notice.
    fn look_every(&self) -> Duration {
        UPDATE_LOOK_EVERY
    }
    /// Run `look` on its own thread, or say why it could not start.
    fn spawn_look(&self, look: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
        std::thread::Builder::new()
            .name("aterm-node-updates".to_owned())
            .spawn(look)
            .map(drop)
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

/// How long a node run's active trial goes on looking before what it found decides
/// ([`NodeUpdates::trial_unprovable`], [`NodeUpdates::trial_proof_missed`]). A loaded Mac misses a
/// self-dial (each has [`HANDSHAKE`]) for seconds, a vault writer holds its state for moments and a
/// window hands the port over within its tick; a build whose listener does not answer misses every
/// look.
const TRIAL_SETTLES_WITHIN: Duration = Duration::from_secs(5 * 60);

/// How long a trial's self-dial waits for its census reply once the listener has answered as this
/// very node ([`health::Unproven::ReplyLate`]), apart from [`HANDSHAKE`]: a census reads the vault
/// and the roster, which a writer or a Commons bootstrap can hold for seconds on a loaded Mac.
#[cfg(not(test))]
const HEALTH_REPLY_WITHIN: Duration = Duration::from_secs(10);
/// A unit test's listeners are its own, which answer: the hang detector.
#[cfg(test)]
const HEALTH_REPLY_WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Since when this run's looks have met each kind of thing that settled nothing, with nothing since
/// that said otherwise ([`NodeUpdates::trial_answered`]). A look that could not ask does not clear
/// `missed_since`, so a flapping input cannot keep a build that never answers from being decided.
#[derive(Default)]
struct TrialLooks {
    unprovable_since: Option<Instant>,
    missed_since: Option<Instant>,
    /// The updater would not settle this trial at its bound, and the log has said so once.
    refusal_said: bool,
}

/// A node run's updater and what its boot found ([`NodeServeConfig::updates`]).
#[derive(Clone)]
pub struct NodeUpdates {
    updater: Arc<dyn NodeUpdater>,
    boot: NodeBoot,
    /// A look could not start, so one is still owed ([`Self::start_owed_look`]).
    look_owed: Arc<AtomicBool>,
    trial_looks: Arc<std::sync::Mutex<TrialLooks>>,
}

impl NodeUpdates {
    /// `updater` for a run whose boot found `boot`.
    #[must_use]
    pub fn new(updater: Arc<dyn NodeUpdater>, boot: NodeBoot) -> Self {
        Self {
            updater,
            boot,
            look_owed: Arc::new(AtomicBool::new(false)),
            trial_looks: Arc::default(),
        }
    }

    /// Start the look one that could not start still owes. The job's loop asks between generations
    /// and a serving generation's watch at each tick, since a generation can last as long as the job.
    /// The checker's notices went with the look that could not start, so this one looks on the clock
    /// alone: this channel's sender goes with this line.
    fn start_owed_look(&self, relay_dir: &Path, stop: &Arc<Stop>) {
        if self.look_owed.swap(false, Ordering::AcqRel)
            && !spawn_node_look(
                self,
                relay_dir.to_path_buf(),
                Arc::clone(stop),
                std::sync::mpsc::sync_channel::<()>(1).1,
            )
        {
            self.look_owed.store(true, Ordering::Release);
        }
    }

    fn active_trial(&self) -> bool {
        // Window custody can expire or move after a look. Only the canonical
        // trial receipt, not this process's earlier checkpoint, owns that truth.
        let pending = self.updater.trial_pending();
        if !pending {
            *self.trial_looks.lock_or_recover() = TrialLooks::default();
        }
        pending
    }

    /// What the updater said of confirming the active trial, or `None` where nothing asked it.
    fn confirm_checkpoint(&self, stop: &Stop) -> Option<bool> {
        // Confirmation rechecks headless ownership under the apply lock. A
        // window may have acquired it since active_trial; its true return
        // can leave that window's trial armed, so never cache the result.
        (!stop.is_set() && self.active_trial() && !stop.is_set())
            .then(|| self.updater.confirm_trial())
    }

    /// This look settled what the earlier ones could not: a later miss or fault starts again.
    fn trial_answered(&self, stop: &Stop) {
        *self.trial_looks.lock_or_recover() = TrialLooks::default();
        let _ = self.confirm_checkpoint(stop);
    }

    /// Nothing can be asked of this build's listener now, for a reason the build did not bring: a
    /// config file the job will not serve from, a vault read mid-apply, a key that does not read, a
    /// port or address held elsewhere. A revert cures none of them and the next release would meet
    /// the same, so this look holds and the next asks again. Past [`TRIAL_SETTLES_WITHIN`] of such
    /// looks with no miss among them the trial is confirmed where the updater can, since a trial
    /// nothing confirms refuses every later update. Where it cannot, the trial stays armed and each
    /// look asks again: ending the run would count a launch against a build for a held lock.
    fn trial_unprovable(&self, stop: &Stop, why: &str) {
        let waited = self.trial_looked_for(|looks| &mut looks.unprovable_since, why);
        if waited < TRIAL_SETTLES_WITHIN {
            return;
        }
        match self.confirm_checkpoint(stop) {
            Some(true) => aterm_log::warn!(
                "aterm node serve: this run's boot trial settled unproven after {} s, since \
                 nothing here says anything of this build: {why}",
                waited.as_secs()
            ),
            Some(false)
                if !std::mem::replace(
                    &mut self.trial_looks.lock_or_recover().refusal_said,
                    true,
                ) =>
            {
                aterm_log::warn!(
                    "aterm node serve: this run's boot trial is past its {} s and did not settle, \
                     so later looks ask again: {why}",
                    TRIAL_SETTLES_WITHIN.as_secs()
                );
            }
            Some(false) | None => {}
        }
    }

    /// This build's listener did not answer its own authenticated dial. One miss says little on a
    /// loaded Mac, so the run ends only once misses have lasted [`TRIAL_SETTLES_WITHIN`] with no
    /// answer among them: the next start then counts against the trial, and with no window answering
    /// here a build that never answers reaches the revert.
    fn trial_proof_missed(&self, stop: &Stop, why: &str) {
        self.trial_looks.lock_or_recover().unprovable_since = None;
        let waited = self.trial_looked_for(|looks| &mut looks.missed_since, why);
        if waited >= TRIAL_SETTLES_WITHIN {
            self.failed_trial(stop, why);
        }
    }

    /// How long this run has met what `since` records, which this look may be the first of.
    fn trial_looked_for(
        &self,
        since: fn(&mut TrialLooks) -> &mut Option<Instant>,
        why: &str,
    ) -> Duration {
        let now = self.updater.now();
        let mut looks = self.trial_looks.lock_or_recover();
        let first = *since(&mut looks).get_or_insert_with(|| {
            aterm_log::info!("aterm node serve: this run's boot trial asks again: {why}");
            now
        });
        now.saturating_duration_since(first)
    }

    /// The trial's misses lasted: note the miss and its reason in the update record, then end the
    /// run, so the next start counts against the trial and a revert says why.
    fn failed_trial(&self, stop: &Stop, why: &str) {
        if !stop.is_set() && self.active_trial() && !stop.is_set() {
            self.updater.note_trial_miss(&format!(
                "the node service could not prove its listener: {why}"
            ));
            self.updater.restart(&format!(
                "the active node update trial could not prove its listener: {why}"
            ));
        }
    }

    fn settle_service(
        &self,
        inputs: &health::Inputs,
        (addr, secret, node, listen): (
            SocketAddr,
            &aterm_net::fleet::FleetSecret,
            &Option<(String, aterm_net::fleet::NodeKey)>,
            &str,
        ),
        stop: &Stop,
    ) -> bool {
        self.settle_service_with(inputs, (addr, secret, node, listen), stop, |basis| {
            basis.prove(
                addr,
                health::Bounds {
                    handshake: HANDSHAKE,
                    reply: HEALTH_REPLY_WITHIN,
                },
            )
        })
    }

    fn settle_service_with(
        &self,
        inputs: &health::Inputs,
        (addr, secret, node, listen): (
            SocketAddr,
            &aterm_net::fleet::FleetSecret,
            &Option<(String, aterm_net::fleet::NodeKey)>,
            &str,
        ),
        stop: &Stop,
        prove: impl FnOnce(&health::Basis) -> Result<health::Proven, health::Unproven>,
    ) -> bool {
        if stop.is_set() {
            return false;
        }
        if !self.active_trial() {
            return true;
        }
        let before = match inputs.observe() {
            health::Standing::Held => {
                if !stop.is_set() {
                    self.trial_answered(stop);
                }
                return !stop.is_set();
            }
            health::Standing::Unread(why) => {
                self.trial_unprovable(stop, why);
                return !stop.is_set();
            }
            health::Standing::Admitted(basis) => basis,
        };
        if !before.is_generation(secret, node, listen) {
            return false;
        }
        if stop.is_set() {
            return false;
        }
        let proof = prove(&before);
        if stop.is_set() {
            return false;
        }
        // A successful old exchange grants no checkpoint after its policy, key,
        // identity or generation changed. A withdrawn source grants no restart either.
        let after = inputs.observe();
        if stop.is_set() {
            return false;
        }
        match after {
            health::Standing::Admitted(after)
                if after.is_generation(secret, node, listen) && before.same_as(&after) =>
            {
                match proof {
                    Ok(proof) => self.confirm_service(stop, proof),
                    Err(health::Unproven::ReplyLate(why)) => self.trial_unprovable(stop, &why),
                    Err(health::Unproven::Missed(why)) => {
                        match health::self_dial_says_nothing(addr) {
                            Some(unprovable) => self.trial_unprovable(stop, unprovable),
                            None => self.trial_proof_missed(stop, &why),
                        }
                    }
                }
            }
            // A fault that met this read says what one that met the read before the dial does.
            health::Standing::Unread(why) => self.trial_unprovable(stop, why),
            _ => return false,
        }
        !stop.is_set()
    }

    fn confirm_service(&self, stop: &Stop, _proof: health::Proven) {
        self.trial_answered(stop);
    }
}

enum IdleCheckpoint<'a> {
    Waiting,
    PortOwnedElsewhere,
    Failed(&'a str),
}

fn checkpoint_idle(cfg: &NodeServeConfig, why: IdleCheckpoint<'_>) {
    checkpoint_idle_with(cfg, why, || a_window_relay_answers(&cfg.relay_dir));
}

fn checkpoint_idle_with(
    cfg: &NodeServeConfig,
    why: IdleCheckpoint<'_>,
    window_answers: impl FnOnce() -> bool,
) {
    let Some(updates) = &cfg.updates else {
        return;
    };
    if cfg.stop.is_set() || !updates.active_trial() {
        return;
    }
    let inputs = health::Inputs::of(cfg);
    let standing = inputs.observe();
    if cfg.stop.is_set() {
        return;
    }
    match standing {
        health::Standing::Held => updates.trial_answered(&cfg.stop),
        health::Standing::Unread(why) => updates.trial_unprovable(&cfg.stop, why),
        health::Standing::Admitted(before) => match why {
            IdleCheckpoint::Waiting => {
                updates.trial_unprovable(&cfg.stop, "the job is waiting to serve");
            }
            IdleCheckpoint::PortOwnedElsewhere => {
                let answers = window_answers();
                if cfg.stop.is_set() {
                    return;
                }
                let after = inputs.observe();
                if cfg.stop.is_set()
                    || !matches!(after, health::Standing::Admitted(after) if before.same_as(&after))
                {
                    return;
                }
                if answers {
                    updates.trial_answered(&cfg.stop);
                } else {
                    updates.trial_unprovable(&cfg.stop, "node port is held but no window answers");
                }
            }
            IdleCheckpoint::Failed(why) => updates.trial_unprovable(&cfg.stop, why),
        },
    }
}

/// What a window's `wf-<pid>.sock` says of its process to the node job's updater (§4.13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowPresence {
    /// Its relay took a connect from this account and answered [`RelayHeader::Applies`]: it
    /// applies this Mac's updates unless it said `no` ([`relay_reply_applies`]).
    Answers { applies: bool },
    /// Its process lives and runs an aterm image ([`runs_an_aterm_image`]), but its relay took no
    /// connect within [`RELAY_CONNECT_TIMEOUT`] (a listen queue full behind a window that is not
    /// scheduled, a relay between two binds) or did not answer the ask. That cannot be told from a
    /// window that applies its updates, so it holds the job as one does, for as long
    /// ([`silent_window_holds`]).
    Silent,
    /// Its process has exited, its pid now runs a program that is not aterm, or the socket's
    /// listener is another account's: no window.
    Gone,
}

/// What the relay at `path`, named for process `pid`, says of its window: its pid's liveness and
/// image first, by `runs_aterm`, then a connect and [`RelayHeader::Applies`]. A relay whose pid is
/// alive is never read as gone because its socket refuses: a window that is not scheduled stops
/// accepting, its listen queue fills, and every later connect is refused while it still holds its
/// single-owner state.
fn relay_presence(path: &Path, pid: u32, runs_aterm: impl FnOnce(u32) -> bool) -> WindowPresence {
    if !crate::control_auth::pid_alive(pid) || !runs_aterm(pid) {
        return WindowPresence::Gone;
    }
    let Ok(mut stream) = aterm_uds::connect::until(path, Instant::now() + RELAY_CONNECT_TIMEOUT)
    else {
        return WindowPresence::Silent;
    };
    if crate::control_auth::peer_check(&stream).is_err() {
        aterm_uds::hangup::hang_up(&stream);
        return WindowPresence::Gone;
    }
    let _ = stream.set_read_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(RELAY_CONNECT_TIMEOUT));
    let reply = stream
        .write_all(RelayHeader::Applies.encode().as_bytes())
        .and_then(|()| stream.flush())
        .ok()
        .and_then(|()| read_line_raw(&mut stream, RELAY_HEADER_MAX).ok().flatten());
    aterm_uds::hangup::hang_up(&stream);
    reply.map_or(WindowPresence::Silent, |reply| WindowPresence::Answers {
        applies: relay_reply_applies(&reply),
    })
}

/// Whether a relay's reply to [`RelayHeader::Applies`] leaves the applying to its process. Only
/// `no` says otherwise: an older build refuses a command it does not know, so it is held for as a
/// window has always been. Such an older headless or dev process holds the job until it exits or
/// updates, as does an older window, whose answer is read once for its process and never runs out.
fn relay_reply_applies(reply: &str) -> bool {
    reply != "no\n"
}

/// Whether live process `pid` runs an aterm program's image (`aterm`, `aterm-<verb>`, or this
/// process's own, which a test's windows share), told by the kernel's word on its executable; an
/// image the kernel will not name is held to be one, since a wrong `no` installs beside a window.
/// Linux names an image a later update replaced `<path> (deleted)`, and it is still that program.
pub(crate) fn runs_an_aterm_image(pid: u32) -> bool {
    let image = match aterm_uds::holders::exe_of(pid) {
        aterm_uds::holders::Exe::Exited => return false,
        aterm_uds::holders::Exe::Unnamed => return true,
        aterm_uds::holders::Exe::At(image) => image,
    };
    let name = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.strip_suffix(" (deleted)").unwrap_or(name).to_owned())
    };
    let Some(image) = name(&image) else {
        return true;
    };
    image == "aterm"
        || image.starts_with("aterm-")
        || std::env::current_exe().is_ok_and(|own| name(&own).as_deref() == Some(image.as_str()))
}

/// The `wf-<pid>.sock` files in `relay_dir`, live or not, each with its pid.
fn window_relays(relay_dir: &Path) -> Vec<(u32, PathBuf)> {
    std::fs::read_dir(relay_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str().and_then(parse_relay_filename)?;
            Some((pid, entry.path()))
        })
        .collect()
}

/// What each window's relay in `relay_dir` says of its process ([`relay_presence`]), its pid's
/// image judged by `runs_aterm` (production's is [`runs_an_aterm_image`]).
pub(crate) fn window_presences_by(
    relay_dir: &Path,
    runs_aterm: impl Fn(u32) -> bool,
) -> Vec<(u32, WindowPresence)> {
    window_relays(relay_dir)
        .into_iter()
        .map(|(pid, relay)| (pid, relay_presence(&relay, pid, &runs_aterm)))
        .collect()
}

/// Whether a window here holds the node job's install of `waiting` (the newer build its look found,
/// if any), as the job's updater asks it (§4.13): one that answers it applies this Mac's updates,
/// or one that is [`WindowPresence::Silent`] while [`silent_window_holds`]. It never asks
/// WindowServer. Every relay is asked, not only those up to the first that holds: each window's
/// hold runs from the asks it hears ([`WindowHold::ask`]), so several windows hold the job no
/// longer than the longest of them would alone. Which window holds it, an answering one first.
fn a_window_holds_the_update(
    updater: &dyn NodeUpdater,
    relay_dir: &Path,
    waiting: Option<u64>,
) -> Option<UpdateHolder> {
    let mut holder = None;
    for (pid, presence) in window_presences_by(relay_dir, runs_an_aterm_image) {
        let holds = match presence {
            WindowPresence::Answers { applies } => applies.then_some(UpdateHolder::Answering),
            WindowPresence::Silent => {
                silent_window_holds(updater, pid, waiting).then_some(UpdateHolder::Silent)
            }
            WindowPresence::Gone => None,
        };
        holder = holder.max(holds);
    }
    holder
}

/// The window that holds the node job's install of a waiting build ([`a_window_holds_the_update`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum UpdateHolder {
    /// A window whose process lives but whose relay does not answer ([`WindowPresence::Silent`]):
    /// it holds the install for its bound, and owns no boot trial, since it can neither launch
    /// nor confirm one.
    Silent,
    /// A window that answers and applies this Mac's updates itself.
    Answering,
}

/// What this run has said of each silent window, by pid: whether it held at the last say.
static SILENT_WINDOWS_SAID: Mutex<BTreeMap<u32, bool>> = Mutex::new(BTreeMap::new());

/// Whether the silent window `pid` holds the node job's install of `waiting`: as an answering window
/// that applies its updates does, for [`WINDOW_HOLDS_AN_UPDATE_WITHIN`], read here on the pending
/// clock the update check keeps for the waiting build ([`NodeUpdater::waited_for`]), which the
/// first ask starts and a restart of the job does not restart. A frozen window therefore never
/// holds an update for good: past the bound the job installs beside it, as it does beside a window
/// that said it no longer holds. A look with no waiting build has nothing to hold, and holds. Each
/// change of what a window does is said once in this run's journal.
fn silent_window_holds(updater: &dyn NodeUpdater, pid: u32, waiting: Option<u64>) -> bool {
    let waited = waiting.map(|build| updater.waited_for(build));
    let holds = waited.is_none_or(|waited| waited < WINDOW_HOLDS_AN_UPDATE_WITHIN);
    let said = SILENT_WINDOWS_SAID.lock_or_recover().insert(pid, holds);
    if said != Some(holds) {
        let bound = WINDOW_HOLDS_AN_UPDATE_WITHIN.as_secs() / 60;
        if holds {
            aterm_log::info!(
                "aterm node serve: window pid {pid} is alive but its relay does not answer; it \
                 holds this Mac's updates as a window that applies them, for at most {bound} min \
                 of a waiting build's wait here"
            );
        } else {
            aterm_log::warn!(
                "aterm node serve: window pid {pid} is alive but its relay has not answered while \
                 build {} waited here {} min, past its {bound} min hold: the job installs it",
                waiting.unwrap_or(0),
                waited.unwrap_or_default().as_secs() / 60
            );
        }
    }
    holds
}

/// Whether any process's relay answers here, whether or not it applies updates: the node health
/// check asks it of `node-port.lock` held elsewhere ([`checkpoint_idle`]), where an answer is all it
/// needs. A relay file nothing listens on is no answer. It never asks WindowServer.
fn a_window_relay_answers(relay_dir: &Path) -> bool {
    window_relays(relay_dir)
        .into_iter()
        .any(|(_, relay)| relay_answers(&relay))
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
/// boot apply runs with `[update] auto_apply` on and no window here that holds it
/// ([`a_window_holds_the_update`]: a window that says it applies it applies its own, and one whose
/// process lives but whose relay does not answer holds it as long), whatever this job's look at
/// the stage finds, since the apply reads the stage itself. A window that does not answer holds
/// only the apply: it can neither launch nor confirm a trial, so a start beside it still counts and
/// recovers this build's armed headless trial, and a build that crash-loops under launchd beside a
/// frozen window is still reverted. With
/// automatic installation off, or a setting that could not be read, a windowless start only counts and
/// recovers an already-armed trial, so a build that crash-loops under launchd is still reverted and the
/// restored build re-execed (`ImageReplacement::Replaceable`) without admitting a stage: Install, or a
/// later look, owns that decision. A boot that applies a stage never returns. Its apply leaves
/// staged the exact bytes a window here saw fail to start, for a window to try, arms nothing for
/// them, and installs nothing else in their place; [`NodeUpdater::newer_build`] names no such
/// stage, so this start logs no failure to run it.
pub(crate) fn apply_at_node_boot(updater: &dyn NodeUpdater, relay_dir: &Path) -> NodeBoot {
    let started = updater.now();
    let newer = updater.newer_build();
    let held = if let Some(holder) = a_window_holds_the_update(updater, relay_dir, newer) {
        if holder == UpdateHolder::Silent && updater.trial_pending() {
            let recovery = updater.recover_trial_at_boot();
            aterm_log::info!(
                "aterm node serve: boot trial recovery beside a window that does not answer: \
                 {recovery}"
            );
        }
        Some("a window runs here, which applies it")
    } else if !updater.auto_apply() {
        let recovery = updater.recover_trial_at_boot();
        aterm_log::info!("aterm node serve: boot trial recovery with auto-apply off: {recovery}");
        Some(
            "[update] auto_apply is off or could not be read, so Install or a later look applies it",
        )
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
/// `[update] auto_apply` is on, no window here holds it ([`a_window_holds_the_update`]), no trial
/// is active, and this run's boot did not already find that build and fail to run it within
/// [`RETRY_LEFT_AFTER`] of its start. A start during a trial runs nothing newer, since an armed
/// trial applies nothing, so the build waits for the trial to settle (the run's own looks confirm
/// it, or end the run on misses that last), however long: a restart while it stands would only
/// end the job's service, since each run's deliberate end gives its launch back
/// ([`NodeUpdater::forgive_launch`]) and the next start would meet the same armed trial. Each
/// question is asked only once those before it say go, so the relays are dialled last. A stage
/// of bytes a window here saw fail to start is no newer build ([`NodeUpdater::newer_build`]), so
/// no run ends for a start that would leave them.
fn restart_for(updater: &dyn NodeUpdater, boot: NodeBoot, relay_dir: &Path) -> Option<u64> {
    let build = updater.newer_build()?;
    let left_within = updater.now().saturating_duration_since(boot.started) < RETRY_LEFT_AFTER
        && boot.left.is_some_and(|left| left >= build);
    if left_within || updater.trial_pending() {
        return None;
    }
    (updater.auto_apply() && a_window_holds_the_update(updater, relay_dir, Some(build)).is_none())
        .then_some(build)
}

/// The install over an activation the window here failed to switch to, with `[update] auto_apply`
/// on, window or not: that window applies nothing newer while it waits on an activation it cannot
/// reach, so a newer build would otherwise wait for it for good. That the window then takes the
/// newer build from the install path is read in its code, not tested
/// ([`aterm_update::install_over_failed_activation`]). It waits for this build's own trial to
/// settle, as a changed setting does: the install would end a run whose listener has not missed.
/// The words the run ends with where the install path changed, else `None`.
fn install_over_a_failed_activation(updater: &dyn NodeUpdater) -> Option<String> {
    let waiting = updater.failed_activation()?;
    if !updater.auto_apply() || updater.trial_pending() {
        return None;
    }
    updater.install_over_failed_activation(waiting)
}

/// Why a node run ends now, so that its next start runs as it should (§4.13), or `None`. An
/// older build put back in this copy's place goes first, window or not: this job serves a build
/// that was reverted, or that no longer stands here. Then a changed `[update] enabled`, which
/// this run's checker read once: Settings tells the person a change takes effect at the next
/// launch, and only a restart is this job's next launch. It waits for a trial to settle, since that
/// start would count against a build whose listener has not missed. Then a newer stage installed
/// over an activation the window here failed to switch to ([`install_over_a_failed_activation`]),
/// window or not, once no trial is active. Then [`restart_for`]'s newer build. Each of these ends
/// is deliberate, so the look gives back this run's trial launch first ([`spawn_node_look`]).
fn why_the_run_ends(updater: &dyn NodeUpdater, boot: NodeBoot, relay_dir: &Path) -> Option<String> {
    if let Some(build) = updater.older_build_installed() {
        return Some(format!(
            "build {build}, older than this one, stands in this copy's place (a crash-loop revert \
             put it back, or it was installed over this one): this run ends so launchd starts the \
             job again, and that start runs build {build}"
        ));
    }
    if updater.checks_setting_moved() && !updater.trial_pending() {
        return Some(
            "automatic update checks ([update] enabled) changed since this run started, which \
             reads them once: this run ends so launchd starts the job again under the saved \
             setting"
                .to_owned(),
        );
    }
    if let Some(why) = install_over_a_failed_activation(updater) {
        return Some(why);
    }
    restart_for(updater, boot, relay_dir).map(|build| {
        format!(
            "build {build} waits here (staged, or installed over this image) and no window here \
             applies it: this run ends so launchd starts the job again, and that start runs build \
             {build}, or counts against this build's trial if one still stands"
        )
    })
}

/// Start `updates`' checker and its look (§4.13): at each notice from the checker, and every
/// [`NodeUpdater::look_every`], the look minds the checker ([`the_checker_ends_the_run`]) and
/// [`why_the_run_ends`] decides. Once either names a reason the run ends through
/// [`NodeUpdater::restart`] and the look ends; it ends too once `stop` is set. A look that panics
/// costs that look alone, and one that could not start stays owed
/// ([`NodeUpdates::start_owed_look`]).
fn spawn_node_updates(updates: &NodeUpdates, relay_dir: PathBuf, stop: Arc<Stop>) {
    let (noticed, notices) = std::sync::mpsc::sync_channel::<()>(1);
    updates.updater.start_checker(Box::new(move || {
        let _ = noticed.try_send(());
    }));
    if !spawn_node_look(updates, relay_dir, stop, notices) {
        updates.look_owed.store(true, Ordering::Release);
    }
}

/// Start the look [`spawn_node_updates`] describes, on `notices` and the clock. One that cannot start
/// (this process is out of threads) ends no run, since a run ended here would count a launch against
/// a boot trial each time and revert a good build. It says so and returns `false`.
fn spawn_node_look(
    updates: &NodeUpdates,
    relay_dir: PathBuf,
    stop: Arc<Stop>,
    notices: std::sync::mpsc::Receiver<()>,
) -> bool {
    let updater = Arc::clone(&updates.updater);
    let boot = updates.boot;
    let spawned = updates.updater.spawn_look(Box::new(move || {
        let every = updater.look_every();
        let mut checker_owes = None;
        while !stop.is_set() {
            let decided = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                the_checker_ends_the_run(&*updater, &mut checker_owes)
                    .or_else(|| why_the_run_ends(&*updater, boot, &relay_dir))
            }));
            match decided {
                Ok(Some(why)) => {
                    // A deliberate end, no crash: only a crash and a failed trial count.
                    updater.forgive_launch();
                    updater.restart(&why);
                    return;
                }
                Ok(None) => {}
                Err(_) => aterm_log::warn!(
                    "aterm node serve: the updater's look panicked; it looks again within {} s",
                    every.as_secs()
                ),
            }
            if notices.recv_timeout(every) == Err(std::sync::mpsc::RecvTimeoutError::Disconnected) {
                // The checker let its notices go (automatic checks are off): look on the
                // clock alone, for a build another process staged or installed.
                stop.sleep(every);
            }
        }
    }));
    if let Err(err) = &spawned {
        aterm_log::warn!(
            "aterm node serve: the updater's look could not start ({err}); the job asks again"
        );
    }
    spawned.is_ok()
}

/// What the checker's watchdog says ends this run, once no trial is active: the next start would count
/// against a build whose listener has not missed, and three such starts revert it. The watchdog says it
/// once for a generation, so `owes` keeps it for the look after the trial settles. A replacement it
/// could start has started by then, whatever the trial.
fn the_checker_ends_the_run(
    updater: &dyn NodeUpdater,
    owes: &mut Option<String>,
) -> Option<String> {
    if let Some(why) = updater.mind_checker() {
        *owes = Some(why);
    }
    if owes.is_some() && updater.trial_pending() {
        return None;
    }
    owes.take()
}

/// How long a run [`NodeUpdater::restart`] asked to end may take to return before the process ends
/// itself, once a look has decided. The job's main loop and its watch thread read the stop far
/// sooner: only a read a volume or a dialog never answers, or a thread that died, takes this long.
#[cfg(target_os = "macos")]
const RUN_ENDS_WITHIN: Duration = Duration::from_secs(5 * 60);

/// How long a look of the updater's may stay out. A slow file read under a loaded Mac stays well
/// inside it, and only a file that does not answer goes past it: the look goes on with its fallback,
/// and the next asks again once that read has returned.
#[cfg(target_os = "macos")]
const LOOK_ANSWERS_WITHIN: Duration = Duration::from_secs(10 * 60);

/// The helper threads the looks at a waiting or an older build run on; while one stays out the next
/// is refused at once.
#[cfg(target_os = "macos")]
static STAGE_LOOKS: aterm_uds::deadline::Gate =
    aterm_uds::deadline::Gate::new("aterm-node-stage-look");

/// The helper threads the reads of `[update]` run on, apart from [`STAGE_LOOKS`]: a stuck look at
/// the updates root says nothing of a file on another volume.
#[cfg(target_os = "macos")]
static AUTO_APPLY_LOOKS: aterm_uds::deadline::Gate =
    aterm_uds::deadline::Gate::new("aterm-node-auto-apply-look");

/// `read` on a helper thread of `gate`, or `fallback` once it has been out for `within`, so a file
/// that never answers (a followed symlink onto a volume that stopped, a dialog nobody answers) costs
/// one bound and not the rest of the job's run.
#[cfg(any(target_os = "macos", test))]
fn look_within<T: Send + 'static>(
    gate: &'static aterm_uds::deadline::Gate,
    within: Duration,
    what: &str,
    fallback: T,
    read: impl FnOnce() -> T + Send + 'static,
) -> T {
    gate.run(within, within, read).unwrap_or_else(|refused| {
        aterm_log::warn!(
            "aterm node serve: {what} did not answer within {} s ({refused:?}); this look goes on \
             without it",
            within.as_secs()
        );
        fallback
    })
}

/// `read` of `[update] auto_apply` by [`look_within`]. One that does not answer holds this look: a
/// person's `false` may be behind it, and installing over that is the worse mistake. Answering on
/// here also installed nothing a person wanted, since the swap's budget had run out by then.
#[cfg(any(target_os = "macos", test))]
fn auto_apply_look(
    gate: &'static aterm_uds::deadline::Gate,
    within: Duration,
    read: impl FnOnce() -> bool + Send + 'static,
) -> bool {
    look_within(gate, within, "the read of [update] auto_apply", false, read)
}

/// Run `end` once `within` has passed, on a thread of its own, so that nothing the job is stuck in
/// can hold it. A thread that cannot start runs it at once.
#[cfg(any(target_os = "macos", test))]
fn end_run_within(within: Duration, end: fn()) {
    let spawned = std::thread::Builder::new()
        .name("aterm-node-run-end".to_owned())
        .spawn(move || {
            std::thread::sleep(within);
            end();
        });
    if spawned.is_err() {
        end();
    }
}

/// End this process, for launchd's `KeepAlive` to start the job again. The line goes on a thread of
/// its own, since a log on a volume that stopped answering must not hold the end.
#[cfg(target_os = "macos")]
fn end_stuck_run() {
    let _ = std::thread::Builder::new()
        .name("aterm-node-run-end-log".to_owned())
        .spawn(|| {
            aterm_log::warn!(
                "aterm node serve: this run did not return within {} s of being asked to end; it \
                 ends here so launchd starts the job again",
                RUN_ENDS_WITHIN.as_secs()
            );
        });
    std::thread::sleep(Duration::from_millis(250));
    aterm_uds::process::kill_self_now();
}

/// Ends a serving generation with its watch thread, however that thread ends. The serve loop reads
/// only `listener_stop`, and only this thread turns `stop` into it, so a panic in a look would
/// otherwise leave the generation serving for good, with `stop` unread.
struct EndsTheGeneration(Arc<Stop>);

impl Drop for EndsTheGeneration {
    fn drop(&mut self) {
        self.0.set();
    }
}

/// Production's [`NodeUpdater`]: aterm-update's checker and boot apply for this installed copy,
/// whose run ends by setting `stop` (`run_node_serve` then returns, the process exits, and
/// launchd's `KeepAlive` starts the job again).
#[cfg(target_os = "macos")]
struct LiveNodeUpdater {
    stop: Arc<Stop>,
    /// What the job's checker watchdog remembers between looks ([`mind_node_checker`]).
    checker_watch: Mutex<crate::update_checker_watch::CheckerWatchState>,
    /// What this run's installs over a failed activation held for.
    failed_activation_holds: Arc<Mutex<FailedActivationHolds>>,
    /// The waiting build this run first asked the wait of, and when ([`waited_or_since`]).
    first_asked: Mutex<Option<(u64, Instant)>>,
}

/// How long `build` has waited: `clock`'s answer (the pending clock, which outlives this run), else
/// the time since this run first asked for `build`, which `first` keeps (a newer build starts it
/// again). Every ask records the first, so a clock that stops answering still bounds from then.
#[cfg(any(target_os = "macos", test))]
fn waited_or_since(
    clock: Option<Duration>,
    first: &Mutex<Option<(u64, Instant)>>,
    build: u64,
    now: Instant,
) -> Duration {
    let mut first = first.lock().unwrap_or_else(PoisonError::into_inner);
    let since = match *first {
        Some((asked, at)) if asked == build => at,
        _ => {
            *first = Some((build, now));
            now
        }
    };
    clock.unwrap_or_else(|| now.saturating_duration_since(since))
}

/// What a node run remembers of its installs over a failed activation
/// ([`NodeUpdater::install_over_failed_activation`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default, PartialEq, Eq)]
struct FailedActivationHolds {
    /// Why the last one held, said once until it changes.
    said: Option<String>,
    /// The `(activation, newer)` the verified bundles refused
    /// ([`aterm_update::FailedActivationInstall::Refused`]): only a change to those bundles lets
    /// it through, so this run does not verify them again each look. A hold for now, or one read
    /// from small files before any verification, is never kept.
    settled: Option<(u64, u64)>,
}

/// What a node run says of an install over the failed activation `waiting`
/// ([`NodeUpdater::install_over_failed_activation`]): the words it ends with where the install
/// path changed, else `None`, having said once, in `holds`, why the activation stays.
#[cfg(any(target_os = "macos", test))]
fn failed_activation_words(
    waiting: (u64, u64),
    outcome: aterm_update::FailedActivationInstall,
    holds: &Mutex<FailedActivationHolds>,
) -> Option<String> {
    use aterm_update::FailedActivationInstall::{
        Held, Installed, NoneFailed, Refused, RetiredOnly,
    };
    let mut holds = holds.lock().unwrap_or_else(PoisonError::into_inner);
    if !matches!(outcome, Held(_) | Refused(_)) {
        *holds = FailedActivationHolds::default();
    }
    let (why, refused) = match outcome {
        Held(refusal) => (refusal.reason().to_owned(), false),
        Refused(why) => (why, true),
        NoneFailed => return None,
        Installed {
            retired,
            installed_build,
        } => {
            return Some(format!(
                "a window here failed to switch to the installed build {}, so this run retired \
                 it and installed build {installed_build} over build {}, which stays at the fixed \
                 rollback path: this run ends so launchd starts the job again, and that start \
                 runs build {installed_build}",
                retired.activation_build, retired.restored_build
            ));
        }
        RetiredOnly { retired, why } => {
            aterm_log::warn!(
                "aterm node serve: retired the installed build {}, which a window here failed to \
                 switch to, and build {} is back at the install path, but {why}",
                retired.activation_build,
                retired.restored_build
            );
            return Some(format!(
                "the installed build {} was retired and build {} put back in its place: this run \
                 ends so launchd starts the job again, and that start runs what the install path \
                 holds",
                retired.activation_build, retired.restored_build
            ));
        }
    };
    if holds.said.as_deref() != Some(why.as_str()) {
        aterm_log::info!(
            "aterm node serve: a window here failed to switch to the installed build {}, and \
             build {} waits: {why}",
            waiting.0,
            waiting.1
        );
        holds.said = Some(why);
    }
    holds.settled = refused.then_some(waiting);
    None
}

/// The older installed build a node run ends for ([`NodeUpdater::older_build_installed`]):
/// `older`, only once `image_replaced` reads that this job's image is no longer the installed
/// file. A bundle whose `Info.plist` names an older build than the executable it holds would
/// otherwise end every run at its first look, and launchd would start the job again into the
/// same image, again and again.
#[cfg(any(target_os = "macos", test))]
fn older_build_to_run(older: Option<u64>, image_replaced: impl FnOnce() -> bool) -> Option<u64> {
    older.filter(|_| image_replaced())
}

/// One look of the node job's checker watchdog at `beat`, at `now_secs` (§4.13): a stall the
/// window's watchdog would act on ([`crate::update_checker_watch::judge`], which confirms late
/// heartbeats and backs off known failed starts) is replaced through `respawn`, as the window
/// replaces its own, and logged. Why this run ends where no replacement can check: one stuck inside
/// its check holds the check lane, which a replacement in this process could not take until it lets
/// go, so none starts, and `respawn` may answer no (its budget is spent, or this process is out of
/// threads). Without that, a stuck check would mean no check here for as long as the job stayed up;
/// launchd's next start checks afresh.
#[cfg(any(target_os = "macos", test))]
fn mind_node_checker(
    watch: &mut crate::update_checker_watch::CheckerWatchState,
    beat: Option<aterm_update::checker_watch::CheckerBeat>,
    now_secs: u64,
    respawn: impl FnOnce(u64) -> Option<u64>,
) -> Option<String> {
    let stall = crate::update_checker_watch::judge(watch, beat, now_secs)?;
    if !stall.holds_lane
        && let Some(next) = respawn(stall.generation)
    {
        aterm_log::warn!(
            "aterm node serve: the update checker stalled (generation {} has not stamped its \
             heartbeat for {} s, last in {}); started generation {next} in its place",
            stall.generation,
            stall.for_secs,
            stall.phase.as_str()
        );
        return None;
    }
    Some(format!(
        "the update check has not moved for {} s (last in {}) and no replacement can take it over: \
         this run ends so launchd starts the job again, and that start checks afresh",
        stall.for_secs,
        stall.phase.as_str()
    ))
}

#[cfg(target_os = "macos")]
impl NodeUpdater for LiveNodeUpdater {
    fn start_checker(&self, noticed: Box<dyn Fn() + Send>) {
        crate::network_moved::observe_for_updater();
        aterm_update::spawn_background_check_with_settings(
            crate::running_build_number(),
            Arc::new(crate::app_config::update_check_settings_setting),
            None,
            Some(Box::new(move |_build, _version| noticed())),
            None,
        );
    }

    fn newer_build(&self) -> Option<u64> {
        // An updates root that does not answer lands nothing in any process, so it finds no build.
        let build = crate::running_build_number();
        look_within(
            &STAGE_LOOKS,
            LOOK_ANSWERS_WITHIN,
            "the look at a waiting build",
            None,
            move || aterm_update::newer_build_for_a_start(build),
        )
    }

    fn auto_apply(&self) -> bool {
        auto_apply_look(&AUTO_APPLY_LOOKS, LOOK_ANSWERS_WITHIN, || {
            crate::app_config::update_check_settings_setting().is_some_and(|s| s.auto_apply)
        })
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

    fn recover_trial_at_boot(&self) -> String {
        let outcome = aterm_update::recover_boot_trial_exact(
            crate::running_build_number(),
            crate::build_info::git_commit(),
            aterm_update::TrialWitness::Headless,
        );
        format!("{outcome:?}")
    }

    fn confirm_trial(&self) -> bool {
        // The service is up: this is its checkpoint. A trial a window launched answers
        // `Pending` (no longer "nothing to retry"), so a later look asks again.
        aterm_update::confirm_boot_health_exact(
            crate::running_build_number(),
            crate::build_info::git_commit(),
            aterm_update::TrialWitness::Headless,
            true,
        )
        .opens_gate()
    }

    fn trial_pending(&self) -> bool {
        aterm_update::headless_boot_trial_pending(crate::running_build_number())
    }

    fn older_build_installed(&self) -> Option<u64> {
        // A bundle on a volume that does not answer finds no older build, as it finds no newer one.
        let build = crate::running_build_number();
        look_within(
            &STAGE_LOOKS,
            LOOK_ANSWERS_WITHIN,
            "the look at an older build",
            None,
            move || {
                older_build_to_run(aterm_update::older_build_installed(build), || {
                    std::env::current_exe().is_ok_and(|exe| {
                        aterm_agent::node_service::image_replaced(std::process::id(), &exe)
                    })
                })
            },
        )
    }

    fn checks_setting_moved(&self) -> bool {
        // A read with no answer is no change, now or at launch: the next look asks again.
        look_within(
            &AUTO_APPLY_LOOKS,
            LOOK_ANSWERS_WITHIN,
            "the look at [update] enabled",
            false,
            || {
                aterm_update::automatic_as_saved()
                    .zip(aterm_update::automatic_at_launch())
                    .is_some_and(|(saved, launch)| saved != launch)
            },
        )
    }

    fn failed_activation(&self) -> Option<(u64, u64)> {
        // An updates root that does not answer holds no failed activation, as it holds no stage.
        look_within(
            &STAGE_LOOKS,
            LOOK_ANSWERS_WITHIN,
            "the look at a failed activation",
            None,
            aterm_update::failed_activation_waiting,
        )
    }

    fn install_over_failed_activation(&self, waiting: (u64, u64)) -> Option<String> {
        let holds = Arc::clone(&self.failed_activation_holds);
        if holds.lock().unwrap_or_else(PoisonError::into_inner).settled == Some(waiting) {
            return None;
        }
        look_within(
            &STAGE_LOOKS,
            LOOK_ANSWERS_WITHIN,
            "the install over a failed activation",
            None,
            move || {
                failed_activation_words(
                    waiting,
                    aterm_update::install_over_failed_activation(
                        crate::running_build_number(),
                        crate::build_info::git_commit(),
                    ),
                    &holds,
                )
            },
        )
    }

    fn mind_checker(&self) -> Option<String> {
        aterm_update::start_owed_launch_waits();
        let mut watch = self
            .checker_watch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        mind_node_checker(
            &mut watch,
            aterm_update::checker_watch::checker_snapshot(),
            aterm_update::checker_watch::now_secs(),
            aterm_update::respawn_stalled_checker,
        )
    }

    fn note_trial_miss(&self, why: &str) {
        if !aterm_update::note_trial_miss(crate::running_build_number(), why) {
            aterm_log::info!("aterm node serve: the update record noted no trial miss: {why}");
        }
    }

    fn restart(&self, why: &str) {
        // The stop and its deadline first: a log on a volume that stopped answering holds neither.
        self.stop.set();
        end_run_within(RUN_ENDS_WITHIN, end_stuck_run);
        aterm_log::info!("aterm node serve: {why}");
    }

    fn forgive_launch(&self) {
        forgive_this_runs_launch();
    }

    fn waited_for(&self, build: u64) -> Duration {
        let clock = look_within(
            &STAGE_LOOKS,
            LOOK_ANSWERS_WITHIN,
            "the look at how long a build has waited",
            None,
            move || aterm_update::pending_update_waited(crate::running_build_number(), build),
        );
        waited_or_since(clock, &self.first_asked, build, Instant::now())
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Whether this `aterm node serve` stages and applies no update ([`live_node_updater`]) and keeps
/// no account claims ([`run_node_serve`]): a run told it is on scratch state or a CI runner
/// (`ATERM_STATE_HOME` or `CI` set, read raw, so a release build, which does not move its state
/// for `ATERM_STATE_HOME`, honours both), so no CI runner ever updates. It decides no vault read:
/// which vault the job reads follows where its state root resolved ([`super::vault_here`]).
fn node_run_updates_and_claims_nothing() -> bool {
    std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some()
}

/// The socket directory holding this Mac's `wf-<pid>.sock` window relays.
fn node_relay_dir(state_dir: &Path) -> PathBuf {
    aterm_uds::control_socket_dir().unwrap_or_else(|| state_dir.to_path_buf())
}

/// This Mac's updater for a run that `stop` ends: production's on macOS, and none on scratch
/// state (a lab instance, a test) or a CI runner ([`node_run_updates_and_claims_nothing`]) or on
/// another platform.
fn live_node_updater(stop: &Arc<Stop>) -> Option<Arc<dyn NodeUpdater>> {
    if node_run_updates_and_claims_nothing() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(LiveNodeUpdater {
            stop: Arc::clone(stop),
            checker_watch: Mutex::new(crate::update_checker_watch::CheckerWatchState::default()),
            failed_activation_holds: Arc::new(Mutex::new(FailedActivationHolds::default())),
            first_asked: Mutex::new(None),
        }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = stop;
        None
    }
}

/// [`apply_at_node_boot`] for `aterm node serve` (launchd's `systems.alab.aterm-node`), first
/// thing once the job's lock is held ([`run_node_serve_owning`]), with this Mac's updater
/// ([`live_node_updater`]).
fn apply_node_update_at_boot() -> NodeBoot {
    // A boot asks no restart: no run has started for one to end.
    let no_run = Arc::new(Stop::new());
    match (live_node_updater(&no_run), aterm_types::dirs::state_dir()) {
        (Some(updater), Some(state_dir)) => {
            apply_at_node_boot(&*updater, &node_relay_dir(&state_dir))
        }
        _ => NodeBoot::nothing_at(Instant::now()),
    }
}

/// A test child's stand-in for the root LAN broker's answer, which [`run_node_serve`] gates its
/// LAN work on ([`node_lan_gate`]): a test binary has no broker of its own.
#[cfg(test)]
pub(crate) static NODE_LAN_GATE_FOR_TEST: std::sync::OnceLock<fn() -> Option<String>> =
    std::sync::OnceLock::new();

/// The gate [`run_node_serve`] asks before its LAN work: the root LAN broker's answer
/// ([`crate::network_moved::lan_unavailable`]), or a test child's stand-in.
fn node_lan_gate() -> fn() -> Option<String> {
    #[cfg(test)]
    if let Some(gate) = NODE_LAN_GATE_FOR_TEST.get() {
        return *gate;
    }
    crate::network_moved::lan_unavailable
}

/// Local nonblocking upkeep retained by the node's owned runtime. Both idle
/// rounds and the serving generation's existing watch invoke it.
pub type NodeMaintenance = Arc<dyn Fn() + Send + Sync>;

/// Foreground entry point for `aterm node serve` (`systems.alab.aterm-node`, C20–C23,
/// §4.13): the job's lock first ([`own_node_job`]), then its boot apply
/// ([`apply_node_update_at_boot`]), then `startup` (the job's Commons runtime), then the run. A start that finds the lock held is a second instance and
/// does none of it, so it neither swaps a stage in nor counts a start against the boot trial of
/// a build another instance runs, and starts no service beside the one that serves.
#[must_use = "return or inspect the node service's exit status"]
pub fn run_node_serve_owning(startup: impl FnOnce() -> Option<NodeMaintenance>) -> ExitCode {
    let Some(state_dir) = aterm_types::dirs::state_dir() else {
        eprintln!("aterm node serve: state directory is unavailable");
        return ExitCode::FAILURE;
    };
    own_node_job(
        &state_dir,
        apply_node_update_at_boot,
        startup,
        |boot, job_lock, maintenance| {
            run_node_serve(state_dir.clone(), boot, job_lock, maintenance)
        },
    )
}

/// Take `<state_dir>/node-job.lock`, then run `boot`, `startup` and `serve` under it, in that
/// order. A lock another instance holds ends the start at once, with none of them run, as a
/// second instance. A lock that cannot be opened does too, and fails the start: it cannot rule out
/// a second instance, and a file's mode or owner says nothing of a build, so no boot swaps or counts
/// a launch against its trial for it. The lock is close-on-exec, so a boot that execs the build it
/// installed lets it go, and that build's start takes it again before it counts or confirms its
/// trial.
fn own_node_job<S>(
    state_dir: &Path,
    boot: impl FnOnce() -> NodeBoot,
    startup: impl FnOnce() -> S,
    serve: impl FnOnce(NodeBoot, NodeJobLock, S) -> ExitCode,
) -> ExitCode {
    match take_node_job_lock(state_dir) {
        JobLock::Taken(job_lock) => {
            let boot = boot();
            let maintenance = startup();
            serve(boot, job_lock, maintenance)
        }
        JobLock::HeldElsewhere => ExitCode::SUCCESS,
        JobLock::Unopenable => ExitCode::FAILURE,
    }
}

/// What a start found of `<state_dir>/node-job.lock` ([`take_node_job_lock`]).
enum JobLock {
    /// This start took it.
    Taken(NodeJobLock),
    /// Another instance holds it.
    HeldElsewhere,
    /// It could not be opened or taken (said on stderr).
    Unopenable,
}

/// Take `<state_dir>/node-job.lock`, saying why where another instance holds it or it cannot be
/// opened.
fn take_node_job_lock(state_dir: &Path) -> JobLock {
    match NodeJobLock::try_acquire(state_dir) {
        Ok(Some(job_lock)) => JobLock::Taken(job_lock),
        Ok(None) => {
            aterm_log::info!(
                "aterm node serve: another instance already holds {}/{}",
                state_dir.display(),
                aterm_agent::node_service::NODE_JOB_LOCK
            );
            JobLock::HeldElsewhere
        }
        Err(err) => {
            eprintln!(
                "aterm node serve: cannot acquire {}/{}: {err}",
                state_dir.display(),
                aterm_agent::node_service::NODE_JOB_LOCK
            );
            JobLock::Unopenable
        }
    }
}

/// The run of `aterm node serve` on `state_dir` that holds `job_lock`, after its boot found
/// `boot`.
fn run_node_serve(
    state_dir: PathBuf,
    boot: NodeBoot,
    job_lock: NodeJobLock,
    maintenance: Option<NodeMaintenance>,
) -> ExitCode {
    crate::descriptor_limit::raise_descriptor_limit_for_node();
    let relay_dir = node_relay_dir(&state_dir);
    let home_dir = aterm_types::dirs::home_dir();
    let config_path = crate::app_config::config_path();
    // A job that cannot read its deny list must not serve as though the list named no one.
    let vault_paths = match super::vault_here() {
        Ok(paths) => paths,
        Err(why) => {
            eprintln!("aterm node serve: {why}, so this job does not serve");
            return ExitCode::FAILURE;
        }
    };
    let members = vault_paths
        .as_ref()
        .map(|paths| Arc::new(aterm_agent::fleet_log::LocalLog::of(paths)));
    let stop = Arc::new(Stop::new());
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
        on_generation: None,
        tick_every: retry(),
        wake_timeout: DEFAULT_WAKE_TIMEOUT,
        stop,
        launcher: Arc::new(LiveWindowLauncher::default()),
        lan_unavailable: node_lan_gate(),
        updates,
        mesh: NodeMesh::live(),
    };
    crate::board_keeper::start(cfg.state_dir.clone());
    if !node_run_updates_and_claims_nothing() {
        spawn_claims_upkeep(cfg.relay_dir.clone(), Arc::clone(&cfg.stop));
        arm_claims_off_on_quit(
            cfg.relay_dir.clone(),
            cfg.state_dir.clone(),
            crate::harness_host::claims_off_as_the_node_job_stops,
        );
    }
    serve_node_job(cfg, job_lock, maintenance)
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
/// `off` (production passes `harness_host::claims_off_as_the_node_job_stops`), the host's quit
/// step (`crate::host_quit::run`, bounded by its host: the job's Commons runtime stops and
/// closes the network history), and `state_dir`'s fleet times this job noted and holds
/// unwritten ([`write_seen_at_quit`]), then this run's trial launch given back
/// ([`forgive_this_runs_launch`]); then the job dies of that signal as it did before
/// (`crate::owned_endpoint::arm_orderly_quit`; a second signal ends it at once). This is the
/// job's usual end: launchd stops it with SIGTERM, and its serve loop returns only for an
/// update's restart.
fn arm_claims_off_on_quit(relay_dir: PathBuf, state_dir: PathBuf, off: fn()) {
    if let Err(err) = crate::owned_endpoint::arm_orderly_quit(move |_sig| {
        claims_off_on_quit(&relay_dir, &off);
        crate::host_quit::run();
        write_seen_at_quit(&state_dir);
        // A stop asked of the job is no crash: its launch does not count against a trial. Last,
        // since the give-back may wait on an install's apply lock, and the claims' release and
        // the seen times must not spend launchd's exit allowance on it.
        forgive_this_runs_launch();
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

/// Give back the launch this run of the job counted against its build's boot trial, if it counted
/// one, as a run ends by choice (an update's restart, a terminating signal). It says what it did
/// where it gave one back.
fn forgive_this_runs_launch() {
    if aterm_update::forgive_own_trial_launch(crate::running_build_number()) {
        aterm_log::info!(
            "aterm node serve: this run ends by choice, so its launch no longer counts against \
             its build's boot trial"
        );
    }
}

/// Run [`claims_upkeep`] now and every `claims::RECHECK_S` until `stop`.
fn spawn_claims_upkeep(relay_dir: PathBuf, stop: Arc<Stop>) {
    let every = Duration::from_secs(aterm_agent::harness::claims::RECHECK_S.unsigned_abs());
    let spawned = std::thread::Builder::new()
        .name("aterm-node-claims".to_owned())
        .spawn(move || {
            while !stop.is_set() {
                claims_upkeep(&relay_dir, &crate::harness_host::claims_look);
                stop.sleep(every);
            }
        });
    if let Err(err) = spawned {
        aterm_log::warn!(
            "aterm node serve: the account claims' look could not start: {err}; with no window \
             open this Mac's claims stand until their rows lapse"
        );
    }
}

/// Run the node job supervision loop with explicit paths and seams, once it takes the job's
/// lock ([`take_node_job_lock`]); production starts through [`run_node_serve_owning`].
#[cfg(test)]
#[must_use = "return or inspect the configured node service's exit status"]
pub(crate) fn run_node_serve_with(cfg: NodeServeConfig) -> ExitCode {
    match take_node_job_lock(&cfg.state_dir) {
        JobLock::Taken(job_lock) => serve_node_job(cfg, job_lock, None),
        JobLock::HeldElsewhere => ExitCode::SUCCESS,
        JobLock::Unopenable => ExitCode::FAILURE,
    }
}

/// The node job supervision loop of the instance that holds `job_lock`, which it keeps until it
/// returns.
fn serve_node_job(
    mut cfg: NodeServeConfig,
    job_lock: NodeJobLock,
    maintenance: Option<NodeMaintenance>,
) -> ExitCode {
    if !cfg!(test) {
        crate::qos::set_self(crate::qos::Role::Background);
    }
    let _held_lock = job_lock;
    // Local upkeep retires before the job relinquishes its exclusive lifetime.
    let maintenance = maintenance;
    if !cfg.stop.is_set() {
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
    let mut waits_for_broker = false;
    // The fabric's bus.log, hosted here with the mesh on (C2), whatever the generations do:
    // local clients reach it with no fleet key, no LAN broker and no node key. Held
    // before the mesh logs, so it closes after them.
    let bus_keeper = cfg.mesh.keep_bus();
    let mut mesh_logs = MeshHeld::default();
    mesh_logs.bus = bus_keeper.as_ref().map(|keeper| Arc::clone(keeper.slot()));

    while !cfg.stop.is_set() {
        if let Some(updates) = cfg.updates.as_ref() {
            updates.start_owed_look(&cfg.relay_dir, &cfg.stop);
        }
        if let Some(maintain) = &maintenance {
            maintain();
        }
        if fleet_turned_off_or_swarm_disabled(&cfg) {
            advertiser.stop();
            port_lock = None;
            cfg.stop.sleep(cfg.tick_every);
            checkpoint_idle(&cfg, IdleCheckpoint::Waiting);
            continue;
        }
        let Some(wanted) = wanted_for_node(&cfg) else {
            advertiser.stop();
            port_lock = None;
            cfg.stop.sleep(cfg.tick_every);
            checkpoint_idle(&cfg, IdleCheckpoint::Waiting);
            continue;
        };
        let Some(traffic) =
            aterm_agent::fleet_vault::Traffic::capture(&cfg.state_dir, cfg.vault_paths.as_ref())
        else {
            advertiser.stop();
            port_lock = None;
            cfg.stop.sleep(cfg.tick_every);
            checkpoint_idle(&cfg, IdleCheckpoint::Waiting);
            continue;
        };
        let secret = match aterm_net::fleet::read_key_file(&cfg.state_dir) {
            Ok(secret) => secret,
            Err(_) => {
                advertiser.stop();
                port_lock = None;
                cfg.stop.sleep(cfg.tick_every);
                checkpoint_idle(&cfg, IdleCheckpoint::Waiting);
                continue;
            }
        };
        // Asked once a generation is due (the job is wanted and holds the fleet's key), right
        // before it advertises, listens and dials.
        if let Some(why) = (cfg.lan_unavailable)() {
            if !std::mem::replace(&mut waits_for_broker, true) {
                aterm_log::info!(
                    "aterm node serve: waits for the LAN broker before it advertises, listens or \
                     dials on the local network: {why}"
                );
            }
            advertiser.stop();
            port_lock = None;
            cfg.stop.sleep(cfg.tick_every);
            checkpoint_idle(&cfg, IdleCheckpoint::Waiting);
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
                cfg.stop.sleep(cfg.tick_every);
                checkpoint_idle(
                    &cfg,
                    IdleCheckpoint::Failed("node listen address is invalid"),
                );
                continue;
            };
            if aterm_net::loopback::check(aterm_net::loopback::Reach::Listen(addr)).is_err() {
                advertiser.stop();
                port_lock = None;
                cfg.stop.sleep(cfg.tick_every);
                checkpoint_idle(
                    &cfg,
                    IdleCheckpoint::Failed("node listen address is refused"),
                );
                continue;
            }
            if port_lock.is_none() {
                match NodeJobLock::try_acquire_port(&cfg.state_dir) {
                    Ok(Some(held)) => port_lock = Some(held),
                    Ok(None) => {
                        cfg.stop.sleep(cfg.tick_every);
                        checkpoint_idle(&cfg, IdleCheckpoint::PortOwnedElsewhere);
                        continue;
                    }
                    Err(_) => {
                        cfg.stop.sleep(cfg.tick_every);
                        checkpoint_idle(
                            &cfg,
                            IdleCheckpoint::Failed("node port custody is unreadable"),
                        );
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
                    cfg.stop.sleep(cfg.tick_every);
                    checkpoint_idle(&cfg, IdleCheckpoint::Failed("node listener could not bind"));
                    continue;
                }
            }
        };

        let Ok(bound_addr) = listener.local_addr() else {
            port_lock = None;
            cfg.stop.sleep(cfg.tick_every);
            checkpoint_idle(
                &cfg,
                IdleCheckpoint::Failed("node bound address is unreadable"),
            );
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
            maintenance.as_ref(),
            traffic,
        ) {
            port_lock = None;
            checkpoint_idle(
                &cfg,
                IdleCheckpoint::Failed("node listener could not start"),
            );
        }
    }

    advertiser.stop();
    // The loop returned (an update's restart): the same write the job's terminating signal
    // makes ([`arm_claims_off_on_quit`]).
    write_seen_at_quit(&cfg.state_dir);
    ExitCode::SUCCESS
}

/// When members were last heard, where this job still holds it unwritten, goes to disk as the
/// job ends in order, by its serve loop's return or by a terminating signal, waiting at most a
/// second for the book's lock. A job that is killed, or that ends by a signal it could not arm
/// for, loses at most a minute of it.
fn write_seen_at_quit(state_dir: &Path) {
    let _ = aterm_net::fleet_member::write_seen_noted(state_dir, Duration::from_secs(1));
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
    let lan_unavailable = cfg.lan_unavailable;
    let now_ms = Arc::clone(&cfg.mesh.pass_now_ms);
    let start_converge = Arc::clone(&cfg.mesh.pass_converge);
    let pace = cfg.mesh.pass_pace;
    let rung = cfg.mesh.pass_rung.clone();
    let broker = cfg.mesh.pass_broker;
    std::thread::Builder::new()
        .name("node-vault-pass".into())
        .spawn(move || {
            let switches = rung
                .as_ref()
                .and_then(|rung| watch_pass_switches(&state_dir, config_path.as_deref(), rung));
            // A pass whose switches go unheard looks at its pace.
            let rung = rung.filter(|_| switches.is_some());
            aterm_secrets::client::run_node_pass(&aterm_secrets::client::NodePassHost {
                paths: &paths,
                stop: &stop,
                held: &|| sharing_held(&state_dir, config_path.as_deref()),
                may_post: &|| lan_unavailable().is_none(),
                routes: &aterm_secrets::server::route_sources,
                make_inputs: &|| {
                    // The observer rings the pass as an input it reads changes.
                    aterm_secrets::client::convergence_inputs(
                        &paths,
                        aterm_secrets::client::ConvergenceRole::Node,
                        rung.clone(),
                    )
                },
                start_converge: &*start_converge,
                now_ms: &*now_ms,
                log: &|line| aterm_log::info!("node: {line}"),
                pace,
                rung: rung.clone(),
                broker,
            });
        })
        .map_err(|error| aterm_log::warn!("node: the vault pass did not start: {error}"))
        .ok()
}

/// A watch over what [`sharing_held`] reads of `state_dir` and `config_path` (the fleet's off
/// switch and key, and `[swarm] enabled`), ringing `rung` as one changes, so the vault pass hears a
/// hold lift. `None` where it cannot start.
fn watch_pass_switches(
    state_dir: &Path,
    config_path: Option<&Path>,
    rung: &Arc<aterm_uds::ready::Doorbell>,
) -> Option<aterm_uds::file_watch::FileWatch> {
    let ringing = Arc::clone(rung);
    let watch = aterm_uds::file_watch::FileWatch::platform(
        "node-pass-switches",
        Box::new(move |_| ringing.ring()),
    )
    .map_err(|error| {
        aterm_log::warn!(
            "node: the vault pass cannot watch its switches, so it looks each second: {error}"
        );
    })
    .ok()?;
    watch.watch(0, state_dir.join(aterm_net::fleet_member::OFF_FILE));
    watch.watch(1, state_dir.join(aterm_net::fleet::KEY_FILE));
    if let Some(path) = config_path {
        watch.watch(2, path.to_path_buf());
    }
    Some(watch)
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
/// seat's standing read from the rows; it stops before they close. So do the needs reads (Q4,
/// §4.14), which hand every window's needs row to the installed [`WindowNeeds`], and the activity
/// reads, which hand the count of every window's agents to the installed [`WindowActivity`].
#[derive(Default)]
struct MeshHeld {
    mesh: Option<Arc<aterm_mesh::Mesh>>,
    presence: Option<aterm_mesh::presence::Presence>,
    /// The reads of this Mac's windows' needs ([`WindowNeedsReads`]).
    needs: Option<WindowNeedsReads>,
    /// The counts of this Mac's windows' agents ([`WindowActivityReads`]).
    activity: Option<WindowActivityReads>,
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
            self.activity = None;
            self.needs = None;
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
        if self.needs.is_none() && self.mesh.is_some() {
            match WindowNeedsReads::start(
                cfg.relay_dir.clone(),
                cfg.tick_every,
                installed_window_needs(),
            ) {
                Ok(needs) => self.needs = Some(needs),
                Err(e) => {
                    let why = format!("the needs reads did not start: {e}");
                    if self.said.as_deref() != Some(why.as_str()) {
                        aterm_log::warn!("node: {why}");
                        self.said = Some(why);
                    }
                }
            }
        }
        if self.activity.is_none() && self.mesh.is_some() {
            match WindowActivityReads::start(
                WindowRelays::new(cfg.relay_dir.clone(), Arc::clone(&cfg.launcher)),
                ACTIVITY_EVERY,
                installed_window_activity(),
            ) {
                Ok(activity) => self.activity = Some(activity),
                Err(e) => {
                    let why = format!("the activity reads did not start: {e}");
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
        self.activity = None;
        self.needs = None;
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

/// The cap on one needs frame's row, both ways: a window writes none longer, and the node job
/// refuses a length past it before it reads any of the row ([`RelayHeader::Needs`]).
pub(crate) const NEEDS_FRAME_MAX: usize = 64 * 1024;

/// How long a window that took no needs hand-over (a build that predates it) is left before the
/// node job asks it again.
const NEEDS_ASK_AGAIN: Duration = Duration::from_secs(60);

/// Write `row` to `out` as one needs frame: the length of its encoding as a big-endian `u32`,
/// then [`NeedsRow::encode`], flushed.
///
/// # Errors
/// `InvalidInput` for an encoding longer than [`NEEDS_FRAME_MAX`], which writes nothing; else the
/// write's own.
pub(crate) fn write_needs_frame(out: &mut impl Write, row: &NeedsRow) -> std::io::Result<()> {
    let body = row.encode();
    let len = u32::try_from(body.len())
        .ok()
        .filter(|_| body.len() <= NEEDS_FRAME_MAX)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a needs row longer than a frame holds",
            )
        })?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&body);
    out.write_all(&frame)?;
    out.flush()
}

/// Read one needs frame from `input` ([`write_needs_frame`]): its row, decoded strictly.
///
/// # Errors
/// `UnexpectedEof` where the stream ends, before a frame as within one; `InvalidData` for a
/// length past [`NEEDS_FRAME_MAX`], refused before any of the row is read, or for a row
/// [`NeedsRow::decode`] refuses, one with a trailing byte among them; else the read's own.
pub(crate) fn read_needs_frame(input: &mut impl Read) -> std::io::Result<NeedsRow> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len)?;
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
    if len > NEEDS_FRAME_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a needs frame longer than its cap",
        ));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    NeedsRow::decode(&body).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("a needs row: {error}"),
        )
    })
}

/// Every live window's latest needs row, by the window's pid, handed to the metrics publisher
/// each time one changes or a window goes (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14).
pub type WindowNeeds = Arc<dyn Fn(&BTreeMap<u32, NeedsRow>) + Send + Sync>;

/// The [`WindowNeeds`] this process installed ([`install_window_needs`]).
static WINDOW_NEEDS: OnceLock<WindowNeeds> = OnceLock::new();

/// Install `sink` as the node job's [`WindowNeeds`], which the job's needs reads hand each change
/// to while it holds the mesh. The first install stands: the answer says whether this call
/// installed `sink`. With none installed, the job keeps each window's row and calls nothing.
pub fn install_window_needs(sink: WindowNeeds) -> bool {
    WINDOW_NEEDS.set(sink).is_ok()
}

/// What the node job's needs reads hand each change to: the installed [`WindowNeeds`], looked up
/// at each change, so one installed after the reads began is handed the next.
fn installed_window_needs() -> WindowNeeds {
    Arc::new(|rows| {
        if let Some(sink) = WINDOW_NEEDS.get() {
            sink(rows);
        }
    })
}

/// Whether `a` and `b` say the same, the times they were written aside.
fn same_needs(a: &NeedsRow, b: &NeedsRow) -> bool {
    a.needs == b.needs && a.held == b.held && a.managers == b.managers
}

/// The node job's reads of its windows' needs (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14),
/// run while it holds the mesh ([`MeshHeld`]): one thread that, every tick, hands
/// [`RelayHeader::Needs`] to each live window it reads none from, and one reader per window that
/// keeps the window's latest row and hands its sink the whole map at each change, a window that
/// goes included, from the first map that holds every window the first look found
/// ([`NeedsReads::prime`]). Dropping it stops them: it hangs up on every window, joins every thread, and
/// hands nothing more on.
struct WindowNeedsReads {
    shared: Arc<NeedsReads>,
    dialer: Option<std::thread::JoinHandle<()>>,
}

/// What [`WindowNeedsReads`]' threads share.
struct NeedsReads {
    /// Set as the reads stop.
    stop: Stop,
    /// Each window's latest row, by pid: the map the sink is handed.
    rows: Mutex<BTreeMap<u32, NeedsRow>>,
    /// A copy of the stream each window is read on, by pid, to hang up on as the reads stop.
    streams: Mutex<BTreeMap<u32, CtlStream>>,
    /// Each window's reader, joined as the reads stop.
    readers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// What each change is handed to, called with [`Self::rows`] held, so the calls come in the
    /// order the changes did.
    sink: WindowNeeds,
    /// Set, with [`Self::rows`] held, once every window the first look opened wrote its first row
    /// or went, or [`HANDSHAKE`] passed ([`Self::prime`]): until then rows are kept and handed on
    /// to no one, so the sink's first map is every window's, not the first one read.
    primed: AtomicBool,
    /// The windows the first look opened that have neither written a row nor gone yet.
    first: Mutex<BTreeSet<u32>>,
    /// Rung as each window leaves [`Self::first`].
    first_left: std::sync::Condvar,
}

impl WindowNeedsReads {
    /// Read the needs of every live window whose relay socket is in `relay_dir`, looking for
    /// windows not yet read every `every`, and hand `sink` the whole map at each change.
    ///
    /// # Errors
    /// The spawn of the thread that looks for windows.
    fn start(relay_dir: PathBuf, every: Duration, sink: WindowNeeds) -> std::io::Result<Self> {
        let shared = Arc::new(NeedsReads {
            stop: Stop::new(),
            rows: Mutex::default(),
            streams: Mutex::default(),
            readers: Mutex::default(),
            sink,
            primed: AtomicBool::new(false),
            first: Mutex::default(),
            first_left: std::sync::Condvar::new(),
        });
        let dialer = std::thread::Builder::new()
            .name("aterm-node-needs".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || shared.dial_windows(&relay_dir, every)
            })?;
        Ok(Self {
            shared,
            dialer: Some(dialer),
        })
    }
}

impl Drop for WindowNeedsReads {
    fn drop(&mut self) {
        self.shared.stop.set();
        if let Some(dialer) = self.dialer.take() {
            let _ = dialer.join();
        }
        for stream in self.shared.streams.lock_or_recover().values() {
            aterm_uds::hangup::hang_up(stream);
        }
        let readers = std::mem::take(&mut *self.shared.readers.lock_or_recover());
        for reader in readers {
            let _ = reader.join();
        }
    }
}

impl NeedsReads {
    /// Until the reads stop, every `every`: open a read of each live window in `relay_dir` this
    /// holds none of, a window that refused one left [`NEEDS_ASK_AGAIN`] first.
    fn dial_windows(self: &Arc<Self>, relay_dir: &Path, every: Duration) {
        let mut refused: BTreeMap<u32, Instant> = BTreeMap::new();
        let mut looked = false;
        while !self.stop.is_set() {
            self.readers
                .lock_or_recover()
                .retain(|reader| !reader.is_finished());
            let windows = live_relay_candidates(relay_dir);
            refused.retain(|pid, _| windows.iter().any(|(live, _)| live == pid));
            for (pid, path) in windows {
                if self.stop.is_set() {
                    return;
                }
                if self.streams.lock_or_recover().contains_key(&pid)
                    || refused
                        .get(&pid)
                        .is_some_and(|until| Instant::now() < *until)
                {
                    continue;
                }
                match open_needs(&path) {
                    Some(stream) => {
                        if !looked {
                            self.first.lock_or_recover().insert(pid);
                        }
                        self.read_window(pid, stream);
                    }
                    None => {
                        refused.insert(pid, Instant::now() + NEEDS_ASK_AGAIN);
                    }
                }
            }
            if !looked {
                looked = true;
                self.prime();
            }
            self.stop.sleep(every);
        }
    }

    /// Read window `pid`'s needs on `stream` on a thread of its own, holding a copy of the stream
    /// to hang up on.
    fn read_window(self: &Arc<Self>, pid: u32, stream: CtlStream) {
        let Ok(copy) = aterm_uds::kept_out(|| stream.try_clone()) else {
            aterm_uds::hangup::hang_up(&stream);
            self.left_first(pid);
            return;
        };
        self.streams.lock_or_recover().insert(pid, copy);
        let shared = Arc::clone(self);
        match std::thread::Builder::new()
            .name("aterm-node-needs-read".to_owned())
            .spawn(move || shared.take_frames(pid, &stream))
        {
            Ok(reader) => self.readers.lock_or_recover().push(reader),
            Err(error) => {
                aterm_log::warn!("node: no needs read of window {pid}: {error}");
                if let Some(copy) = self.streams.lock_or_recover().remove(&pid) {
                    aterm_uds::hangup::hang_up(&copy);
                }
                self.left_first(pid);
            }
        }
    }

    /// Keep each row window `pid` writes on `stream`, handing the sink the map at each change,
    /// until the window hangs up, writes a frame [`read_needs_frame`] refuses, or the reads stop.
    /// Then forget its row, handing the sink the map without it where it held one and the reads
    /// did not stop, and let go of its stream, so the next look may read the window again.
    fn take_frames(&self, pid: u32, stream: &CtlStream) {
        let mut awake = aterm_uds::hangup::Awake(stream);
        while let Ok(row) = read_needs_frame(&mut awake) {
            if self.stop.is_set() {
                break;
            }
            let mut rows = self.rows.lock_or_recover();
            if rows.get(&pid).is_some_and(|held| same_needs(held, &row)) {
                continue;
            }
            rows.insert(pid, row);
            if self.primed.load(Ordering::Acquire) {
                (self.sink)(&rows);
            } else {
                drop(rows);
                self.left_first(pid);
            }
        }
        {
            let mut rows = self.rows.lock_or_recover();
            if rows.remove(&pid).is_some()
                && self.primed.load(Ordering::Acquire)
                && !self.stop.is_set()
            {
                (self.sink)(&rows);
            }
        }
        self.left_first(pid);
        aterm_uds::hangup::hang_up(stream);
        drop(self.streams.lock_or_recover().remove(&pid));
    }
}

impl NeedsReads {
    /// Wait, at most [`HANDSHAKE`], until every window the first look opened wrote its first row
    /// or went, then hand the sink the map once, an empty one included, so a row of windows gone
    /// before this node job started is cleared, and every change from then on.
    fn prime(&self) {
        let deadline = Instant::now() + HANDSHAKE;
        let mut first = self.first.lock_or_recover();
        while !first.is_empty() && !self.stop.is_set() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            first = self
                .first_left
                .wait_timeout(first, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        first.clear();
        drop(first);
        let rows = self.rows.lock_or_recover();
        self.primed.store(true, Ordering::Release);
        if !self.stop.is_set() {
            (self.sink)(&rows);
        }
    }

    /// Window `pid` wrote its first row or went: the first look waits on it no longer.
    fn left_first(&self, pid: u32) {
        if self.first.lock_or_recover().remove(&pid) {
            self.first_left.notify_all();
        }
    }
}

/// How often the node job counts its windows' agents ([`WindowActivityReads`]): the Fleet glance
/// reads a Mac's copy as often, so a count is never older than one read; a publisher writes only
/// a count that changed, and an unchanged one each `aterm_metrics::row::ACTIVITY_REFRESH_S`.
const ACTIVITY_EVERY: Duration = Duration::from_secs(60);

/// What this Mac's windows' agents are doing, counted at each read
/// ([`activity_of_windows`]), handed to the metrics publisher for the Mac's `now/activity` row
/// (`docs/DESIGN-fleet-field-2026-11-06.md` §11).
pub type WindowActivity = Arc<dyn Fn(&ActivityRow) + Send + Sync>;

/// The [`WindowActivity`] this process installed ([`install_window_activity`]).
static WINDOW_ACTIVITY: OnceLock<WindowActivity> = OnceLock::new();

/// Install `sink` as the node job's [`WindowActivity`], which the job's activity reads hand each
/// count to while it holds the mesh. The first install stands: the answer says whether this call
/// installed `sink`. With none installed, the job counts and calls nothing.
pub fn install_window_activity(sink: WindowActivity) -> bool {
    WINDOW_ACTIVITY.set(sink).is_ok()
}

/// What the node job's activity reads hand each count to: the installed [`WindowActivity`],
/// looked up at each count.
fn installed_window_activity() -> WindowActivity {
    Arc::new(|row| {
        if let Some(sink) = WINDOW_ACTIVITY.get() {
            sink(row);
        }
    })
}

/// This Mac's agents at `at` (Unix seconds), counted from a facts survey of every window here
/// ([`survey_every_window`], [`crate::fleet_glance::activity_of_survey`]), which every released
/// window already answers, so no window needs a new stream for it: a zero count where no
/// window runs (no agent runs here), and `None` where windows run and none answered a survey,
/// or where the answer is partial (a window left unread, rows the reply cap cut), so nothing is
/// published and the last whole count stands until it ages out at its readers.
pub(crate) fn activity_of_windows(windows: &WindowRelays, at: u64) -> Option<ActivityRow> {
    let header = RelayHeader::Survey {
        content: false,
        drive: false,
        peer: None,
    };
    match survey_every_window(windows, &header, "survey\n") {
        WindowsSurveyed::Answered(wire) => crate::fleet_glance::activity_of_survey(&wire, at),
        WindowsSurveyed::Untaken(None) => Some(ActivityRow {
            at,
            ..ActivityRow::default()
        }),
        WindowsSurveyed::Untaken(Some(_)) => None,
    }
}

/// The node job's counts of its windows' agents, run while it holds the mesh ([`MeshHeld`]): one
/// thread that, every `every`, counts them ([`activity_of_windows`]) and hands its sink each count
/// it made. Dropping it stops the thread and joins it, which waits out a survey under way
/// (`SURVEY_ANSWER_WITHIN` at most).
struct WindowActivityReads {
    stop: Arc<Stop>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl WindowActivityReads {
    /// Count the agents of `windows` at once and every `every` after, handing `sink` each count.
    ///
    /// # Errors
    /// The spawn of its thread.
    fn start(
        windows: WindowRelays,
        every: Duration,
        sink: WindowActivity,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(Stop::new());
        let thread = std::thread::Builder::new()
            .name("aterm-node-activity".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                move || {
                    while !stop.is_set() {
                        let at = aterm_time::unix_secs().unwrap_or(0);
                        if let Some(row) = activity_of_windows(&windows, at)
                            && !stop.is_set()
                        {
                            sink(&row);
                        }
                        stop.sleep(every);
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for WindowActivityReads {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The stream a window's needs are read on: [`RelayHeader::Needs`] handed to the window whose
/// relay socket is `path` ([`hand_over`]), then `go` written within [`HANDSHAKE`]; `None` where
/// the window took none.
fn open_needs(path: &Path) -> Option<CtlStream> {
    let HandOver::Took(mut stream) = hand_over(path, &RelayHeader::Needs.encode()) else {
        return None;
    };
    let _ = stream.set_write_timeout(Some(HANDSHAKE));
    if stream
        .write_all(b"go\n")
        .and_then(|()| stream.flush())
        .is_err()
    {
        aterm_uds::hangup::hang_up(&stream);
        return None;
    }
    let _ = stream.set_write_timeout(None);
    Some(stream)
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
/// ([`aterm_secrets::client::swarm_denial_snapshot_now`]): the listener asks it for each proved
/// dialer and its hand-over, and the watch every tick, so an unchanged vault is never decrypted
/// again. `cache` keeps the last authoritative list through a mid-apply or failed read, and
/// `None` until one was read, under which every proved dialer is refused
/// ([`super::update_denied_nodes_cache`]). A job that reads no vault (`vault_paths` `None`: one
/// under a private state root, [`super::vault_under`]) has a deny list that names no one.
/// A vault-tenure mismatch refuses traffic independently of the cached denial list.
fn denied_nodes_for(
    state: &Path,
    vault_paths: Option<&aterm_secrets::client::Paths>,
    cache: &Mutex<DeniedCache>,
) -> Option<Arc<[String]>> {
    if !aterm_agent::fleet_vault::traffic_in_effect(state, vault_paths) {
        return None;
    }
    let observed = match vault_paths {
        Some(paths) => aterm_secrets::client::swarm_denial_snapshot_now(paths),
        None => Ok(aterm_secrets::client::SwarmDenialSnapshot::Available(
            Vec::new(),
        )),
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
    fn is_empty(&self) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty()
    }

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
    maintenance: Option<&NodeMaintenance>,
    traffic: aterm_agent::fleet_vault::Traffic,
) -> bool {
    if !traffic.current() {
        return false;
    }
    // Announcements belong to this listener generation. Stop them before its
    // socket is released, including every early return while starting workers. Closing the
    // admission callback also precedes joining its worker on an early return or unwind.
    struct Announcing<'a> {
        advertiser: &'a AlabTcpAdvertiser,
        admission: aterm_agent::node_service::AdmissionAnnouncer,
        automatic_joins: Option<aterm_agent::vault_join::AutomaticJoins>,
    }
    impl Announcing<'_> {
        fn stop(&self) {
            self.admission.close();
            self.advertiser.stop();
        }
    }
    impl Drop for Announcing<'_> {
        fn drop(&mut self) {
            self.stop();
        }
    }
    let mut announcing = Announcing {
        advertiser,
        admission: advertiser.admission_announcer(addr.port()),
        automatic_joins: None,
    };
    let listener_stop = Arc::new(Stop::new());
    let open_table = Arc::new(NodeOpenTable::default());
    let windows = WindowRelays::new(cfg.relay_dir.clone(), Arc::clone(&cfg.launcher));
    announcing.automatic_joins = match cfg
        .vault_paths
        .clone()
        .map(|paths| {
            aterm_agent::vault_join::AutomaticJoins::start(paths, announcing.admission.notifier())
        })
        .transpose()
    {
        Ok(joins) => joins,
        Err(error) => {
            aterm_log::warn!("node: the automatic admission worker did not start: {error}");
            announcing.stop();
            cfg.stop.sleep(cfg.tick_every);
            return false;
        }
    };
    let automatic_handle = announcing
        .automatic_joins
        .as_ref()
        .map(|joins| joins.handle());
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
        let state = cfg.state_dir.clone();
        let cache = Arc::clone(denied_cache);
        super::denial_under_take(traffic.clone(), move || {
            let log = members.as_ref().and_then(|members| members.keys());
            super::denied_for_fleet(
                &state,
                vault.as_ref(),
                denied_nodes_for(&state, vault.as_ref(), &cache).as_deref(),
                log.as_deref(),
            )
        })
    };
    let neighbours = match (node.as_ref(), cfg.mesh.on) {
        (Some(me), true) => {
            let serve = LinkServe {
                state_dir: cfg.state_dir.clone(),
                windows: windows.clone(),
                home_dir: cfg.home_dir.clone(),
                config_path: cfg.config_path.clone(),
                vault_paths: cfg.vault_paths.clone(),
                members: cfg.members.clone(),
                deny: Arc::clone(&deny_now),
                denied: Arc::clone(denied_cache),
                open: Arc::clone(&open_table),
                serving: Arc::new(serving_fn.clone()),
                wake_timeout: cfg.wake_timeout,
                ask: relay_ask(windows.clone(), automatic_handle.clone().map(automatic_ask)),
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
        let listener_stop = Arc::clone(&listener_stop);
        let open_watch = Arc::clone(&open_table);
        let stop = Arc::clone(&cfg.stop);
        let state_dir = cfg.state_dir.clone();
        let windows = windows.clone();
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
        let traffic_watch = traffic.clone();
        let browse_watch = browse.cloned();
        let discovery = Arc::clone(discovery);
        let automatic = automatic_handle.clone();
        let updates_watch = cfg.updates.clone();
        let relay_dir = cfg.relay_dir.clone();
        let health_inputs = health::Inputs::of(cfg);
        let maintenance = maintenance.cloned();
        std::thread::Builder::new()
            .name("aterm-node-watch".to_owned())
            .spawn(move || {
                let _ends_the_generation = EndsTheGeneration(Arc::clone(&listener_stop));
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
                // Keep the same finite cap as the stop-only sleep this wait replaces.
                let tick_every = tick_every.min(Duration::from_secs(86_400 * 365));
                let mut next_tick = Instant::now() + tick_every;
                let mut wait_failed = false;

                while !listener_stop.is_set() {
                    let moved = match admission_network.wait_until(
                        &[stop.as_ref(), listener_stop.as_ref()],
                        Some(next_tick),
                    ) {
                        Ok(woke) => {
                            wait_failed = false;
                            match woke {
                                Some(crate::network_moved::Woke::Timer) => None,
                                other => other,
                            }
                        }
                        Err(error) => {
                            if !wait_failed {
                                aterm_log::warn!("node: the network wait failed: {error}; keeping its timed checks");
                                wait_failed = true;
                            }
                            stop.sleep(next_tick.saturating_duration_since(Instant::now()));
                            admission_network.take_at(Instant::now())
                        }
                    };
                    if stop.is_set() || listener_stop.is_set() {
                        listener_stop.set();
                        return;
                    }
                    if moved.is_some()
                        && let Some(automatic) = &automatic
                    {
                        automatic.refresh();
                    }
                    // Moves wake admission at once without renewing this independent deadline
                    // or repeating unrelated upkeep before it is due.
                    if Instant::now() < next_tick {
                        continue;
                    }
                    if let Some(updates) = &updates_watch {
                        updates.start_owed_look(&relay_dir, &stop);
                    }
                    if let Some(maintain) = &maintenance {
                        maintain();
                    }
                    if aterm_agent::node_service::left_the_fleet(&state_dir) {
                        listener_stop.set();
                        return;
                    }
                    if swarm_holds(config_path.as_deref()) {
                        listener_stop.set();
                        return;
                    }
                    if let Some(text) = read_config_text(config_path.as_deref())
                        && let Some(now_wanted) = node_table_in(&text, &fallback)
                        && now_wanted.as_ref().map(|w| w.listen.as_str())
                            != Some(initial_listen.as_str())
                    {
                        listener_stop.set();
                        return;
                    }
                    if !traffic_watch.current() {
                        listener_stop.set();
                        return;
                    }
                    let now_key = aterm_net::fleet::read_key_file(&state_dir);
                    let identity = aterm_net::fleet_member::read_node_identity(&state_dir)
                        .ok()
                        .flatten();
                    let now_node = identity.as_ref().map(|n| (n.node().to_owned(), n.key()));
                    if now_key.as_ref().ok() != Some(&secret_watch) || now_node != proved {
                        listener_stop.set();
                        return;
                    }

                    let mut log = members_watch.as_ref().and_then(|members| members.keys());
                    if let Some(automatic) = &automatic {
                        automatic.observe_membership(log.as_ref());
                    }
                    let denied = denied_nodes_for(&state_dir, vault_paths.as_ref(), &denied_watch);
                    if !open_watch.is_empty() {
                        // No deny list read yet: every open drive or content read is cut, as the
                        // handshake refuses each new connection, until one is read.
                        let unread = Serving {
                            scope: FleetScope::Facts,
                            grants: Vec::new(),
                        };
                        let _ = match denied.as_deref() {
                            Some(denied) => {
                                let serving = serving_watch();
                                // The book is read at the first judgment that needs it, an open
                                // drive's key, and once a tick at most: a tick with no drive open
                                // reads none, as the window's listener does (`holds_now`,
                                // `fleet_listen.rs`).
                                let book = std::cell::OnceCell::new();
                                let keys = || {
                                    book.get_or_init(|| {
                                        aterm_net::fleet_member::Keyring::read(&state_dir)
                                    })
                                };
                                open_watch.cut_unserved(
                                    &serving,
                                    &|n, key| holds_in(keys(), log.as_deref(), n, key),
                                    denied,
                                )
                            }
                            None => open_watch.cut_unserved(&unread, &|_, _| false, &[]),
                        };
                    }
                    // A link to a key the log took out ends now, not at its next re-proof.
                    if let Some(table) = &neighbours_watch {
                        table.judge();
                    }
                    // The trial's self-dial may wait out its reply bound: it runs after the deny
                    // list's cuts and the log's judgment, which it must not delay.
                    if let Some(updates) = &updates_watch
                        && !updates.settle_service(
                            &health_inputs,
                            (addr, &secret_watch, &proved, &initial_listen),
                            &stop,
                        )
                    {
                        listener_stop.set();
                        return;
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
                            has_live_window_relay(windows.dir()),
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
                            &windows,
                            home_dir.as_deref(),
                            config_path.as_deref(),
                            &secret_watch,
                            (denied, log.as_deref()),
                         &|| traffic_watch.current(),);
                    }
                    if last_census_refresh.elapsed() >= CENSUS_REFRESH_EVERY {
                        last_census_refresh = Instant::now();
                        maybe_refresh_local_census_at(&state_dir, home_dir.as_deref());
                    }
                    // A round still out takes no move: the move waits for the round after it.
                    if let Some(woke) = heartbeat
                        .dispatch_when_free(Instant::now(), hb_in_flight.load(Ordering::Acquire), || {
                        // Widen only an existing matching vault record.
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
                                (&listener_stop, &hb_in_flight),
                                (denied, log, browse_watch.clone()),
                                traffic_watch.clone(),
                            ),
                            None => {
                                aterm_log::debug!("node job heartbeat skipped: {DENY_LIST_UNREAD}");
                                false
                            }
                        }
                    }) {
                        super::say_heartbeat_woke("node job", woke);
                    }
                    next_tick = Instant::now() + tick_every;
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
            announcing.stop();
            cfg.stop.sleep(cfg.tick_every);
            return false;
        }
    };

    let initial_serving = serving_fn();
    let sharing = (!is_prebound)
        .then(|| share_units(addr, &listener_stop))
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
    let windows = windows.clone();
    let home_dir = cfg.home_dir.clone();
    let config_path = cfg.config_path.clone();
    let vault_paths = cfg.vault_paths.clone();
    let wake_timeout = cfg.wake_timeout;
    let join_secret = Arc::new(secret);
    let members = cfg.members.clone();
    let denied_for_serve = Arc::clone(denied_cache);
    let serving_for_offer = serving_fn.clone();
    let open_for_serve = Arc::clone(&open_table);
    let serving_generation = Arc::clone(&listener_stop);
    let ask = relay_ask(windows.clone(), automatic_handle.map(automatic_ask));
    let mesh = cfg.mesh.on;
    let table_for_serve = neighbours.as_ref().map(|links| Arc::clone(&links.table));
    let mesh_for_serve = mesh_logs.cloned();
    let units_for_serve = Arc::clone(&cfg.mesh.units);
    let ask_for_serve = Arc::clone(&ask);

    if let Some(told) = &cfg.on_generation {
        told(&initial_wanted.listen);
    }
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
        &listener_stop,
        HANDSHAKE,
        move |served, op| {
            let peer = served.peer_addr();
            // The handshake refused this connection while no deny list was read; one read since
            // is kept, so this is defense in depth: an unread list serves no op.
            let Some(denied) =
                denied_nodes_for(&state_dir, vault_paths.as_ref(), &denied_for_serve)
            else {
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
                    let authorize = stream.authorization();
                    register_member_at(
                        stream,
                        &st,
                        (peer, proof.as_ref()),
                        (addr, &secret),
                        (&denied, log.as_deref()),
                        &|| authorize().is_ok(),
                    )
                });
            }
            if op == FleetOp::Book {
                let st = state_dir.clone();
                return served.answer(|stream| {
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    let authorize = stream.authorization();
                    share_book_at(
                        stream,
                        &st,
                        peer,
                        (&denied, log.as_deref()),
                        (addr, &serving_generation),
                        &|| authorize().is_ok(),
                    )
                });
            }
            if op == FleetOp::Facts {
                let proof = served.peer_node().cloned();
                let st = state_dir.clone();
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    let authorize = stream.authorization();
                    receive_pushed_facts_at(
                        stream,
                        &st,
                        proof.as_ref(),
                        peer,
                        (&denied, log.as_deref()),
                        &|| authorize().is_ok(),
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
                let on = super::vault_join_on(judged, vault_paths.clone());
                return served.answer(|stream| {
                    stream.set_read_timeout(Some(HANDSHAKE))?;
                    stream.set_write_timeout(Some(HANDSHAKE))?;
                    let from = peer.map_or(std::net::Ipv4Addr::UNSPECIFIED.into(), |at| at.ip());
                    let ended = on.and_then(|(judged, paths)| {
                        aterm_agent::vault_join::serve(stream, from, &judged, None, &paths)
                            .map_err(|error| error.to_string())
                    });
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
                        let now_keys = std::cell::OnceCell::new();
                        let now_log = members.as_ref().and_then(|m| m.keys());
                        // No deny list read now: nothing is served.
                        denied_nodes_for(&state_dir, vault_paths.as_ref(), &denied_for_serve)
                            .is_some_and(|now_denied| {
                                now_serving.serves(
                                    content,
                                    grant_ref,
                                    &|n, key| {
                                        let now_keys = now_keys.get_or_init(|| {
                                            aterm_net::fleet_member::Keyring::read(&state_dir)
                                        });
                                        holds_in(now_keys, now_log.as_deref(), n, key)
                                    },
                                    &now_denied,
                                )
                            })
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
                    windows.clone(),
                    (driver, driver_key),
                    peer,
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
            let (near, far) = aterm_uds::kept_out(CtlStream::pair)?;
            let Some(open) = open_for_serve.admit(content, None, &near, |c, d| {
                serving_fn().serves(c, d, &|_, _| false, &[])
            })?
            else {
                return Err(std::io::Error::other(
                    "[fleet] lowered its level while it was admitted",
                ));
            };
            let reader = crate::fleet_activity::Reader::of(served.peer_node());
            if has_live_window_relay(windows.dir()) {
                let header = RelayHeader::Survey {
                    content,
                    drive,
                    peer,
                };
                spawn_node_survey(
                    near,
                    windows.clone(),
                    header,
                    Arc::clone(&open),
                    (state_dir.clone(), reader),
                )?;
            } else {
                spawn_windowless_survey(
                    near,
                    state_dir.clone(),
                    home_dir.clone(),
                    config_path.clone(),
                    (content, drive),
                    reader,
                )?;
            }
            served.relay_answers(far)
        },
        fleet_event,
    );

    listener_stop.set();
    announcing.stop();
    drop(neighbours);
    if let Some(sharing) = sharing {
        let _ = sharing.join();
    }
    if let Some(watch) = watch_handle {
        let _ = watch.join();
    }
    drop(announcing.automatic_joins.take());
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
    windows: &WindowRelays,
    home_dir: Option<&Path>,
    config_path: Option<&Path>,
    secret: &aterm_net::fleet::FleetSecret,
    (denied_nodes, log): (&[String], Option<&aterm_net::fleet_member::MemberKeys>),
    current: &dyn Fn() -> bool,
) {
    let window = has_live_window_relay(windows.dir());
    let node = aterm_net::fleet_member::read_node_id(state_dir)
        .ok()
        .flatten();
    let now_s = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    let facts = if window {
        query_window_survey_facts(windows).map(|mut live| {
            if live.node.is_none() {
                live.node.clone_from(&node);
            }
            live
        })
    } else {
        None
    }
    .unwrap_or_else(|| {
        let mut facts = aterm_agent::survey_facts::SwarmSurveyFacts::collect_at(
            state_dir,
            home_dir,
            config_path,
            0,
            window,
            node,
            now_s,
        );
        facts.update = Some(crate::survey::updater_fact());
        facts
    });
    let at_ms = aterm_time::unix_millis().unwrap_or(0);
    let host = aterm_uds::process::lan_host_name();
    let instances = usize::from(window);
    let access = aterm_agent::fleet_access::read(state_dir).level;
    let payload = facts.to_survey_reply(
        std::process::id(),
        host.as_deref(),
        at_ms,
        instances,
        (access, false),
    );
    if let Err(why) =
        push_facts_to_issuer_with_denied(state_dir, secret, (denied_nodes, log), &payload, current)
    {
        aterm_log::debug!(
            "node job: survey facts heartbeat push skipped: {}",
            aterm_log::sanitize_record(&why)
        );
    }
}

/// How many requests one fleet `Survey` connection is served, as a window serves one
/// (`crate::control`'s `FLEET_REQUESTS`): each `survey` asks every window here.
const SURVEY_REQUESTS: usize = 4;

/// Serve a fleet `Survey` connection on `near` while windows run here: each `survey` line is
/// answered by every window at once, merged ([`survey_every_window`]); any other line (`version`,
/// `setup …`, or one a window refuses in its words) and the rest of the connection go to the
/// installed app's window, as the one window served them all before. Where no window takes a
/// survey, it is answered busy, as a window that took no hand-over was.
fn spawn_node_survey(
    mut near: CtlStream,
    windows: WindowRelays,
    header: RelayHeader,
    open: Arc<NodeOpened>,
    (state_dir, reader): (PathBuf, Option<crate::fleet_activity::Reader>),
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-node-survey".to_owned())
        .spawn(move || {
            for _ in 0..SURVEY_REQUESTS {
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
                if open.is_cut() {
                    break;
                }
                let verb = line.split_ascii_whitespace().next().unwrap_or_default();
                if verb != "survey" {
                    match relay_to_window(&windows, &header) {
                        RelayOpen::Opened(window) => {
                            forward_node_drive(near, window, &open, &line);
                            return;
                        }
                        RelayOpen::Busy(why) => {
                            let _ = near.write_all(busy_line(&why).as_bytes());
                        }
                        RelayOpen::NoWindow => {
                            let _ = near.write_all(busy_line("its window closed").as_bytes());
                        }
                    }
                    break;
                }
                // The session that asked, where the line names one: this job notes its read, and
                // hands each window the line without the word, which a window of an aterm from
                // before it would answer with its usage.
                let rest = line.trim_start().strip_prefix("survey").unwrap_or_default();
                let asker = aterm_types::fleet_activity::survey_asker(rest).map(str::to_owned);
                let line = aterm_types::fleet_activity::survey_unmarked(line.trim_end())
                    .map_or(line, |plain| format!("{plain}\n"));
                let answer = match survey_every_window(&windows, &header, &line) {
                    WindowsSurveyed::Answered(answer) => answer,
                    WindowsSurveyed::Untaken(why) => {
                        let _ = near.write_all(
                            busy_line(why.as_deref().unwrap_or("its window closed")).as_bytes(),
                        );
                        break;
                    }
                };
                if near.write_all(answer.as_bytes()).is_err() || near.flush().is_err() {
                    break;
                }
                if answer.starts_with("OK ") {
                    crate::fleet_activity::note_in(
                        &state_dir,
                        crate::fleet_activity::surveyed(reader.as_ref(), asker.as_deref()),
                    );
                }
            }
            let _ = near.flush();
            aterm_uds::hangup::hang_up(&near);
        })
        .map(|_| ())
}

/// The fleet access level the last connection this node job judged was served at.
static NODE_ACCESS_SEEN: Mutex<Option<FleetAccess>> = Mutex::new(None);

/// This Mac's fleet access as `state_dir` stores it now, read per connection and failing closed
/// ([`aterm_agent::fleet_access::read`]). The first connection to read a level other than the
/// last this job served journals the change, and what made it.
fn node_access(state_dir: &Path) -> FleetAccess {
    let reading = aterm_agent::fleet_access::read(state_dir);
    let was = NODE_ACCESS_SEEN
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .replace(reading.level);
    if let Some(was) = was.filter(|was| *was != reading.level) {
        aterm_log::info!(
            "aterm node serve: fleet access is now {} (was {}; set by {})",
            reading.level.word(),
            was.word(),
            reading
                .by
                .map_or("a writer its file does not name", |by| by.said()),
        );
    }
    reading.level
}

fn spawn_windowless_survey(
    mut near: CtlStream,
    state_dir: PathBuf,
    home_dir: Option<PathBuf>,
    config_path: Option<PathBuf>,
    (content, drive): (bool, bool),
    reader: Option<crate::fleet_activity::Reader>,
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
                        let mut facts = aterm_agent::survey_facts::SwarmSurveyFacts::collect_at(
                            &state_dir,
                            home_dir.as_deref(),
                            config_path.as_deref(),
                            0,
                            false,
                            node,
                            now_s,
                        );
                        facts.update = Some(crate::survey::updater_fact());
                        let here = crate::survey::Here {
                            host: aterm_uds::process::lan_host_name(),
                            instances: 0,
                            access: aterm_agent::fleet_access::read(&state_dir).level,
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
                        // A survey a session asked for is that session's read of this Mac.
                        if reply.starts_with("OK ") {
                            let _ = near.flush();
                            crate::fleet_activity::note_in(
                                &state_dir,
                                crate::fleet_activity::surveyed(
                                    reader.as_ref(),
                                    aterm_types::fleet_activity::survey_asker(rest),
                                ),
                            );
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
    windows: WindowRelays,
    (driver, driver_key): (crate::fabric::Driver, aterm_net::fleet::NodeKey),
    peer: Option<SocketAddr>,
    wake_timeout: Duration,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-node-drive".to_owned())
        .spawn(move || {
            // The requester line first: refused, nothing of the drive is served or routed. A
            // window gets it in the relay header ([`RelayHeader::Drive`]), which a window of
            // another build refuses whole, so none serves a drive that names no requester.
            let requester = match crate::control::read_requester(&mut near) {
                Ok(requester) => requester,
                Err(why) => {
                    let _ = near.write_all(format!("ERR drive: {why}\n").as_bytes());
                    let _ = near.flush();
                    aterm_uds::hangup::hang_up(&near);
                    return;
                }
            };
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
            let (verb, rest) = trimmed
                .split_once(char::is_whitespace)
                .map_or((trimmed, ""), |(verb, rest)| (verb, rest.trim_start()));
            // The lock, before anything this job serves itself or routes: the line's lexical
            // kind against this Mac's stored level. A line with no kind is no act the lock
            // names, and whoever serves it refuses it in its own words.
            let kind = crate::control::drive_kind_of_line(trimmed);
            // The fleet activity record's row of this first line, where this job ends or serves
            // it: a line it relays is the window's to note, which judges it again. A read no
            // session asked for has no row, and neither has a fleet service's line (the
            // vault's, service-repair's, accounts-renew's), whose client is aterm itself.
            let note = |kind: DriveKind, outcome| {
                crate::fleet_activity::note_in(
                    &state_dir,
                    crate::fleet_activity::driven(&driver, kind, requester.sid(), outcome),
                );
            };
            let served = |reply: &str| {
                if let Ok(kind) = kind
                    && reply.starts_with("OK")
                {
                    note(kind, aterm_types::fleet_activity::Outcome::Admitted);
                }
            };
            if let Ok(kind) = kind {
                let level = node_access(&state_dir);
                if let Some(refusal) = crate::control::drive_locked(level, kind) {
                    note_refused_driver(
                        peer,
                        &format!("{} driving `{}`: fleet access", driver.host, kind.word()),
                    );
                    let _ = near.write_all(refusal.as_bytes());
                    let _ = near.flush();
                    // Every refusal has its row, however many the log line above thins; it is
                    // handed over once the answer is out.
                    note(
                        kind,
                        aterm_types::fleet_activity::Outcome::Blocked {
                            level,
                            needs: kind.needs(),
                        },
                    );
                    aterm_uds::hangup::hang_up(&near);
                    return;
                }
            }
            if verb == "vault" {
                let reply = aterm_secrets::client::admin_service::serve_at(
                    vault_paths.as_ref(),
                    &format!("fleet:{}", driver.node),
                    rest,
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
                served(&reply);
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            // Machine upkeep stays in the node, even while windows open and close.
            // Forwarding polls to the newest window would lose their request book.
            if verb == aterm_agent::harness::remote_renew::VERB {
                let reply = aterm_agent::harness::remote_renew::serve(
                    &format!("fleet:{}", driver.node),
                    rest,
                );
                let _ = near.write_all(reply.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            if verb == aterm_agent::service_repair_rpc::VERB {
                let reply = aterm_agent::service_repair_rpc::serve_in_context(
                    &driver.node,
                    &driver_key.hex(),
                    rest,
                    vault_paths.as_ref(),
                    &state_dir,
                );
                let _ = near.write_all(reply.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            let busy = |why: &str| {
                format!(
                    "ERR busy: an aterm window is open on this Mac and took no fleet connection \
                     ({}); ask again\n",
                    aterm_log::sanitize_record(why),
                )
            };
            let header = RelayHeader::Drive {
                driver,
                key: Some(driver_key),
                peer,
                sids: Vec::new(),
                requester,
            };
            let answer = match route_drive(&windows, &header, &first_line) {
                DriveRoute::Window(win) => {
                    forward_node_drive(near, win, &open, &first_line);
                    return;
                }
                DriveRoute::Split(shares) => {
                    // The wait is one request of this Mac however many windows share it: its
                    // row is written here, and each window is handed its share naming no
                    // session ([`unnamed`]), which it counts as no request.
                    if let (
                        Ok(kind),
                        RelayHeader::Drive {
                            driver, requester, ..
                        },
                    ) = (kind, &header)
                    {
                        crate::fleet_activity::note_in(
                            &state_dir,
                            crate::fleet_activity::driven(
                                driver,
                                kind,
                                requester.sid(),
                                aterm_types::fleet_activity::Outcome::Admitted,
                            ),
                        );
                    }
                    serve_split_await(near, &open, &unnamed(&header), shares, &first_line);
                    return;
                }
                DriveRoute::Answer(answer) => answer,
                DriveRoute::Busy(why) => busy(&why),
                // A window is launched only for an admitted spawn: the same parse decided the
                // lock above, so a locked Mac launches none for any line.
                DriveRoute::NoWindow if kind == Ok(DriveKind::Spawn) => String::new(),
                DriveRoute::NoWindow => "ERR no window open on this Mac\n".to_owned(),
            };
            if !answer.is_empty() {
                let _ = near.write_all(answer.as_bytes());
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            if !windows.launcher.is_logged_in() {
                let _ = near.write_all(b"ERR logged out\n");
                let _ = near.flush();
                aterm_uds::hangup::hang_up(&near);
                return;
            }
            if let Err(why) = windows.launcher.launch_window(&state_dir, windows.dir()) {
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
                if let Some(stream) = open_window_relay(&windows, &header) {
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

/// Hand `near` to the window on `win`, which reads `first_line` first: a drive's first request,
/// or a survey connection's first line.
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

/// Where a drive's first line goes among this Mac's windows ([`route_drive`]).
enum DriveRoute {
    /// The window that took it: the one that hosts or keeps every session it names, or with none
    /// named the installed app's.
    Window(CtlStream),
    /// No window runs here.
    NoWindow,
    /// Windows run and none took it: why, last heard.
    Busy(String),
    /// An `await task` whose targets live in more than one window: each window's share, in
    /// hand-over order ([`serve_split_await`]).
    Split(Vec<AwaitShare>),
    /// Answered here, as a window would: a session the wait names that no window hosts.
    Answer(String),
}

/// One window's share of an `await task` split across windows: its relay, the stream it took the
/// share's header on, and the targets it hosts or keeps.
struct AwaitShare {
    path: PathBuf,
    stream: CtlStream,
    sids: Vec<SessionId>,
}

/// Route a drive's `first_line`, one rule for every drive verb. A line that names sessions
/// ([`crate::control::drive_line_sessions`]) goes to the window that hosts or keeps every one, as
/// `header` carries them; one that names none (a spawn's) to the installed app's window
/// ([`WindowRelays::in_order`]).
///
/// Where no window hosts them all: an `await task` whose targets live in several windows is split
/// across them, so a driver never needs to know this Mac runs two (a refusal would make it ask
/// each window apart, which it cannot name); any other line goes, naming no session, to the window
/// that hosts the most of them (the first in order where none does), whose own drive gate answers
/// for the rest in its words (`no such session`, or that a drive names a session this Mac hosts).
/// A window that answered nothing at all may host the rest: then the drive is answered busy.
fn route_drive(windows: &WindowRelays, header: &RelayHeader, first_line: &str) -> DriveRoute {
    let mut sids = crate::control::drive_line_sessions(first_line);
    sids.retain(|sid| relay_names(sid.as_str()));
    let naming = |sids: Vec<SessionId>| share_header(header, &sids);
    let declined = match hand_over_in_order(windows, &naming(sids.clone())) {
        Ok(stream) => return DriveRoute::Window(stream),
        Err(declined) => declined,
    };
    if declined.hosting.is_empty() {
        return declined.busy.map_or(DriveRoute::NoWindow, DriveRoute::Busy);
    }
    // The window (by its place in `hosting`) that hosts each named session, the first in order.
    let owner: Vec<Option<usize>> = sids
        .iter()
        .map(|sid| {
            declined
                .hosting
                .iter()
                .position(|(_, hosted)| hosted.contains(sid))
        })
        .collect();
    if let Some(why) = &declined.busy
        && owner.iter().any(Option::is_none)
    {
        return DriveRoute::Busy(why.clone());
    }
    let mut owners: Vec<usize> = owner.iter().flatten().copied().collect();
    owners.sort_unstable();
    owners.dedup();
    let unconstrained = naming(Vec::new()).encode();
    if owners.len() > 1 && is_await_task(first_line) {
        if let Some(gone) = sids.iter().zip(&owner).find(|(_, at)| at.is_none()) {
            return DriveRoute::Answer(format!("ERR no such session @{}\n", gone.0.as_str()));
        }
        let mut shares = Vec::with_capacity(owners.len());
        for at in owners {
            let share: Vec<SessionId> = sids
                .iter()
                .zip(&owner)
                .filter(|(_, by)| **by == Some(at))
                .map(|(sid, _)| sid.clone())
                .collect();
            let path = declined.hosting[at].0.clone();
            match hand_over(&path, &share_header(&unnamed(header), &share).encode()) {
                HandOver::Took(stream) => shares.push(AwaitShare {
                    path,
                    stream,
                    sids: share,
                }),
                HandOver::Elsewhere(_) => {
                    return DriveRoute::Busy("a session moved between windows".to_owned());
                }
                HandOver::Busy(why) => return DriveRoute::Busy(why),
            }
        }
        return DriveRoute::Split(shares);
    }
    let most = declined
        .hosting
        .iter()
        .enumerate()
        .max_by_key(|(at, (_, hosted))| (hosted.len(), std::cmp::Reverse(*at)))
        .map(|(_, (path, _))| path.clone());
    match most.map(|path| hand_over(&path, &unconstrained)) {
        Some(HandOver::Took(stream)) => DriveRoute::Window(stream),
        Some(HandOver::Elsewhere(_)) | None => {
            DriveRoute::Busy("it hosts not the sessions the drive names".to_owned())
        }
        Some(HandOver::Busy(why)) => DriveRoute::Busy(why),
    }
}

/// Whether a drive's line is an `await task` (with no selector in front).
fn is_await_task(line: &str) -> bool {
    let mut words = line.split_whitespace();
    words.next() == Some("await") && words.next() == Some("task")
}

/// `first_line`, an `await task` over several windows' sessions, narrowed to the targets in
/// `share` (each list element kept as the driver spelled it, `@s-…:<rev>` edges included) and,
/// where `timeout` is given, waiting that long instead of the line's own timeout.
fn await_share_line(first_line: &str, share: &[SessionId], timeout: Option<&str>) -> String {
    let mut words = first_line.split_whitespace();
    let mut out: Vec<String> = Vec::new();
    while let Some(word) = words.next() {
        if word.starts_with('@') {
            let kept: Vec<&str> = word
                .split(',')
                .filter(|element| {
                    let body = element.strip_prefix('@').unwrap_or(element);
                    let sid = body.split_once(':').map_or(body, |(sid, _)| sid);
                    share.iter().any(|named| named.as_str() == sid)
                })
                .collect();
            if !kept.is_empty() {
                out.push(kept.join(","));
            }
        } else if timeout.is_some() && word == "timeout" {
            // `timeout <ms>`, replaced below.
            let _ = words.next();
        } else if !(timeout.is_some() && word.starts_with("timeout=")) {
            out.push(word.to_owned());
        }
    }
    if let Some(timeout) = timeout {
        out.push(format!("timeout={timeout}"));
    }
    let mut line = out.join(" ");
    line.push('\n');
    line
}

/// What a window answered its share of a split `await task`: `OK <n>[ status=timeout]` and its
/// rows, the one line it answered otherwise (an `ERR`), or nothing whole: it hung up, or no window
/// here took the share again.
enum ShareReply {
    Rows { timed_out: bool, rows: Vec<String> },
    Line(String),
    HungUp,
}

/// Read a window's answer to an `await task` share.
fn read_share_reply(stream: &mut CtlStream) -> ShareReply {
    let Some(head) = read_line_raw(stream, aterm_types::control_verbs::MAX_REQUEST_LINE)
        .ok()
        .flatten()
    else {
        return ShareReply::HungUp;
    };
    let Some(count) = head
        .trim_end_matches(['\r', '\n'])
        .strip_prefix("OK ")
        .map(|rest| rest.split_whitespace().collect::<Vec<_>>())
    else {
        return ShareReply::Line(head);
    };
    let timed_out = count.get(1) == Some(&"status=timeout");
    let Some(n) = count
        .first()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|&n| n <= crate::task_record::MAX_AWAIT_TARGETS)
    else {
        return ShareReply::Line(head);
    };
    let mut rows = Vec::with_capacity(n);
    for _ in 0..n {
        let Some(row) = read_line_raw(stream, aterm_types::control_verbs::MAX_REQUEST_LINE)
            .ok()
            .flatten()
        else {
            return ShareReply::HungUp;
        };
        rows.push(row.trim_end_matches(['\r', '\n']).to_owned());
    }
    ShareReply::Rows { timed_out, rows }
}

/// Serve an `await task` whose targets live in several windows here ([`DriveRoute::Split`]): each
/// window waits on its own share, as the line asked (`any` or `all`, its edges and timeout), and
/// the driver is answered as one window would answer the whole wait: one row per target, in the
/// line's order.
///
/// Under `any`, the first share with news ends the wait: every other window's wait is hung up and
/// its share read again at once (`timeout=0`, the same edges, so news there shows too). Under
/// `all`, or once a share ran out of time, every share's answer is waited for; the answer times
/// out where the line's combine says (`any`: all of them; `all`: any). A share answered with one
/// line (an `ERR`) is the answer. A window that hangs up on its share hangs up the driver too,
/// unanswered, as one window closing on the whole wait would: the driver asks again, and the
/// question finds where the sessions are then. The driver hanging up hangs up every window's wait.
fn serve_split_await(
    mut near: CtlStream,
    open: &NodeOpened,
    header: &RelayHeader,
    shares: Vec<AwaitShare>,
    first_line: &str,
) {
    let all = first_line.split_whitespace().any(|word| word == "all");
    let hang_up_all = |shares: &[(PathBuf, Vec<SessionId>, CtlStream)]| {
        for (_, _, stream) in shares {
            aterm_uds::hangup::hang_up(stream);
        }
    };
    let mut held = Vec::with_capacity(shares.len());
    for share in shares {
        held.push((share.path, share.sids, share.stream));
    }
    let attached = held
        .first()
        .is_some_and(|(_, _, stream)| open.attach_line(stream));
    let mut sent = attached;
    for (_, sids, stream) in &mut held {
        sent = sent
            && stream
                .write_all(await_share_line(first_line, sids, None).as_bytes())
                .and_then(|()| stream.flush())
                .is_ok();
    }
    if !sent {
        hang_up_all(&held);
        aterm_uds::hangup::hang_up(&near);
        return;
    }
    enum Heard {
        Share(usize, ShareReply),
        HungUp,
    }
    let (tx, rx) = std::sync::mpsc::channel::<Heard>();
    for (at, (_, _, stream)) in held.iter().enumerate() {
        let Ok(mut reading) = aterm_uds::kept_out(|| stream.try_clone()) else {
            hang_up_all(&held);
            aterm_uds::hangup::hang_up(&near);
            return;
        };
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("aterm-node-await-share".to_owned())
            .spawn(move || {
                let _ = tx.send(Heard::Share(at, read_share_reply(&mut reading)));
            });
    }
    if let Ok(mut watching) = aterm_uds::kept_out(|| near.try_clone()) {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("aterm-node-await-driver".to_owned())
            .spawn(move || {
                let mut sink = [0u8; 256];
                while matches!(watching.read(&mut sink), Ok(n) if n > 0) {}
                let _ = tx.send(Heard::HungUp);
            });
    }
    drop(tx);
    let mut answers: Vec<Option<(bool, Vec<String>)>> = vec![None; held.len()];
    let answer = loop {
        let Ok(heard) = rx.recv() else {
            break None;
        };
        let (at, reply) = match heard {
            Heard::HungUp => break None,
            Heard::Share(at, reply) => (at, reply),
        };
        let (timed_out, rows) = match reply {
            ShareReply::Line(line) => break Some(line),
            ShareReply::HungUp => break None,
            ShareReply::Rows { timed_out, rows } => (timed_out, rows),
        };
        answers[at] = Some((timed_out, rows));
        if !all && !timed_out {
            // News ends an `any` wait: the other shares are read again at once.
            let mut failed = None;
            for (other, (path, sids, stream)) in held.iter().enumerate() {
                if answers[other].is_some() {
                    continue;
                }
                aterm_uds::hangup::hang_up(stream);
                match read_share_now(path, header, sids, first_line) {
                    ShareReply::Rows { rows, .. } => answers[other] = Some((false, rows)),
                    ShareReply::Line(line) => {
                        failed = Some(Some(line));
                        break;
                    }
                    ShareReply::HungUp => {
                        failed = Some(None);
                        break;
                    }
                }
            }
            if let Some(failed) = failed {
                break failed;
            }
        }
        if answers.iter().all(Option::is_some) {
            break Some(merge_share_rows(first_line, &answers, all));
        }
    };
    hang_up_all(&held);
    if let Some(answer) = answer {
        let _ = near.write_all(answer.as_bytes());
        let _ = near.flush();
    }
    aterm_uds::hangup::hang_up(&near);
}

/// A split wait's share read at once (`timeout=0`, the line's own edges), on a hand-over of its
/// own to the window at `path`, once another share's news ended an `any` wait. A look answers
/// within the served exchange's handshake bound; a window that does not, or no longer hosts the
/// share or takes no connection, is [`ShareReply::HungUp`].
fn read_share_now(
    path: &Path,
    header: &RelayHeader,
    sids: &[SessionId],
    first_line: &str,
) -> ShareReply {
    let HandOver::Took(mut stream) = hand_over(path, &share_header(header, sids).encode()) else {
        return ShareReply::HungUp;
    };
    let _ = stream.set_read_timeout(Some(HANDSHAKE));
    let read = stream
        .write_all(await_share_line(first_line, sids, Some("0")).as_bytes())
        .and_then(|()| stream.flush())
        .map(|()| read_share_reply(&mut stream));
    aterm_uds::hangup::hang_up(&stream);
    read.unwrap_or(ShareReply::HungUp)
}

/// `header` naming no requester: what a window is handed with its share of a split wait. The
/// node job wrote the wait's one fleet activity row, so no window counts its share as a
/// request of its own.
fn unnamed(header: &RelayHeader) -> RelayHeader {
    match header.clone() {
        RelayHeader::Drive {
            driver,
            key,
            peer,
            sids,
            ..
        } => RelayHeader::Drive {
            driver,
            key,
            peer,
            sids,
            requester: aterm_types::control_verbs::Requester::default(),
        },
        other => other,
    }
}

/// `header` naming `sids`: a split wait's share handed over again.
fn share_header(header: &RelayHeader, sids: &[SessionId]) -> RelayHeader {
    match header.clone() {
        RelayHeader::Drive {
            driver,
            key,
            peer,
            requester,
            ..
        } => RelayHeader::Drive {
            driver,
            key,
            peer,
            sids: sids.to_vec(),
            requester,
        },
        other => other,
    }
}

/// The split wait's answer, as one window answers `await task`: `OK <n>`, ` status=timeout` where
/// the line's combine has every share (`any`) or one share (`all`) out of time, then each
/// target's row in the line's order.
fn merge_share_rows(
    first_line: &str,
    answers: &[Option<(bool, Vec<String>)>],
    all: bool,
) -> String {
    let timed_out = |share: &Option<(bool, Vec<String>)>| share.as_ref().is_some_and(|(t, _)| *t);
    let timeout = if all {
        answers.iter().any(timed_out)
    } else {
        answers.iter().all(timed_out)
    };
    let mut rows: Vec<&str> = answers
        .iter()
        .flatten()
        .flat_map(|(_, rows)| rows.iter().map(String::as_str))
        .collect();
    let order = crate::control::drive_line_sessions(first_line);
    let place = |row: &str| {
        let sid = row.split_whitespace().next().unwrap_or_default();
        let sid = sid.strip_prefix('@').unwrap_or(sid);
        order
            .iter()
            .position(|named| named.as_str() == sid)
            .unwrap_or(usize::MAX)
    };
    rows.sort_by_key(|row| place(row));
    let mut out = format!(
        "OK {}{}\n",
        rows.len(),
        if timeout { " status=timeout" } else { "" }
    );
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
    out
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
    deny: aterm_net::fleet::DenyFn,
    state_dir: PathBuf,
    windows: WindowRelays,
    home_dir: Option<PathBuf>,
    config_path: Option<PathBuf>,
    vault_paths: Option<aterm_secrets::client::Paths>,
    members: Option<Arc<aterm_agent::fleet_log::LocalLog>>,
    denied: Arc<Mutex<DeniedCache>>,
    open: Arc<NodeOpenTable>,
    serving: Arc<dyn Fn() -> Serving + Send + Sync>,
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
        if (self.deny)().refuses(Some(incoming.peer())) {
            return incoming.refuse("admission changed");
        }
        let peer = Some(incoming.peer_addr());
        let op = incoming.op();
        // No deny list read now: no stream is served, as no connection is.
        let Some(denied) =
            denied_nodes_for(&self.state_dir, self.vault_paths.as_ref(), &self.denied)
        else {
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
                // Admitted as a driver, and judged as one: the stream is the mesh's cross-Mac
                // message path, which this Mac's fleet access admits from Read and Write. A
                // stream already open is not cut on a drop, as on a grant taken back.
                if let Some(refusal) =
                    crate::control::drive_locked(node_access(&self.state_dir), DriveKind::BusLog)
                {
                    aterm_log::info!(
                        "aterm node serve: refused {}'s bus.log stream: fleet access",
                        incoming.peer().node
                    );
                    return incoming.refuse(refusal.trim_end());
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
        let Ok((near, far)) = aterm_uds::kept_out(CtlStream::pair) else {
            return incoming.refuse("busy");
        };
        let Ok(Some(open)) = self.open.admit(content, None, &near, still) else {
            return incoming.refuse("level");
        };
        let reader = crate::fleet_activity::Reader::of(Some(incoming.peer()));
        let serving = if has_live_window_relay(self.windows.dir()) {
            let header = RelayHeader::Survey {
                content,
                drive,
                peer,
            };
            spawn_node_survey(
                near,
                self.windows.clone(),
                header,
                Arc::clone(&open),
                (self.state_dir.clone(), reader),
            )
        } else {
            spawn_windowless_survey(
                near,
                self.state_dir.clone(),
                self.home_dir.clone(),
                self.config_path.clone(),
                (content, drive),
                reader,
            )
        };
        if serving.is_err() {
            return incoming.refuse("busy");
        }
        pump_link(incoming.accept(), far, open);
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
            let now_keys = std::cell::OnceCell::new();
            let now_log = self.members.as_ref().and_then(|m| m.keys());
            denied_nodes_for(&self.state_dir, self.vault_paths.as_ref(), &self.denied).is_some_and(
                |now_denied| {
                    now_serving.serves(
                        content,
                        grant,
                        &|n, key| {
                            let now_keys = now_keys.get_or_init(|| {
                                aterm_net::fleet_member::Keyring::read(&self.state_dir)
                            });
                            holds_in(now_keys, now_log.as_deref(), n, key)
                        },
                        &now_denied,
                    )
                },
            )
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
            self.windows.clone(),
            (driver, driver_key),
            peer,
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
