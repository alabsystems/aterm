// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `systems.alab.aterm-node` background job (`aterm node serve | status | on | off`), kept
//! separate from `systems.alab.aterm-secrets` so a fault in fleet code never stops credentials
//! (`docs/DESIGN-alab-swarm-2026-10-27.md` §3.3, §7, §12.3 C20–C23).

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The launchd label and systemd user unit stem for the fleet/swarm node job.
pub const NODE_LABEL: &str = "systems.alab.aterm-node";

/// Systemd unit `Description` for [`NODE_LABEL`].
pub const NODE_DESCRIPTION: &str = "aterm fleet and swarm node job (aterm node serve)";

/// Exclusive advisory lock held by a running `aterm node serve` in `<state_dir>`.
pub const NODE_JOB_LOCK: &str = "node-job.lock";

/// Whether this Mac left its fleet and holds no fleet's key since: `aterm fleet leave` wrote
/// `fleet-off` and removed the key. A `fleet-off` beside a key (a fleet formed after a leave, which
/// keeps the record) is a member's.
#[must_use]
pub fn left_the_fleet(state_dir: &Path) -> bool {
    std::fs::symlink_metadata(state_dir.join(aterm_net::fleet_member::OFF_FILE)).is_ok()
        && aterm_net::fleet::read_key_file(state_dir).is_err()
}

/// Exclusive advisory lock a running `aterm node serve` holds in `<state_dir>` only while it serves
/// the fleet port or is about to bind it ([`node_port_holds_lock`]).
pub const NODE_PORT_LOCK: &str = "node-port.lock";

/// Marker written by `aterm node off` in `<state_dir>` so `sync.node` leaves the job off.
pub const NODE_OFF_FILE: &str = "node-off";

/// Bonjour service type advertised by `aterm node serve` on every network (§3.3, C23; F17).
pub const ALAB_TCP_SERVICE: &str = "_alab._tcp";

/// Maximum age (24 h) of this Mac's last ALab vault answer before `_alab._tcp` stops advertising.
pub const ALAB_MEMBER_TTL_SECS: u64 = 24 * 60 * 60;

/// Path to `dns-sd` on macOS.
pub const DNS_SD: &str = aterm_uds::os_path!("dns-sd");

/// Path to `launchctl` on macOS.
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// Candidate `systemctl` paths on Linux (never a `PATH` lookup).
pub const SYSTEMCTL_PATHS: [&str; 3] = [
    "/usr/bin/systemctl",
    "/bin/systemctl",
    "/run/current-system/sw/bin/systemctl",
];

/// LaunchAgent plist path: `<home>/Library/LaunchAgents/systems.alab.aterm-node.plist`.
#[must_use]
pub fn plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{NODE_LABEL}.plist"))
}

/// Systemd user unit path: `<home>/.config/systemd/user/systems.alab.aterm-node.service`.
#[must_use]
pub fn unit_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{NODE_LABEL}.service"))
}

/// Log path for launchd stdout/stderr: `<state_dir>/systems.alab.aterm-node.log`.
#[must_use]
pub fn log_path(state_dir: &Path) -> PathBuf {
    state_dir.join(format!("{NODE_LABEL}.log"))
}

/// Off-marker path in `<state_dir>`: `<state_dir>/node-off`.
#[must_use]
pub fn off_path(state_dir: &Path) -> PathBuf {
    state_dir.join(NODE_OFF_FILE)
}

/// Whether the person turned `systems.alab.aterm-node` off (`aterm node off`).
#[must_use]
pub fn node_turned_off(state_dir: &Path) -> bool {
    std::fs::symlink_metadata(off_path(state_dir)).is_ok()
}

/// Exclusive advisory lock held in `<state_dir>/node-job.lock` for the lifetime of
/// `aterm node serve` (one job at a time), and the same kind of lock in `node-port.lock` while it
/// serves the fleet port: interactive windows test [`node_port_holds_lock`] and refrain from
/// binding port 7100 while the node job serves it (C20).
#[cfg(unix)]
pub struct NodeJobLock {
    file: File,
}

#[cfg(unix)]
impl NodeJobLock {
    /// Try to acquire `<state_dir>/node-job.lock` without blocking. Returns `Ok(None)` when
    /// another `aterm node serve` process already holds the lock.
    ///
    /// # Errors
    /// Returns an error if `state_dir` cannot be made private or the lock file cannot be opened.
    pub fn try_acquire(state_dir: &Path) -> std::io::Result<Option<Self>> {
        Self::try_acquire_at(state_dir, NODE_JOB_LOCK)
    }

    /// Try to acquire `<state_dir>/node-port.lock` ([`NODE_PORT_LOCK`]) without blocking: the
    /// node job holds it only while it serves the fleet port or is about to bind it, so a window
    /// yields the port to a job that serves it and never to one that idles. `Ok(None)` while
    /// another process holds it.
    ///
    /// # Errors
    /// As [`Self::try_acquire`].
    pub fn try_acquire_port(state_dir: &Path) -> std::io::Result<Option<Self>> {
        Self::try_acquire_at(state_dir, NODE_PORT_LOCK)
    }

    fn try_acquire_at(state_dir: &Path, name: &str) -> std::io::Result<Option<Self>> {
        aterm_uds::private_file::ensure_private_dir(state_dir)?;
        let file = aterm_uds::private_file::open_private_rw(state_dir, name)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        aterm_uds::keep_out_of_children(&file)?;
        for attempt in 0..32 {
            #[expect(
                clippy::disallowed_methods,
                reason = "node-job.lock and node-port.lock are CLOEXEC/CLOFORK and NodeJobLock::drop releases by LOCK_UN"
            )]
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { file })),
                Err(std::fs::TryLockError::WouldBlock) => {
                    // Distinguish another exclusive `NodeJobLock` (`LOCK_EX`) from a transient
                    // shared `node_job_holds_lock` probe (`LOCK_SH`): if `try_lock_shared` also
                    // returns `WouldBlock`, an exclusive lock is held so we return `Ok(None)`
                    // immediately; if `try_lock_shared` succeeds, only a shared probe was in
                    // flight so we unlock and retry.
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "probe lock is CLOEXEC/CLOFORK and released immediately by LOCK_UN"
                    )]
                    match file.try_lock_shared() {
                        Ok(()) => {
                            let _ = file.unlock();
                            if attempt < 4 {
                                std::thread::yield_now();
                            } else {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                        }
                        Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                        Err(std::fs::TryLockError::Error(err)) => return Err(err),
                    }
                }
                Err(std::fs::TryLockError::Error(err)) => return Err(err),
            }
        }
        Ok(None)
    }
}

#[cfg(unix)]
impl Drop for NodeJobLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(unix)]
struct NodeJobProbe(File);

#[cfg(unix)]
impl Drop for NodeJobProbe {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Whether a local `aterm node serve` process currently holds `<state_dir>/node-job.lock`.
#[cfg(unix)]
#[must_use]
pub fn node_job_holds_lock(state_dir: &Path) -> bool {
    holds_lock_at(state_dir, NODE_JOB_LOCK)
}

/// Whether a local `aterm node serve` serves the fleet port now, or is about to bind it: it holds
/// `<state_dir>/node-port.lock` ([`NodeJobLock::try_acquire_port`]). A window yields the port only
/// while this holds.
#[cfg(unix)]
#[must_use]
pub fn node_port_holds_lock(state_dir: &Path) -> bool {
    holds_lock_at(state_dir, NODE_PORT_LOCK)
}

#[cfg(unix)]
fn holds_lock_at(state_dir: &Path, name: &str) -> bool {
    let Ok((file, _)) = aterm_uds::private_file::open_private(state_dir, name) else {
        return false;
    };
    if aterm_uds::keep_out_of_children(&file).is_err() {
        return false;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "shared probe lock is CLOEXEC/CLOFORK and NodeJobProbe::drop releases immediately by LOCK_UN"
    )]
    match file.try_lock_shared() {
        Ok(()) => {
            drop(NodeJobProbe(file));
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// Role advertised in `_alab._tcp` TXT (`role=member|steward`, §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Ordinary swarm member.
    Member,
    /// Steward holding the vault's admin locker.
    Steward,
}

impl NodeRole {
    /// Wire token for `role=`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Steward => "steward",
        }
    }
}

/// Exact `_alab._tcp` TXT payload (`v=1`, `role=member|steward`, `fleet=<port>`, `window=0|1`;
/// nothing else — no label, node id or version, per §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlabTcpTxt {
    /// Whether this node holds the steward admin locker.
    pub role: NodeRole,
    /// Fleet TCP port (`7100` by default).
    pub fleet_port: u16,
    /// Whether at least one interactive `aterm` window is open for this login.
    pub window: bool,
}

impl AlabTcpTxt {
    /// Four TXT key-value pairs in canonical order (`§3.3`).
    #[must_use]
    pub fn txt_pairs(self) -> [String; 4] {
        [
            "v=1".to_owned(),
            format!("role={}", self.role.word()),
            format!("fleet={}", self.fleet_port),
            format!("window={}", u8::from(self.window)),
        ]
    }
}

/// Arguments to `dns-sd` registering `_alab._tcp` on `local` with [`AlabTcpTxt`].
/// `instance` must be an ephemeral opaque token (never a label, hostname, node id or version).
#[must_use]
pub fn alab_tcp_dns_sd_args(instance: &str, txt: AlabTcpTxt) -> Vec<String> {
    let mut args = vec![
        "-R".to_owned(),
        instance.to_owned(),
        ALAB_TCP_SERVICE.to_owned(),
        "local".to_owned(),
        txt.fleet_port.to_string(),
    ];
    args.extend(txt.txt_pairs());
    args
}

/// Generate a fresh random opaque instance token (`alab-<8 hex>`) carrying no machine identity.
#[must_use]
pub fn random_alab_instance_name() -> String {
    let mut bytes = [0u8; 4];
    if aterm_uds::rand::fill(&mut bytes).is_err() {
        let fallback = u32::try_from(aterm_time::unix_millis().unwrap_or(0) & 0xffff_ffff)
            .unwrap_or(0)
            ^ std::process::id();
        bytes = fallback.to_le_bytes();
    }
    format!("alab-{}", aterm_codec::hex::encode_lower(&bytes))
}

/// Current route-source tokens (`route_sources()`), empty when offline.
#[must_use]
pub fn current_route_tokens() -> Vec<String> {
    aterm_secrets::server::route_sources()
        .into_iter()
        .flatten()
        .map(|ip| ip.to_string())
        .collect()
}

/// Whether `_alab._tcp` is advertised (§3.3, C23; on every network since F17,
/// `docs/DESIGN-fluid-swarm-2026-10-28.md` §4.1): `[swarm] enabled` is not `false` (`enabled`),
/// this Mac has a route (`routes`), and it is an ALab vault member whose vault answered within
/// [`ALAB_MEMBER_TTL_SECS`] of `now_secs` (`heard`, unix seconds). No network is special: a
/// café's routes advertise as an office's do, since the advert carries no identity and a dial of
/// it is judged by its handshake's proof.
#[must_use]
pub fn advertises(enabled: bool, routes: &[String], heard: Option<u64>, now_secs: u64) -> bool {
    enabled
        && !routes.is_empty()
        && heard.is_some_and(|heard| now_secs.saturating_sub(heard) <= ALAB_MEMBER_TTL_SECS)
}

/// [`advertises`] as `aterm_config`'s `[swarm]` and the vault at `vault_paths` stand at
/// `now_secs` on `routes`, with the role the advert names: steward on a vault admin.
pub fn should_advertise_alab_tcp_with(
    vault_paths: &aterm_secrets::client::Paths,
    aterm_config: Option<&Path>,
    routes: &[String],
    now_secs: u64,
) -> (bool, NodeRole) {
    let enabled = aterm_config
        .is_none_or(|cfg_path| aterm_secrets::swarm::enabled_at(cfg_path).unwrap_or(false));
    let heard = aterm_secrets::client::status(vault_paths)
        .ok()
        .filter(|status| {
            status
                .config
                .as_ref()
                .is_some_and(aterm_secrets::client::alab_vault)
        })
        .and_then(|status| match status.membership {
            aterm_secrets::client::Membership::In { heard, .. } => Some(heard),
            aterm_secrets::client::Membership::Plain
            | aterm_secrets::client::Membership::Unconfigured
            | aterm_secrets::client::Membership::Pending
            | aterm_secrets::client::Membership::Revoked { .. } => None,
        });
    if !advertises(enabled, routes, heard, now_secs) {
        return (false, NodeRole::Member);
    }
    let role = if aterm_secrets::client::admin_cached(vault_paths).is_ok() {
        NodeRole::Steward
    } else {
        NodeRole::Member
    };
    (true, role)
}

/// Tender for a single `_alab._tcp` registration helper, tied to this process by
/// [`aterm_uds::spawnfd::Tether`], killed and reaped on drop or whenever this Mac stops
/// advertising ([`advertises`]).
pub struct AlabTcpAdvertiser {
    instance: String,
    inner: Mutex<AdvertiserInner>,
}

struct AdvertiserInner {
    child: Option<(aterm_uds::spawnfd::Child, AlabTcpTxt)>,
}

impl Default for AlabTcpAdvertiser {
    fn default() -> Self {
        Self::new(random_alab_instance_name())
    }
}

impl AlabTcpAdvertiser {
    /// Create a tender using `instance` as the opaque Bonjour service instance name.
    #[must_use]
    pub fn new(instance: String) -> Self {
        Self {
            instance,
            inner: Mutex::new(AdvertiserInner { child: None }),
        }
    }

    /// The opaque Bonjour instance name it registers, which this process's own browse holds as
    /// its own.
    #[must_use]
    pub fn instance(&self) -> &str {
        &self.instance
    }

    /// Reconcile the `dns-sd` child with current ALab-network eligibility and [`AlabTcpTxt`].
    pub fn tick(
        &self,
        vault_paths: &aterm_secrets::client::Paths,
        aterm_config: Option<&Path>,
        fleet_port: u16,
        window_open: bool,
    ) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let routes = current_route_tokens();
        let now_secs = aterm_time::unix_secs().unwrap_or(0);
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let (eligible, role) =
            should_advertise_alab_tcp_with(vault_paths, aterm_config, &routes, now_secs);
        let wanted = eligible.then_some(AlabTcpTxt {
            role,
            fleet_port,
            window: window_open,
        });
        reconcile_dns_sd(&mut guard.child, wanted, &self.instance, &spawn_dns_sd);
    }

    /// Stop and reap any running `dns-sd` child immediately.
    pub fn stop(&self) {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((mut child, _)) = guard.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for AlabTcpAdvertiser {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_dns_sd(args: &[String]) -> std::io::Result<aterm_uds::spawnfd::Child> {
    use aterm_uds::spawnfd::{Stdio, Tether as _};
    aterm_uds::spawnfd::command(DNS_SD)
        .tether()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Run the advertiser's helper with private service arguments for a lifecycle probe.
///
/// # Errors
/// The helper could not start.
#[cfg(any(test, debug_assertions))]
pub fn spawn_alab_advertisement_for_probe(
    args: &[String],
) -> std::io::Result<aterm_uds::spawnfd::Child> {
    spawn_dns_sd(args)
}

fn reconcile_dns_sd(
    held: &mut Option<(aterm_uds::spawnfd::Child, AlabTcpTxt)>,
    wanted: Option<AlabTcpTxt>,
    instance: &str,
    spawn: &dyn Fn(&[String]) -> std::io::Result<aterm_uds::spawnfd::Child>,
) {
    if let (Some((child, prev_txt)), Some(txt)) = (held.as_mut(), wanted)
        && *prev_txt == txt
        && matches!(child.try_wait(), Ok(None))
    {
        return;
    }
    if let Some((mut child, _)) = held.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let Some(txt) = wanted else {
        return;
    };
    let args = alab_tcp_dns_sd_args(instance, txt);
    if let Ok(child) = spawn(&args) {
        *held = Some((child, txt));
    }
}

/// Render the macOS `LaunchAgent` plist for `systems.alab.aterm-node`, putting `atpkg`'s managed
/// `pkg/bin` first on `PATH` (C8, C20).
#[must_use]
pub fn node_plist_text(
    program: &Path,
    home: &Path,
    state_dir: &Path,
    bundle_id: Option<&str>,
) -> String {
    let xml = |s: &str| format!("<string>{}</string>", aterm_codec::xml::escape(s));
    let log = log_path(state_dir);
    let pkg_bin = home.join("Library/Application Support/aterm/pkg/bin");
    let path_env = format!(
        "{}:/usr/bin:/bin:/usr/sbin:/sbin",
        pkg_bin.to_string_lossy()
    );
    let associated = bundle_id.map_or_else(String::new, |id| {
        format!(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t{}\n\t</array>\n",
            xml(id)
        )
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t{}\n\
         \t\t<string>node</string>\n\
         \t\t<string>serve</string>\n\
         \t</array>\n\
         \t<key>EnvironmentVariables</key>\n\
         \t<dict>\n\
         \t\t<key>HOME</key>\n\
         \t\t{}\n\
         \t\t<key>PATH</key>\n\
         \t\t{}\n\
         \t</dict>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Background</string>\n\
         {associated}\
         \t<key>StandardOutPath</key>\n\
         \t{}\n\
         \t<key>StandardErrorPath</key>\n\
         \t{}\n\
         </dict>\n\
         </plist>\n",
        xml(NODE_LABEL),
        xml(&program.to_string_lossy()),
        xml(&home.to_string_lossy()),
        xml(&path_env),
        xml(&log.to_string_lossy()),
        xml(&log.to_string_lossy()),
    )
}

fn systemd_escape_exec(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('"');
    for ch in word.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn systemd_escape_env(key: &str, val: &str) -> String {
    let mut out = String::from("\"");
    out.push_str(key);
    out.push('=');
    for ch in val.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render the Linux `systemd --user` unit for `systems.alab.aterm-node`.
#[must_use]
pub fn node_systemd_unit_text(program: &Path, home: &Path) -> String {
    let pkg_bin = home.join(".local/share/aterm/pkg/bin");
    let path_env = format!("{}:/usr/local/bin:/usr/bin:/bin", pkg_bin.to_string_lossy());
    format!(
        "[Unit]\n\
         Description={NODE_DESCRIPTION}\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={} \"node\" \"serve\"\n\
         Environment={}\n\
         Environment={}\n\
         Restart=always\n\
         RestartSec=5\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        systemd_escape_exec(&program.to_string_lossy()),
        systemd_escape_env("HOME", &home.to_string_lossy()),
        systemd_escape_env("PATH", &path_env),
    )
}

/// Extract the `ProgramArguments[0]` or `ExecStart` binary path from a rendered unit file.
#[must_use]
pub fn kept_program_in_unit(text: &str) -> Option<PathBuf> {
    if let Some(after) = text.split("<key>ProgramArguments</key>").nth(1)
        && let Some(array) = after.split("</array>").next()
        && let Some(first) = array.split("<string>").nth(1)
        && let Some(raw) = first.split("</string>").next()
    {
        let unescaped = raw
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        if !unescaped.is_empty() {
            return Some(PathBuf::from(unescaped));
        }
    }
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("ExecStart=\"")
            && let Some(end) = rest.find('"')
        {
            let unescaped = rest[..end]
                .replace("$$", "$")
                .replace("%%", "%")
                .replace("\\\"", "\"")
                .replace("\\\\", "\\");
            if !unescaped.is_empty() {
                return Some(PathBuf::from(unescaped));
            }
        }
    }
    None
}

/// Observed state of `systems.alab.aterm-node`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeJobState {
    /// Unit file is not installed.
    Absent,
    /// Unit file is installed; `pid` is `Some` when launchd/systemd reports it running.
    Installed {
        /// Running process ID, if active.
        pid: Option<u32>,
        /// Path to the plist or systemd service file.
        unit: PathBuf,
        /// Binary path named in the unit file, if readable.
        program: Option<PathBuf>,
    },
}

/// Platform supervisor seam so unit tests exercise `on`, `off`, `sync.node`, and `fleet leave`
/// deterministically without touching the host's real launchd/systemd.
pub trait ServiceSupervisor {
    /// Unit file path under `home` (`~/Library/LaunchAgents/...` or `~/.config/systemd/user/...`).
    fn unit_file(&self, home: &Path) -> PathBuf;
    /// Render unit file contents for `program`.
    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String;
    /// Query running PID (`Ok(Some(pid))` if running, `Ok(None)` if not running).
    ///
    /// # Errors
    /// Returns an error if the supervisor query itself failed fatally.
    fn running_pid(&self, home: &Path) -> Result<Option<u32>, String>;
    /// Load/restart the written unit file at `unit_path`.
    ///
    /// # Errors
    /// Returns an error if `launchctl` or `systemctl` rejected the unit.
    fn load_and_start(&self, unit_path: &Path) -> Result<(), String>;
    /// Stop and unload `systems.alab.aterm-node` (ignoring already-unloaded units).
    ///
    /// # Errors
    /// Returns an error if unloading a loaded unit failed.
    fn stop_and_unload(&self, unit_path: &Path) -> Result<(), String>;
    /// Why the running job stands where it should not, `None` where it stands right: on macOS, a
    /// job loaded in the background domain (`user/<uid>`, where an SSH-only pass put it) while a
    /// login holds the screen, whose `open -g -a` a spawn needs in `gui/<uid>`.
    fn misplaced(&self) -> Option<String> {
        None
    }
    /// Whether the job's process `pid` runs a build an update replaced: its executable image is
    /// no longer the file `program` names.
    fn runs_replaced(&self, pid: u32, program: &Path) -> bool {
        let _ = (pid, program);
        false
    }
    /// Restart the loaded job on the program its unit names now (`launchctl kickstart -k`),
    /// never a bootout followed at once by a bootstrap.
    ///
    /// # Errors
    /// The supervisor refused the restart.
    fn renew(&self) -> Result<(), String> {
        Ok(())
    }
}

/// One `launchctl` call, so the job's upkeep runs inert in a test.
pub trait Launchctl {
    /// Run `launchctl args…`: whether it exited 0, and what it printed (stdout, then stderr).
    ///
    /// # Errors
    /// `launchctl` could not be run, or did not end within [`LAUNCHCTL_WITHIN`].
    fn run(&self, args: &[&str]) -> Result<(bool, String), String>;
    /// Observe a service: its printed state, or `None` only when its service or domain is absent.
    ///
    /// # Errors
    /// The query could not run or was refused for a reason other than explicit absence.
    fn print_service(&self, target: &str) -> Result<Option<String>, String>;
}

/// How long one `launchctl` call may take.
pub const LAUNCHCTL_WITHIN: Duration = Duration::from_secs(30);

/// How long a reload waits for launchd to let the old job go before it loads the plist again.
pub const BOOTOUT_SETTLE: Duration = Duration::from_secs(30);

/// `/bin/launchctl`, each call bounded by [`LAUNCHCTL_WITHIN`].
pub struct RealLaunchctl;

impl RealLaunchctl {
    fn output(&self, args: &[&str]) -> Result<(std::process::ExitStatus, String), String> {
        let mut cmd = aterm_uds::spawnfd::command(LAUNCHCTL);
        cmd.args(args);
        let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + LAUNCHCTL_WITHIN)
            .map_err(|e| format!("{LAUNCHCTL} {}: {e}", args.join(" ")))?;
        let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
        said.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status, said))
    }
}

fn printed_service(
    target: &str,
    code: Option<i32>,
    said: String,
) -> Result<Option<String>, String> {
    match code {
        Some(0) => Ok(Some(said)),
        // `launchctl error 112` and `113`: absent domain and absent service. Other
        // nonzero statuses, including a permission refusal, establish no absence.
        Some(112 | 113) => Ok(None),
        _ => Err(format!(
            "{LAUNCHCTL} print {target} (exit {code:?}): {}",
            said.trim()
        )),
    }
}

impl Launchctl for RealLaunchctl {
    fn run(&self, args: &[&str]) -> Result<(bool, String), String> {
        let (status, said) = self.output(args)?;
        Ok((status.success(), said))
    }

    fn print_service(&self, target: &str) -> Result<Option<String>, String> {
        let (status, said) = self.output(&["print", target])?;
        printed_service(target, status.code(), said)
    }
}

/// The job's launchd upkeep for user `uid`, every call through `launchctl`.
pub struct Launchd<'a> {
    /// The seam each call goes through.
    pub launchctl: &'a dyn Launchctl,
    /// The user whose domains hold the job.
    pub uid: u32,
    /// How long a reload waits for a bootout to settle ([`BOOTOUT_SETTLE`]).
    pub settle: Duration,
}

impl Launchd<'_> {
    fn gui(&self) -> String {
        format!("gui/{}", self.uid)
    }

    fn user(&self) -> String {
        format!("user/{}", self.uid)
    }

    /// The domain the job belongs in now: the login at the screen's where there is one, else the
    /// user's background domain.
    fn wanted(&self) -> Result<String, String> {
        Ok(if self.launchctl.run(&["print", &self.gui()])?.0 {
            self.gui()
        } else {
            self.user()
        })
    }

    /// Each domain that holds the job, with its pid while it runs.
    fn loaded(&self) -> Result<Vec<(String, Option<u32>)>, String> {
        let mut held = Vec::new();
        for domain in [self.gui(), self.user()] {
            let printed = self
                .launchctl
                .print_service(&format!("{domain}/{NODE_LABEL}"))?;
            if let Some(said) = printed {
                held.push((domain, parse_launchctl_pid(&said)));
            }
        }
        Ok(held)
    }

    /// The running job's pid, in whichever domain holds it.
    ///
    /// # Errors
    /// `launchctl` could not be asked.
    pub fn running_pid(&self) -> Result<Option<u32>, String> {
        Ok(self.loaded()?.into_iter().find_map(|(_, pid)| pid))
    }

    /// Load the plist at `unit_path` in the domain the job belongs in: every domain that holds the
    /// job lets it go first, and the plist is loaded only once launchd no longer holds it, since a
    /// bootstrap during the teardown fails and strands the job unloaded.
    ///
    /// # Errors
    /// A bootout did not settle within [`Self::settle`], or launchd refused the bootstrap.
    pub fn load(&self, unit_path: &Path) -> Result<(), String> {
        let wanted = self.wanted()?;
        for (domain, _) in self.loaded()? {
            let target = format!("{domain}/{NODE_LABEL}");
            let _ = self.launchctl.run(&["bootout", &target])?;
            let until = Instant::now() + self.settle;
            while self.launchctl.run(&["print", &target])?.0 {
                if Instant::now() >= until {
                    return Err(format!(
                        "launchd still holds {target} {} s after its bootout; the next pass loads it",
                        self.settle.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let unit = unit_path.to_string_lossy();
        let (ok, said) = self.launchctl.run(&["bootstrap", &wanted, &unit])?;
        if ok {
            return Ok(());
        }
        let refused = format!("{LAUNCHCTL} bootstrap {wanted} {unit}: {}", said.trim());
        // A move the login's session refuses must not strand the job unloaded: it goes back to
        // the background session it ran in, and the pass still reports the refusal.
        let user = self.user();
        if wanted != user && self.launchctl.run(&["bootstrap", &user, &unit])?.0 {
            return Err(format!("{refused}; kept it in {user}"));
        }
        Err(refused)
    }

    /// Unload the job from every domain that holds it.
    ///
    /// # Errors
    /// `launchctl` could not be asked, or refused to unload a job it reported loaded.
    pub fn unload(&self) -> Result<(), String> {
        for (domain, _) in self.loaded()? {
            let target = format!("{domain}/{NODE_LABEL}");
            let (ok, said) = self.launchctl.run(&["bootout", &target])?;
            if !ok {
                return Err(format!("{LAUNCHCTL} bootout {target}: {}", said.trim()));
            }
        }
        Ok(())
    }

    /// See [`ServiceSupervisor::misplaced`].
    #[must_use]
    pub fn misplaced(&self) -> Option<String> {
        let wanted = self.wanted().ok()?;
        let held = self.loaded().ok()?;
        (!held.is_empty() && held.iter().any(|(domain, _)| *domain != wanted)).then(|| {
            format!(
                "{NODE_LABEL} is loaded in {}, and it belongs in {wanted}, where the login at the \
                 screen is",
                held.iter()
                    .map(|(domain, _)| domain.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )
        })
    }

    /// See [`ServiceSupervisor::renew`].
    ///
    /// # Errors
    /// No domain holds the job, or launchd refused the restart.
    pub fn renew(&self) -> Result<(), String> {
        let Some((domain, _)) = self.loaded()?.into_iter().next() else {
            return Err(format!("launchd holds no {NODE_LABEL} to renew"));
        };
        let target = format!("{domain}/{NODE_LABEL}");
        let (ok, said) = self.launchctl.run(&["kickstart", "-k", &target])?;
        if ok {
            Ok(())
        } else {
            Err(format!(
                "{LAUNCHCTL} kickstart -k {target}: {}",
                said.trim()
            ))
        }
    }
}

/// Whether process `pid`'s executable image is another file than the one `program` names now
/// (an update replaced it at the same path).
#[must_use]
pub fn image_replaced(pid: u32, program: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let now = std::fs::metadata(program).ok().map(|meta| meta.ino());
        matches!(
            (aterm_sysprobe::kproc::image_inode(pid), now),
            (Some(running), Some(now)) if running != now
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, program);
        false
    }
}

/// Live OS supervisor (`launchd` on macOS, `systemd --user` on Linux).
pub struct LiveSupervisor;

/// A rejected disable is harmless only when the supervisor explicitly names no loaded unit
/// and no running process. Missing, mixed or unreadable status stays a refusal.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn systemd_absent(state: &str) -> bool {
    let field = |key: &str| {
        let mut values = state.lines().filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name == key).then_some(value)
        });
        let value = values.next()?;
        values.next().is_none().then_some(value)
    };
    field("LoadState") == Some("not-found")
        && field("ActiveState") == Some("inactive")
        && field("MainPID") == Some("0")
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
type SystemctlResult = Result<(bool, String), String>;

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn unload_systemd(run: &dyn Fn(&[&str]) -> SystemctlResult) -> Result<(), String> {
    let service = format!("{NODE_LABEL}.service");
    let (ok, said) = run(&["--user", "disable", "--now", &service])?;
    if !ok {
        let (_, state) = run(&[
            "--user",
            "show",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=MainPID",
            &service,
        ])?;
        if !systemd_absent(&state) {
            return Err(format!(
                "systemctl --user disable --now {service}: {}",
                said.trim()
            ));
        }
    }
    let (ok, said) = run(&["--user", "daemon-reload"])?;
    if !ok {
        return Err(format!("systemctl --user daemon-reload: {}", said.trim()));
    }
    Ok(())
}

impl LiveSupervisor {
    /// The bundle identifier of the app `program` (through any link to it) is the executable of,
    /// as its `Info.plist` names it, so the job is aterm.app's, as `secrets watch`'s is.
    fn bundle_id_for(program: &Path) -> Option<String> {
        #[cfg(unix)]
        {
            let exe = program
                .canonicalize()
                .unwrap_or_else(|_| program.to_path_buf());
            aterm_secrets::service::bundle_id_of(&exe)
        }
        #[cfg(not(unix))]
        {
            let _ = program;
            None
        }
    }
}

impl ServiceSupervisor for LiveSupervisor {
    fn unit_file(&self, home: &Path) -> PathBuf {
        if cfg!(target_os = "macos") {
            plist_path(home)
        } else {
            unit_path(home)
        }
    }

    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String {
        if cfg!(target_os = "macos") {
            node_plist_text(
                program,
                home,
                state_dir,
                Self::bundle_id_for(program).as_deref(),
            )
        } else {
            node_systemd_unit_text(program, home)
        }
    }

    fn running_pid(&self, _home: &Path) -> Result<Option<u32>, String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).running_pid()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let Some(systemctl) = SYSTEMCTL_PATHS.into_iter().find(|p| Path::new(p).is_file())
            else {
                return Ok(None);
            };
            let service = format!("{NODE_LABEL}.service");
            let out = aterm_uds::spawnfd::command(systemctl)
                .args(["--user", "show", "--property=MainPID", "--value", &service])
                .output()
                .map_err(|e| format!("{systemctl} show {service}: {e}"))?;
            if !out.status.success() {
                return Ok(None);
            }
            let pid = String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|&p| p > 0);
            Ok(pid)
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }

    fn load_and_start(&self, unit_path: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).load(unit_path)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = unit_path;
            let systemctl = SYSTEMCTL_PATHS
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .ok_or_else(|| "systemctl not found".to_owned())?;
            let service = format!("{NODE_LABEL}.service");
            let _ = aterm_uds::spawnfd::command(systemctl)
                .args(["--user", "daemon-reload"])
                .output();
            let out = aterm_uds::spawnfd::command(systemctl)
                .args(["--user", "enable", "--now", &service])
                .output()
                .map_err(|e| format!("{systemctl} --user enable --now {service}: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "{systemctl} --user enable --now {service}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let _ = aterm_uds::spawnfd::command(systemctl)
                .args(["--user", "restart", &service])
                .output();
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = unit_path;
            Err("node service is Unix only".to_owned())
        }
    }

    fn stop_and_unload(&self, _unit_path: &Path) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).unload()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let systemctl = SYSTEMCTL_PATHS
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .ok_or_else(|| "systemctl not found; the node unit was kept".to_owned())?;
            unload_systemd(&|args| {
                let mut command = aterm_uds::spawnfd::command(systemctl);
                command.args(args);
                let out = aterm_uds::deadline::output_until(
                    &mut command,
                    Instant::now() + LAUNCHCTL_WITHIN,
                )
                .map_err(|error| format!("{systemctl} {}: {error}", args.join(" ")))?;
                let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
                said.push_str(&String::from_utf8_lossy(&out.stderr));
                Ok((out.status.success(), said))
            })
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }

    fn misplaced(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).misplaced()
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    fn runs_replaced(&self, pid: u32, program: &Path) -> bool {
        image_replaced(pid, program)
    }

    fn renew(&self) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            live_launchd(&RealLaunchctl).renew()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let systemctl = SYSTEMCTL_PATHS
                .into_iter()
                .find(|p| Path::new(p).is_file())
                .ok_or_else(|| "systemctl not found".to_owned())?;
            let service = format!("{NODE_LABEL}.service");
            let out = aterm_uds::spawnfd::command(systemctl)
                .args(["--user", "restart", &service])
                .output()
                .map_err(|e| format!("{systemctl} --user restart {service}: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "{systemctl} --user restart {service}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }
        #[cfg(not(unix))]
        {
            Ok(())
        }
    }
}

/// This user's launchd, as the live supervisor reaches it.
#[cfg(target_os = "macos")]
fn live_launchd(launchctl: &dyn Launchctl) -> Launchd<'_> {
    Launchd {
        launchctl,
        uid: aterm_uds::peer::our_uid(),
        settle: BOOTOUT_SETTLE,
    }
}

/// Inert supervisor for isolated test environments (`ATERM_STATE_HOME` or `CI`), writing/removing
/// unit files under `home` without calling the host's live `launchctl` or `systemctl`.
pub struct FileOnlySupervisor;

impl ServiceSupervisor for FileOnlySupervisor {
    fn unit_file(&self, home: &Path) -> PathBuf {
        LiveSupervisor.unit_file(home)
    }

    fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String {
        LiveSupervisor.render_unit(program, home, state_dir)
    }

    fn running_pid(&self, _home: &Path) -> Result<Option<u32>, String> {
        Ok(None)
    }

    fn load_and_start(&self, _unit_path: &Path) -> Result<(), String> {
        Ok(())
    }

    fn stop_and_unload(&self, _unit_path: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// Parse `pid = <u32>` from `launchctl print` output.
#[must_use]
pub fn parse_launchctl_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("pid = ") {
            let digits = rest.trim_end_matches(';').trim();
            if let Ok(pid) = digits.parse::<u32>()
                && pid > 0
            {
                return Some(pid);
            }
        }
    }
    None
}

/// Inspect `systems.alab.aterm-node` state via `supervisor`.
///
/// # Errors
/// Returns an error if reading the unit file or supervisor state fails fatally.
pub fn state_with(home: &Path, supervisor: &dyn ServiceSupervisor) -> Result<NodeJobState, String> {
    let unit = supervisor.unit_file(home);
    if !unit.is_file() {
        return Ok(NodeJobState::Absent);
    }
    let program = std::fs::read_to_string(&unit)
        .ok()
        .and_then(|text| kept_program_in_unit(&text));
    let pid = supervisor.running_pid(home)?;
    Ok(NodeJobState::Installed { pid, unit, program })
}

/// Install or update `systems.alab.aterm-node`, clearing any `node-off` marker when `clear_off`
/// is true.
///
/// # Errors
/// Returns an error if writing the unit file or loading the job fails.
pub fn install_with(
    program: &Path,
    home: &Path,
    state_dir: &Path,
    clear_off: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<PathBuf, String> {
    aterm_uds::private_file::ensure_private_dir(state_dir).map_err(|e| e.to_string())?;
    if clear_off {
        let off = off_path(state_dir);
        match std::fs::remove_file(&off) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", off.display())),
        }
    }
    let unit = supervisor.unit_file(home);
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let rendered = supervisor.render_unit(program, home, state_dir);
    aterm_secrets::fsx::write_atomic(&unit, rendered.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", unit.display()))?;
    supervisor.load_and_start(&unit)?;
    Ok(unit)
}

/// Stop and remove `systems.alab.aterm-node`. When `record_off` is true (`aterm node off`),
/// writes `<state_dir>/node-off` so `sync.node` leaves it off; when false (`aterm fleet leave`),
/// removes the unit cleanly without recording `node-off`.
///
/// # Errors
/// Returns an error if writing `node-off`, stopping the loaded job or removing the unit file
/// fails. A failed stop preserves the unit so it can be retried.
pub fn remove_with(
    home: &Path,
    state_dir: &Path,
    record_off: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<Option<PathBuf>, String> {
    if record_off {
        aterm_uds::private_file::ensure_private_dir(state_dir).map_err(|e| e.to_string())?;
        aterm_uds::private_file::write_private(
            state_dir,
            NODE_OFF_FILE,
            b"the node job was turned off by choice; aterm node on turns it back on\n",
        )
        .map_err(|e| format!("{}: {e}", off_path(state_dir).display()))?;
    }
    let unit = supervisor.unit_file(home);
    let existed = unit.is_file();
    supervisor.stop_and_unload(&unit)?;
    match std::fs::remove_file(&unit) {
        Ok(()) => Ok(Some(unit)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(existed.then_some(unit)),
        Err(e) => Err(format!("{}: {e}", unit.display())),
    }
}

/// Remove `systems.alab.aterm-node` during `aterm fleet leave` (C20).
///
/// # Errors
/// Returns an error if the unit file exists and cannot be removed.
pub fn remove_for_fleet_leave(home: &Path, state_dir: &Path) -> Result<Option<PathBuf>, String> {
    if std::env::var_os("ATERM_STATE_HOME").is_some() || std::env::var_os("CI").is_some() {
        // In isolated test homes, never call the host's live launchctl/systemctl.
        let unit = LiveSupervisor.unit_file(home);
        return match std::fs::remove_file(&unit) {
            Ok(()) => Ok(Some(unit)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", unit.display())),
        };
    }
    remove_with(home, state_dir, false, &LiveSupervisor)
}

/// Converge read state for `sync.node` (C20).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncNodeState {
    /// Not applicable on this Mac (turned off, not in a fleet, CI, or no installed binary).
    NotApplicable(String),
    /// Installed and running the expected binary.
    Current(String),
    /// Needs installation, binary update, or restart.
    Due {
        /// Current observed state.
        from: String,
        /// Target state after `sync.node` applies.
        to: String,
    },
}

/// Resolve the installed release `aterm` executable for `sync.node` (`current_exe` when it is an
/// installed release app, else `/Applications/aterm.app/Contents/MacOS/aterm` on macOS or
/// `<home>/.local/bin/aterm` on Linux). Never returns a scratch or cargo build binary.
#[must_use]
pub fn resolve_installed_program(
    current_exe: Option<&Path>,
    installed_release: bool,
    home: &Path,
) -> Option<PathBuf> {
    if installed_release
        && let Some(exe) = current_exe
        && exe.is_file()
    {
        return Some(exe.to_path_buf());
    }
    let mac_app = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
    if cfg!(target_os = "macos") && mac_app.is_file() {
        return Some(mac_app);
    }
    let linux_bin = home.join(".local/bin/aterm");
    if cfg!(target_os = "linux") && linux_bin.is_file() {
        return Some(linux_bin);
    }
    None
}

/// Pure `sync.node` probe over injected paths and `supervisor`.
///
/// # Errors
/// Returns an error if reading `aterm.toml` or querying `supervisor` fails.
pub fn read_for_sync_with(
    home: &Path,
    state_dir: &Path,
    aterm_config: Option<&Path>,
    program: Option<&Path>,
    under_ci: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<SyncNodeState, String> {
    if let Some(cfg) = aterm_config
        && !aterm_secrets::swarm::enabled_at(cfg).map_err(|e| e.to_string())?
    {
        return Ok(SyncNodeState::NotApplicable(
            aterm_secrets::swarm::SWARM_OFF.to_owned(),
        ));
    }
    if under_ci {
        return Ok(SyncNodeState::NotApplicable(
            "under CI no pass installs the node job".to_owned(),
        ));
    }
    if node_turned_off(state_dir) {
        return Ok(SyncNodeState::NotApplicable(
            "off on this Mac by your choice: `aterm node on` turns it on".to_owned(),
        ));
    }
    // Membership is the fleet's key. A leave removes it (and writes `fleet-off`, which keeps the
    // default fleet's sync from joining again); a fleet a person forms or joins here after that is
    // served by the window all the same, so the job stands for it too.
    if aterm_net::fleet::read_key_file(state_dir).is_err() {
        return Ok(SyncNodeState::NotApplicable(
            if left_the_fleet(state_dir) {
                "this Mac left its fleet (`aterm fleet leave`)"
            } else {
                "this Mac is in no fleet (sync.fleet)"
            }
            .to_owned(),
        ));
    }
    let Some(program) = program else {
        return Ok(SyncNodeState::NotApplicable(
            "no installed release aterm binary on this Mac".to_owned(),
        ));
    };
    let target = format!("{NODE_LABEL} running {}", program.display());
    match state_with(home, supervisor)? {
        NodeJobState::Absent => Ok(SyncNodeState::Due {
            from: format!("{NODE_LABEL} is not installed"),
            to: target,
        }),
        NodeJobState::Installed {
            pid: Some(pid),
            program: Some(kept),
            ..
        } if kept == program => Ok(match renewal(supervisor, home, state_dir, pid, &kept) {
            Some(Renewal::Reload(why)) => SyncNodeState::Due {
                from: why,
                to: target,
            },
            Some(Renewal::Restart) => SyncNodeState::Due {
                from: format!(
                    "{NODE_LABEL} (pid {pid}) runs a build an update replaced ({} is another \
                     file now)",
                    kept.display()
                ),
                to: format!("{target}, renewed with kickstart -k"),
            },
            None => {
                SyncNodeState::Current(format!("{NODE_LABEL} runs (pid {pid}, {})", kept.display()))
            }
        }),
        NodeJobState::Installed {
            pid, program: kept, ..
        } => {
            let from = match (pid, kept) {
                (Some(pid), Some(kept)) => {
                    format!("{NODE_LABEL} runs {} (pid {pid})", kept.display())
                }
                (Some(pid), None) => format!("{NODE_LABEL} runs (pid {pid}, program unread)"),
                (None, Some(kept)) => {
                    format!(
                        "{NODE_LABEL} is installed ({}) and not running",
                        kept.display()
                    )
                }
                (None, None) => format!("{NODE_LABEL} is installed and not running"),
            };
            Ok(SyncNodeState::Due { from, to: target })
        }
    }
}

/// What a job that runs its unit's program still needs ([`renewal`]).
enum Renewal {
    /// Its unit must be written and loaded again (it sits in the wrong domain, or its plist is
    /// not the one this build writes): why.
    Reload(String),
    /// It runs a build an update replaced: a restart in place.
    Restart,
}

/// What the job `pid`, running `program` as its unit says, still needs, `None` where nothing.
/// A plist an earlier build wrote is rewritten, so a fix to the unit reaches jobs already
/// installed and not only new ones.
fn renewal(
    supervisor: &dyn ServiceSupervisor,
    home: &Path,
    state_dir: &Path,
    pid: u32,
    program: &Path,
) -> Option<Renewal> {
    if let Some(why) = supervisor.misplaced() {
        return Some(Renewal::Reload(why));
    }
    let unit = supervisor.unit_file(home);
    if std::fs::read_to_string(&unit).ok() != Some(supervisor.render_unit(program, home, state_dir))
    {
        return Some(Renewal::Reload(format!(
            "{} is not the unit this build writes",
            unit.display()
        )));
    }
    supervisor
        .runs_replaced(pid, program)
        .then_some(Renewal::Restart)
}

/// Apply `sync.node` via `supervisor`.
///
/// # Errors
/// Returns an error if installing or starting `systems.alab.aterm-node` fails.
pub fn ensure_for_sync_with(
    home: &Path,
    state_dir: &Path,
    aterm_config: Option<&Path>,
    program: Option<&Path>,
    under_ci: bool,
    supervisor: &dyn ServiceSupervisor,
) -> Result<String, String> {
    match read_for_sync_with(home, state_dir, aterm_config, program, under_ci, supervisor)? {
        SyncNodeState::NotApplicable(why) => Ok(format!("left as it is: {why}")),
        SyncNodeState::Current(now) => Ok(format!("nothing changed: {now}")),
        SyncNodeState::Due { .. } => {
            let Some(program) = program else {
                return Ok("left as it is: no installed release aterm binary".to_owned());
            };
            // A job that runs as its unit says, on a build an update replaced, restarts in place;
            // anything else is (re)installed, which reloads it where it belongs.
            if let NodeJobState::Installed {
                pid: Some(pid),
                program: Some(kept),
                ..
            } = state_with(home, supervisor)?
                && kept == program
                && matches!(
                    renewal(supervisor, home, state_dir, pid, &kept),
                    Some(Renewal::Restart)
                )
            {
                supervisor.renew()?;
                return Ok(format!("renewed {NODE_LABEL} on this build (kickstart -k)"));
            }
            let unit = install_with(program, home, state_dir, false, supervisor)?;
            Ok(format!(
                "installed and started {NODE_LABEL} ({})",
                unit.display()
            ))
        }
    }
}

/// CLI text for `aterm node --help`.
pub const NODE_USAGE: &str = "\
usage: aterm node <command>

commands:
  serve    run the fleet/swarm node job in the foreground (launchd/systemd entry)
  status   show systems.alab.aterm-node installation and process status
  on       install and start systems.alab.aterm-node for this login
  off      stop and remove systems.alab.aterm-node (keeps sync.node off)
";

/// Run `aterm node status | on | off` (everything except `serve`, which `aterm` composes with
/// `aterm-db` and `aterm-gui`).
///
/// # Errors
/// Returns `(message, exit_code)` on usage or runtime failure.
pub fn run_admin_subcommand(
    subcmd: &str,
    rest: &[String],
    home: &Path,
    state_dir: &Path,
    current_exe: &Path,
    supervisor: &dyn ServiceSupervisor,
) -> Result<String, (String, u8)> {
    if !rest.is_empty() {
        return Err((
            format!(
                "aterm node {subcmd}: unexpected argument {:?}\n{NODE_USAGE}",
                rest[0]
            ),
            2,
        ));
    }
    match subcmd {
        "status" => {
            let st =
                state_with(home, supervisor).map_err(|e| (format!("aterm node status: {e}"), 1))?;
            let lock_held = {
                #[cfg(unix)]
                {
                    node_job_holds_lock(state_dir)
                }
                #[cfg(not(unix))]
                {
                    let _ = state_dir;
                    false
                }
            };
            let off = node_turned_off(state_dir);
            let mut out = match st {
                NodeJobState::Absent => format!("{NODE_LABEL}: not installed\n"),
                NodeJobState::Installed {
                    pid: Some(pid),
                    unit,
                    program,
                } => format!(
                    "{NODE_LABEL}: running (pid {pid})\n  unit: {}\n  program: {}\n",
                    unit.display(),
                    program.map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
                ),
                NodeJobState::Installed {
                    pid: None,
                    unit,
                    program,
                } => format!(
                    "{NODE_LABEL}: installed, not running\n  unit: {}\n  program: {}\n",
                    unit.display(),
                    program.map_or_else(|| "-".to_owned(), |p| p.display().to_string()),
                ),
            };
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!(
                    "  lock: {}\n  sync: {}\n",
                    if lock_held { "held" } else { "free" },
                    if off { "off (node-off)" } else { "on" }
                ),
            );
            Ok(out)
        }
        "on" => {
            let unit = install_with(current_exe, home, state_dir, true, supervisor)
                .map_err(|e| (format!("aterm node on: {e}"), 1))?;
            Ok(format!(
                "{NODE_LABEL}: installed and started ({})\n",
                unit.display()
            ))
        }
        "off" => {
            let removed = remove_with(home, state_dir, true, supervisor)
                .map_err(|e| (format!("aterm node off: {e}"), 1))?;
            Ok(match removed {
                Some(unit) => format!("{NODE_LABEL}: stopped and removed ({})\n", unit.display()),
                None => format!(
                    "{NODE_LABEL}: off (recorded in {})\n",
                    off_path(state_dir).display()
                ),
            })
        }
        other => Err((
            format!("aterm node: unknown command {other:?}\n{NODE_USAGE}"),
            2,
        )),
    }
}

#[cfg(test)]
#[path = "node_service_tests.rs"]
mod tests;
