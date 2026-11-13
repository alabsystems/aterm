// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launchd (macOS) and systemd user (Linux) jobs `aterm secrets` installs: the subscriber
//! and the server.

use crate::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Which job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// `aterm secrets watch`.
    Subscriber,
    /// `aterm secrets serve …`.
    Server,
}

impl Job {
    /// The launchd label / systemd unit stem.
    pub const fn label(self) -> &'static str {
        match self {
            Job::Subscriber => "systems.alab.aterm-secrets",
            Job::Server => "systems.alab.aterm-secrets-server",
        }
    }

    /// One line naming the job: a systemd unit's `Description`.
    pub const fn description(self) -> &'static str {
        match self {
            Job::Subscriber => "aterm secrets subscriber (aterm secrets watch)",
            Job::Server => "aterm secrets server (aterm secrets serve)",
        }
    }
}

/// A job's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not installed.
    Absent,
    /// Installed; running with this pid, or not running.
    Installed {
        /// The pid, when running.
        pid: Option<u32>,
        /// The unit file.
        unit: PathBuf,
    },
}

/// The node service's launchd label and systemd unit stem, which `aterm secrets status` reads:
/// its vault pass runs there from R2 (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.13). aterm-agent
/// installs it.
pub const NODE_LABEL: &str = "systems.alab.aterm-node";

/// The launchctl binary.
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// Where `systemctl` may be, tried in order: never a `PATH` lookup.
pub const SYSTEMCTL_PATHS: [&str; 3] = [
    "/usr/bin/systemctl",
    "/bin/systemctl",
    "/run/current-system/sw/bin/systemctl",
];

/// Where `loginctl` may be, tried in order: never a `PATH` lookup.
pub const LOGINCTL_PATHS: [&str; 3] = [
    "/usr/bin/loginctl",
    "/bin/loginctl",
    "/run/current-system/sw/bin/loginctl",
];

/// Where systemd records that a user lingers: a file named for them.
#[cfg(not(target_os = "macos"))]
const LINGER_DIR: &str = "/var/lib/systemd/linger";

/// A minute hang detector for a required local service-manager action or observation.
const CHILD_HANG: Duration = Duration::from_secs(60);

/// Timeout waiting for launchd to forget a booted-out job ([`install`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
const BOOTOUT_SETTLE: Duration = Duration::from_secs(60);

/// The job's launchd property list: `~/Library/LaunchAgents/<label>.plist`.
pub fn plist_path(home: &Path, job: Job) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", job.label()))
}

/// The job's systemd user unit: `~/.config/systemd/user/<label>.service`.
pub fn unit_path(home: &Path, job: Job) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(unit_name(job))
}

/// The job's log (launchd's stdout and stderr): `~/.config/aterm/secrets/<label>.log`.
pub fn log_path(home: &Path, job: Job) -> PathBuf {
    secrets_dir(home).join(format!("{}.log", job.label()))
}

/// The system LaunchDaemon label on always-on members (`aterm machines apply`).
pub const DAEMON_LABEL: &str = "systems.alab.aterm-vault-server";

/// Where the system LaunchDaemon's property list sits on macOS.
pub const DAEMON_PLIST: &str = "/Library/LaunchDaemons/systems.alab.aterm-vault-server.plist";

/// Where the system LaunchDaemon stores served envelopes on macOS.
pub const DAEMON_STORE: &str = "/Library/Application Support/aterm/vault-server/served";

/// Where launchd writes the system LaunchDaemon's stdout and stderr on macOS.
pub const DAEMON_LOG: &str = "/Library/Logs/aterm/systems.alab.aterm-vault-server.log";

/// The arguments (after the `aterm` executable) of the system LaunchDaemon: `secrets serve`
/// pinned to [`crate::ALAB_SIGNER`], advertising `advertise` (`_alab-vault._tcp` under the host's
/// own label) or `--no-advertise` when `None` so an unlabelled daemon never contends for
/// `alab-secrets.local`.
#[must_use]
pub fn daemon_serve_args(store: &Path, advertise: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "secrets".into(),
        "serve".into(),
        "--store".into(),
        store.to_string_lossy().into_owned(),
        "--pin".into(),
        format!("{}={}", crate::ALAB_VAULT, crate::ALAB_SIGNER),
    ];
    match advertise.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => {
            args.push("--advertise".into());
            args.push(name.into());
        }
        None => args.push("--no-advertise".into()),
    }
    args
}

/// The launchd property list for [`DAEMON_LABEL`] in `system`: runs at boot before any user logs
/// in (no `LimitLoadToSessionType`, no per-login `HOME`), kept alive as a Background process.
#[must_use]
pub fn daemon_plist_text(
    program: &Path,
    args: &[String],
    log: &Path,
    bundle_id: Option<&str>,
) -> String {
    plist_text_in(DAEMON_LABEL, program, args, log, Domain::Gui, bundle_id)
}

/// The record that the person turned this login's subscriber off:
/// `~/.config/aterm/secrets/subscriber-off`. `agent off` writes it, and so does a `join` or
/// `create` with `--no-agent` where no subscriber runs; an [`install`] of the subscriber
/// removes it. aterm's setup (System State, `aterm init`) reads it as the person's choice: Sync
/// shows off, and the files stay as last synced with no subscriber installed.
pub fn subscriber_off_path(home: &Path) -> PathBuf {
    secrets_dir(home).join("subscriber-off")
}

/// Whether the person turned this login's subscriber off ([`subscriber_off_path`]). A record
/// that cannot be read is no record.
pub fn subscriber_turned_off(home: &Path) -> bool {
    crate::fsx::kind(&subscriber_off_path(home)).is_ok_and(|kind| kind != crate::fsx::Kind::Missing)
}

/// Record that the person turned this login's subscriber off ([`subscriber_off_path`]), in
/// the 0700 `~/.config/aterm/secrets`.
pub fn record_subscriber_off(home: &Path) -> crate::Result<()> {
    let _decision = JobDecision::take(home, Job::Subscriber)?;
    record_subscriber_off_locked(home)
}

fn record_subscriber_off_locked(home: &Path) -> crate::Result<()> {
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    crate::fsx::write_atomic(
        &subscriber_off_path(home),
        b"the subscriber was turned off by choice; aterm secrets agent on turns it back on\n",
        0o600,
    )
}

/// The record that the person turned this login's vault server off:
/// `~/.config/aterm/secrets/server-off`. `server off` writes it, and an [`install`] of the server
/// removes it, so automatic setup (`sync.steward.server.standby`) and [`move_held`] leave the
/// server off by choice.
pub fn server_off_path(home: &Path) -> PathBuf {
    secrets_dir(home).join("server-off")
}

/// Whether the person turned this login's vault server off ([`server_off_path`]). A record that
/// cannot be read is no record.
pub fn server_turned_off(home: &Path) -> bool {
    crate::fsx::kind(&server_off_path(home)).is_ok_and(|kind| kind != crate::fsx::Kind::Missing)
}

/// Record that the person turned this login's vault server off ([`server_off_path`]), in the 0700
/// `~/.config/aterm/secrets`.
pub fn record_server_off(home: &Path) -> crate::Result<()> {
    let _decision = JobDecision::take(home, Job::Server)?;
    record_server_off_locked(home)
}

fn record_server_off_locked(home: &Path) -> crate::Result<()> {
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    crate::fsx::write_atomic(
        &server_off_path(home),
        b"the server was turned off by choice; aterm secrets server on turns it back on\n",
        0o600,
    )
}

/// Orders a job's local on/off decision and service changes across processes
/// ([`decision_lock_path`]): an `on`, an `off`, a repair and a mend's move of the same job never
/// interleave, so a move ([`move_held`]) never undoes an `off` or a pin that came while the mend
/// judged the job. It is separate from the vault's data lock, and spans a fetch only in
/// [`move_held_serving`], which holds it until the moved job is seen serving (up to a minute) or
/// is put back, so an `off` meanwhile waits for that and then stands.
pub(crate) struct JobDecision(std::fs::File);

/// The lock [`JobDecision`] takes for `job`: `~/.config/aterm/secrets/subscriber.lock`, the one
/// earlier releases take for the subscriber, or `server.lock`.
fn decision_lock_path(home: &Path, job: Job) -> PathBuf {
    secrets_dir(home).join(match job {
        Job::Subscriber => "subscriber.lock",
        Job::Server => "server.lock",
    })
}

impl JobDecision {
    pub(crate) fn take(home: &Path, job: Job) -> crate::Result<Self> {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        crate::fsx::ensure_private_dir(&secrets_dir(home))?;
        let path = decision_lock_path(home, job);
        let file: std::fs::File = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&path)
        })?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.uid() != aterm_uds::peer::our_uid() || meta.nlink() != 1 {
            return Err(Error::new(format!(
                "{}: not an owned regular lock file",
                path.display()
            )));
        }
        if meta.mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of JobDecision, whose Drop releases the local service decision lock"
        )]
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for JobDecision {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// launchd's restart window: a `KeepAlive` job that ended is started again within
/// its throttle interval (10 s by default), so a subscriber read as installed but
/// not running may only be between two runs until it has stayed down past this.
pub const RESTART_WINDOW: Duration = Duration::from_secs(11);

/// How often a job read as not running is read again within [`RESTART_WINDOW`].
pub const RESTART_POLL: Duration = Duration::from_millis(500);

/// Whether a job read as not running runs again within launchd's restart window:
/// `running` is read every [`RESTART_POLL`] until it says so, or until
/// [`RESTART_WINDOW`] has passed since `down_since`, when the job was first seen
/// down (`now`, when unset). `down_since` keeps that moment for a later read, so a
/// job already down past the window is not waited on again.
/// # Errors
/// A read of `running` failed.
pub fn back_within_restart_window<E>(
    down_since: &mut Option<Instant>,
    now: &mut dyn FnMut() -> Instant,
    running: &mut dyn FnMut() -> Result<bool, E>,
    pause: &mut dyn FnMut(Duration),
) -> Result<bool, E> {
    let start = *down_since.get_or_insert_with(&mut *now);
    loop {
        let at = now();
        let Some(rem) = RESTART_WINDOW.checked_sub(at.saturating_duration_since(start)) else {
            return Ok(false);
        };
        if rem.is_zero() {
            return Ok(false);
        }
        pause(RESTART_POLL.min(rem));
        if running()? {
            return Ok(true);
        }
    }
}

/// Remove the person's record that the subscriber is off: it is on again.
fn clear_subscriber_off(home: &Path) -> crate::Result<()> {
    let path = subscriber_off_path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// Remove the person's record that the server is off: it is on again.
fn clear_server_off(home: &Path) -> crate::Result<()> {
    let path = server_off_path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// `~/.config/aterm/secrets`.
fn secrets_dir(home: &Path) -> PathBuf {
    home.join(".config").join("aterm").join("secrets")
}

/// `<label>.service`.
fn unit_name(job: Job) -> String {
    format!("{}.service", job.label())
}

/// The home a log at [`log_path`] lies under: `<home>` of `<home>/.config/aterm/secrets/<file>`.
fn home_of_log(log: &Path) -> Option<&Path> {
    let dir = log.parent()?;
    if !dir.ends_with(Path::new(".config").join("aterm").join("secrets")) {
        return None;
    }
    let home = dir.parent()?.parent()?.parent()?;
    home.is_absolute().then_some(home)
}

/// The launchd property list for a job, setting `HOME` when `log` is a [`log_path`]: the plist
/// [`install`] writes for the login at the Mac's screen, naming no app ([`plist_text_in`]).
pub fn plist_text(label: &str, program: &Path, args: &[String], log: &Path) -> String {
    plist_text_in(label, program, args, log, Domain::Gui, None)
}

/// [`plist_text`] for a job in `domain` (its `LimitLoadToSessionType`, [`Domain::session`]),
/// attributed to the app `bundle_id` names (`AssociatedBundleIdentifiers`: Login Items and the
/// background-item notice name that app, not the program's file).
pub fn plist_text_in(
    label: &str,
    program: &Path,
    args: &[String],
    log: &Path,
    domain: Domain,
    bundle_id: Option<&str>,
) -> String {
    plist_text_in_with_subscriber_process_type(
        label,
        program,
        args,
        log,
        domain,
        bundle_id,
        SubscriberProcessType::Interactive,
    )
}

/// The subscriber's process policy. Interactive permits the early per-thread housekeeping
/// hold to lift a books-lock holder; Background retains the clamp for an older image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriberProcessType {
    Interactive,
    Background,
}

/// Render a selected subscriber policy; server jobs always retain Background and Nice 5.
/// Installation chooses Interactive only for the currently loaded, still-bound executable.
pub fn plist_text_in_with_subscriber_process_type(
    label: &str,
    program: &Path,
    args: &[String],
    log: &Path,
    domain: Domain,
    bundle_id: Option<&str>,
    subscriber: SubscriberProcessType,
) -> String {
    plist_text_with_path(
        label,
        program,
        args,
        log,
        domain,
        bundle_id,
        LaunchdPolicy {
            path: LaunchdPath::Managed,
            subscriber,
        },
    )
}

/// A service either inherits launchd's PATH or explicitly names the managed tools.
/// Replacing its executable preserves that choice; new installations use the managed path.
#[derive(Clone, Copy)]
enum LaunchdPath {
    Inherited,
    Managed,
}

/// The property list of a login job that runs until it says it is done: `RunAtLoad` and
/// `KeepAlive {SuccessfulExit: false}`, so launchd starts it at each login (an automatic one
/// after a restart included) and starts it again after any end but its own exit 0 (a crash, a
/// kill, jetsam), with its throttle, and never once it exits 0. It runs in the login at the Mac's
/// screen ([`Domain::Gui`]) with `HOME` set to `home` and the managed PATH, attributed to the app
/// `bundle_id` names, at the standard pace (`ProcessType Standard`, no `Nice`): its work (clones,
/// installs) is what a window would do, which background throttling would stretch past the
/// limits it is held to.
pub fn plist_text_until_done(
    label: &str,
    program: &Path,
    args: &[String],
    log: &Path,
    home: &Path,
    bundle_id: Option<&str>,
) -> String {
    plist_text_shaped(
        label,
        program,
        args,
        log,
        Some(home),
        Domain::Gui,
        bundle_id,
        // The subscriber's process type is read only by a job kept alive always.
        LaunchdPolicy {
            path: LaunchdPath::Managed,
            subscriber: SubscriberProcessType::Background,
        },
        KeepAlive::UntilDone,
    )
}

/// When launchd starts a loaded job again, and at what pace it runs.
#[derive(Clone, Copy)]
enum KeepAlive {
    /// Whenever it is not running: a service that waits on others most of its life, throttled as
    /// background work (`ProcessType Background`, `Nice` 5), but for a subscriber whose policy
    /// is Interactive ([`SubscriberProcessType`]).
    Always,
    /// Whenever it ended otherwise than by its own exit 0: work that runs until it is done, at
    /// the standard pace.
    UntilDone,
}

#[derive(Clone, Copy)]
struct LaunchdPolicy {
    path: LaunchdPath,
    subscriber: SubscriberProcessType,
}

fn plist_text_with_path(
    label: &str,
    program: &Path,
    args: &[String],
    log: &Path,
    domain: Domain,
    bundle_id: Option<&str>,
    policy: LaunchdPolicy,
) -> String {
    plist_text_shaped(
        label,
        program,
        args,
        log,
        home_of_log(log),
        domain,
        bundle_id,
        policy,
        KeepAlive::Always,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one plist's every key, each its caller's choice"
)]
fn plist_text_shaped(
    label: &str,
    program: &Path,
    args: &[String],
    log: &Path,
    home: Option<&Path>,
    domain: Domain,
    bundle_id: Option<&str>,
    policy: LaunchdPolicy,
    keep_alive: KeepAlive,
) -> String {
    let (keep_alive, scheduling) = match keep_alive {
        KeepAlive::Always
            if label == Job::Subscriber.label()
                && policy.subscriber == SubscriberProcessType::Interactive =>
        {
            (
                "\t<true/>\n",
                "\t<key>ProcessType</key>\n\t<string>Interactive</string>\n",
            )
        }
        KeepAlive::Always => (
            "\t<true/>\n",
            "\t<key>ProcessType</key>\n\t<string>Background</string>\n\t<key>Nice</key>\n\t<integer>5</integer>\n",
        ),
        KeepAlive::UntilDone => (
            "\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n",
            "\t<key>ProcessType</key>\n\t<string>Standard</string>\n",
        ),
    };
    let string = |s: &str| format!("<string>{}</string>", aterm_codec::xml::escape(s));
    let mut argv = String::new();
    for arg in
        std::iter::once(program.to_string_lossy().as_ref()).chain(args.iter().map(String::as_str))
    {
        argv.push_str(&format!("\t\t{}\n", string(arg)));
    }
    let environment = home.map_or_else(String::new, |home| {
        let path = match policy.path {
            LaunchdPath::Inherited => String::new(),
            LaunchdPath::Managed => {
                let pkg_bin = home.join("Library/Application Support/aterm/pkg/bin");
                let value = format!(
                    "{}:/usr/bin:/bin:/usr/sbin:/sbin",
                    pkg_bin.to_string_lossy()
                );
                format!("\t\t<key>PATH</key>\n\t\t{}\n", string(&value))
            }
        };
        format!(
            "\t<key>EnvironmentVariables</key>\n\
             \t<dict>\n\
             \t\t<key>HOME</key>\n\
             \t\t{}\n\
             {path}\
             \t</dict>\n",
            string(&home.to_string_lossy()),
        )
    });
    let session = domain.session().map_or_else(String::new, |session| {
        format!(
            "\t<key>LimitLoadToSessionType</key>\n\t{}\n",
            string(session)
        )
    });
    let associated = bundle_id.map_or_else(String::new, |id| {
        format!(
            "\t<key>AssociatedBundleIdentifiers</key>\n\t<array>\n\t\t{}\n\t</array>\n",
            string(id)
        )
    });
    let log = string(&log.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{label}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {argv}\
         \t</array>\n\
         {session}\
         {associated}\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         {keep_alive}\
         {scheduling}\
         \t<key>SoftResourceLimits</key>\n\
         \t<dict>\n\
         \t\t<key>NumberOfFiles</key>\n\
         \t\t<integer>4096</integer>\n\
         \t</dict>\n\
         {environment}\
         \t<key>StandardOutPath</key>\n\
         \t{log}\n\
         \t<key>StandardErrorPath</key>\n\
         \t{log}\n\
         </dict>\n\
         </plist>\n",
        label = string(label),
    )
}

/// One argv element as systemd's `ExecStart` reads it back exactly.
pub fn systemd_word(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('"');
    for c in word.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c if c.is_control() && u32::from(c) < 0x80 => {
                out.push_str(&format!("\\x{:02x}", u32::from(c)));
            }
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A unit's free-text value (`Description`): one line, `%` doubled, no trailing `\`.
fn systemd_text(text: &str) -> String {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace('%', "%%");
    line.trim_end_matches('\\').to_string()
}

/// Render the systemd user unit for a job ([`systemd_word`]).
pub fn systemd_unit(description: &str, program: &Path, args: &[String]) -> String {
    let mut exec = systemd_word(&program.to_string_lossy());
    for arg in args {
        exec.push(' ');
        exec.push_str(&systemd_word(arg));
    }
    format!(
        "[Unit]\n\
         Description={description}\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         Restart=always\n\
         RestartSec=5\n\
         LimitNOFILE=4096\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        description = systemd_text(description),
    )
}

/// The largest unit file [`unit_program`], [`server_repair_due`] or [`held_at`] reads.
const UNIT_FILE_MOST: usize = 256 * 1024;

/// Read a regular file (following symlinks such as NixOS unit links, never blocking on a FIFO) up
/// to `max` bytes.
fn read_regular_bounded(path: &Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() || meta.len() > max as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    let limit = (max as u64).saturating_add(1);
    file.take(limit).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= max).then_some(bytes)
}

fn read_regular_string(path: &Path, max: usize) -> Option<String> {
    String::from_utf8(read_regular_bounded(path, max)?).ok()
}

/// The program `unit`, a job's plist on macOS or its systemd unit elsewhere, starts: the first
/// `ProgramArguments` string or `ExecStart` word, as [`plist_text`] and [`systemd_unit`] write
/// them. `None` for a file it cannot read or a form they never write.
pub fn unit_program(unit: &Path) -> Option<PathBuf> {
    let text = read_regular_string(unit, UNIT_FILE_MOST)?;
    if cfg!(target_os = "macos") {
        plist_program(&text)
    } else {
        exec_program(&text)
    }
}

/// [`unit_program`] of a plist's text.
pub fn plist_program(text: &str) -> Option<PathBuf> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    let after = after.trim_start().strip_prefix("<array>")?;
    let (escaped, _) = after
        .trim_start()
        .strip_prefix("<string>")?
        .split_once("</string>")?;
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (entity, c) = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find(|(entity, _)| rest.starts_with(entity))?;
        out.push(c);
        rest = &rest[entity.len()..];
    }
    out.push_str(rest);
    Some(PathBuf::from(out))
}

/// Every `<string>` of a plist's `ProgramArguments` array, its XML entities read back: the
/// program [`plist_program`] names and its arguments, as [`plist_text`] writes them. `None`
/// for a form it never writes.
pub fn plist_argv(text: &str) -> Option<Vec<String>> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    let mut rest = after.trim_start().strip_prefix("<array>")?;
    let mut argv = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with("</array>") {
            return (!argv.is_empty()).then_some(argv);
        }
        let (escaped, tail) = rest.strip_prefix("<string>")?.split_once("</string>")?;
        let mut word = String::with_capacity(escaped.len());
        let mut left = escaped;
        while let Some(at) = left.find('&') {
            word.push_str(&left[..at]);
            left = &left[at..];
            let (entity, c) = [
                ("&amp;", '&'),
                ("&lt;", '<'),
                ("&gt;", '>'),
                ("&quot;", '"'),
                ("&apos;", '\''),
            ]
            .into_iter()
            .find(|(entity, _)| left.starts_with(entity))?;
            word.push(c);
            left = &left[entity.len()..];
        }
        word.push_str(left);
        argv.push(word);
        rest = tail;
    }
}

/// Every word of a systemd unit's `ExecStart`, as [`systemd_unit`] writes them (each quoted by
/// [`systemd_word`], one space apart): the program [`exec_program`] names and its arguments.
/// `None` for a form it never writes.
pub fn exec_argv(text: &str) -> Option<Vec<String>> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))?;
    let mut chars = line.chars();
    let mut argv = Vec::new();
    loop {
        if chars.next()? != '"' {
            return None;
        }
        let mut word = String::new();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    c @ ('\\' | '"') => word.push(c),
                    _ => return None,
                },
                c @ ('%' | '$') => {
                    if chars.next()? != c {
                        return None;
                    }
                    word.push(c);
                }
                c => word.push(c),
            }
        }
        argv.push(word);
        match chars.next() {
            None => return Some(argv),
            Some(' ') => {}
            Some(_) => return None,
        }
    }
}

/// [`unit_program`] of a systemd unit's text: its `ExecStart`'s first word, which
/// [`systemd_word`] quoted (a program holds no control character, [`check_argv`]).
pub fn exec_program(text: &str) -> Option<PathBuf> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))?;
    let mut chars = line.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(PathBuf::from(out)),
            '\\' => match chars.next()? {
                c @ ('\\' | '"') => out.push(c),
                _ => return None,
            },
            c @ ('%' | '$') => {
                if chars.next()? != c {
                    return None;
                }
                out.push(c);
            }
            c => out.push(c),
        }
    }
}

/// Which of this user's launchd domains a job lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// `gui/<uid>`: the login at the Mac's screen, there only while it lasts; launchd loads the
    /// job again at each such login, an automatic one after a restart included.
    Gui,
    /// `user/<uid>`: this user's background domain, there with no login at the screen (an SSH
    /// session's): the job runs until the Mac restarts, and no login loads it again.
    User,
}

impl Domain {
    /// The domain's `launchctl` name for `uid`.
    pub fn target(self, uid: u32) -> String {
        match self {
            Domain::Gui => format!("gui/{uid}"),
            Domain::User => format!("user/{uid}"),
        }
    }

    /// The `LimitLoadToSessionType` a plist needs for `launchctl bootstrap` to load it here:
    /// `Background` for [`Domain::User`] (launchd refuses any other there: "Bootstrap failed: 5:
    /// Input/output error"), which also keeps a login at the screen from loading a second copy
    /// that the first holds the port of; none (`Aqua`) for [`Domain::Gui`].
    pub const fn session(self) -> Option<&'static str> {
        match self {
            Domain::Gui => None,
            Domain::User => Some("Background"),
        }
    }

    /// The other domain.
    #[cfg(any(target_os = "macos", all(test, unix)))]
    const fn other(self) -> Self {
        match self {
            Domain::Gui => Domain::User,
            Domain::User => Domain::Gui,
        }
    }
}

/// What a `launchctl` call does, in which of this user's domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launchctl<'a> {
    /// `print <domain>`: whether the domain is there (`gui/<uid>` only with a login at the
    /// Mac's screen).
    PrintDomain(Domain),
    /// `print <domain>/<label>`: read the job's state.
    Print(Domain, &'a str),
    /// `bootout <domain>/<label>`: stop the job and unload it.
    Bootout(Domain, &'a str),
    /// `bootstrap <domain> <plist>`: load (and with `RunAtLoad`, start) the job a plist defines.
    Bootstrap(Domain, &'a Path),
    /// `kickstart <domain>/<label>`: start the job unless it runs.
    Kickstart(Domain, &'a str),
    /// `kickstart -k <domain>/<label>`: start the job, killing a running instance first.
    Restart(Domain, &'a str),
}

/// The argv (after `/bin/launchctl`) of one call in one of the domains of the user `uid`.
pub fn launchctl_argv(uid: u32, call: Launchctl<'_>) -> Vec<String> {
    let job = |domain: Domain, label: &str| format!("{}/{label}", domain.target(uid));
    match call {
        Launchctl::PrintDomain(domain) => vec!["print".into(), domain.target(uid)],
        Launchctl::Print(domain, label) => vec!["print".into(), job(domain, label)],
        Launchctl::Bootout(domain, label) => vec!["bootout".into(), job(domain, label)],
        Launchctl::Bootstrap(domain, plist) => vec![
            "bootstrap".into(),
            domain.target(uid),
            plist.to_string_lossy().into_owned(),
        ],
        Launchctl::Kickstart(domain, label) => vec!["kickstart".into(), job(domain, label)],
        Launchctl::Restart(domain, label) => {
            vec!["kickstart".into(), "-k".into(), job(domain, label)]
        }
    }
}

/// What a `launchctl` call does in `system` (a root LaunchDaemon's domain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemLaunchctl<'a> {
    /// `print system/<label>`: read the daemon's state.
    Print(&'a str),
    /// `bootout system/<label>`: stop the daemon and unload it.
    Bootout(&'a str),
    /// `bootstrap system <plist>`: load (and with `RunAtLoad`, start) the daemon a plist defines.
    Bootstrap(&'a Path),
    /// `kickstart system/<label>`: start the daemon unless it runs.
    Kickstart(&'a str),
    /// `kickstart -k system/<label>`: start the daemon, killing a running instance first.
    Restart(&'a str),
}

/// The argv (after `/bin/launchctl`) of one call in `system`.
#[must_use]
pub fn system_launchctl_argv(call: SystemLaunchctl<'_>) -> Vec<String> {
    let job = |label: &str| format!("system/{label}");
    match call {
        SystemLaunchctl::Print(label) => vec!["print".into(), job(label)],
        SystemLaunchctl::Bootout(label) => vec!["bootout".into(), job(label)],
        SystemLaunchctl::Bootstrap(plist) => vec![
            "bootstrap".into(),
            "system".into(),
            plist.to_string_lossy().into_owned(),
        ],
        SystemLaunchctl::Kickstart(label) => vec!["kickstart".into(), job(label)],
        SystemLaunchctl::Restart(label) => vec!["kickstart".into(), "-k".into(), job(label)],
    }
}

/// The argv (after `systemctl`) of one call to this user's manager: `--user` and `words`.
pub fn systemctl_argv(words: &[&str]) -> Vec<String> {
    std::iter::once("--user")
        .chain(words.iter().copied())
        .map(str::to_string)
        .collect()
}

/// The running pid `launchctl print`'s text names: the service's own `pid = <n>` line.
pub fn print_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let top = line.strip_prefix('\t')?;
        let pid = top.strip_prefix("pid = ")?.trim_end();
        (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
            .then(|| pid.parse().ok())
            .flatten()
            .filter(|pid| *pid > 0)
    })
}

/// Whether a system LaunchDaemon is running, read without changing it. An unloaded job or a
/// successful print without a PID is stopped; a failed or timed-out read is unknown. Off macOS
/// no launchd job is read. This read belongs on a setup worker, never the window thread.
#[must_use]
pub fn system_job_running(label: &str) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let mut command = aterm_uds::spawnfd::command(LAUNCHCTL);
        command.args(system_launchctl_argv(SystemLaunchctl::Print(label)));
        let output =
            aterm_uds::deadline::output_within(&mut command, Duration::from_secs(5)).ok()?;
        system_job_running_of(
            output.status.code(),
            &String::from_utf8(output.stdout).ok()?,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = label;
        None
    }
}

#[cfg(any(target_os = "macos", test))]
fn system_job_running_of(code: Option<i32>, text: &str) -> Option<bool> {
    match code {
        Some(0) => Some(print_pid(text).is_some()),
        Some(113) => Some(false),
        _ => None,
    }
}

/// The pid `systemctl --user show -p MainPID --value` printed.
pub fn main_pid(text: &str) -> Option<u32> {
    let text = text.trim();
    let text = text.strip_prefix("MainPID=").unwrap_or(text);
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
        .filter(|pid| *pid > 0)
}

/// The first of `candidates` that exists: `systemctl`, or a clear refusal when this Linux has none.
pub fn systemctl_path(
    candidates: &[&str],
    exists: &dyn Fn(&Path) -> bool,
) -> crate::Result<PathBuf> {
    candidates
        .iter()
        .map(Path::new)
        .find(|path| exists(path))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::new(format!(
                "no systemctl at {}: this system has no systemd user manager to keep the job running; \
                 run `aterm secrets watch` (or `serve`) under your own supervisor instead",
                candidates.join(" or ")
            ))
        })
}

/// Linux: whether a `--user` job set up from this session needs systemd to let this user
/// linger. Without it systemd ends the user's manager, and every job under it, once their last
/// session ends, and starts none at boot. A remote session (`SSH_CONNECTION`, `SSH_TTY`) or one
/// with no display (a headless box) is the kind whose end leaves nobody logged in; a desktop's
/// user logs in again, which starts the jobs. `env` reads this process's environment.
pub fn needs_linger(env: &dyn Fn(&str) -> Option<String>, lingering: bool) -> bool {
    let set = |key: &str| env(key).is_some_and(|value| !value.is_empty());
    let remote = set("SSH_CONNECTION") || set("SSH_TTY");
    let headless = !set("DISPLAY") && !set("WAYLAND_DISPLAY");
    !lingering && (remote || headless)
}

/// The one line [`install`] adds on Linux after asking systemd to let `user` linger: what
/// `enabled` (the `loginctl enable-linger` it ran) means for the job.
pub fn linger_line(user: &str, enabled: crate::Result<()>) -> String {
    match enabled {
        Ok(()) => format!(
            "loginctl enable-linger {user}: the job keeps running once this session ends, and \
             starts at boot"
        ),
        Err(e) => format!(
            "the job stops once {user}'s last session ends ({e}); an admin keeps it running \
             with: sudo loginctl enable-linger {user}"
        ),
    }
}

/// Whether `path` is in a cargo build directory.
pub fn in_build_dir(path: &Path, exists: &dyn Fn(&Path) -> bool) -> bool {
    let names: Vec<&std::ffi::OsStr> = path.iter().collect();
    let mut dir = PathBuf::new();
    for (index, name) in names.iter().enumerate().take(names.len().saturating_sub(1)) {
        let text = name.to_string_lossy();
        let after = |offset: usize| {
            names
                .get(index + offset)
                .map(|n| n.to_string_lossy().into_owned())
        };
        if text == "target" {
            if exists(&dir.join("Cargo.toml")) {
                return true;
            }
            let profile = |n: Option<String>| n.is_some_and(|n| n == "debug" || n == "release");
            if profile(after(1)) || (index + 2 < names.len() && profile(after(2))) {
                return true;
            }
        }
        if text.ends_with(".noindex")
            && (text.contains("target") || after(1).is_some_and(|n| n == "target"))
        {
            return true;
        }
        // Cargo's own layout, wherever its build directory is (`build.build-dir`, as atpkg's
        // targo shim sets it): a profile's `deps` (the old layout) or `build` (the new one,
        // `build/<package>/<hash>/out/`) holds the test binaries and the units, never an
        // installed program.
        if (text == "debug" || text == "release")
            && after(1).is_some_and(|n| n == "deps" || n == "build")
        {
            return true;
        }
        dir.push(name);
        if exists(&dir.join("CACHEDIR.TAG")) {
            return true;
        }
    }
    false
}

/// Whether `path` is a copy that goes away: on a disk image, an App Translocation copy, in a
/// temporary directory or under a cache directory ([`aterm_toolchain::lasting::transient`], the
/// one rule atpkg's shims and the window's subscriber mend name a program by).
pub fn transient(path: &Path) -> bool {
    aterm_toolchain::lasting::transient(path, &aterm_toolchain::lasting::temp_roots())
}

/// Executable a job should run for `exe` ([`in_build_dir`], [`transient`]).
pub fn service_program(exe: &Path, exists: &dyn Fn(&Path) -> bool) -> crate::Result<PathBuf> {
    if !exe.is_absolute() {
        return Err(Error::new(format!(
            "{}: not an absolute path; pass --program <absolute path to aterm>",
            exe.display()
        )));
    }
    if in_build_dir(exe, exists) {
        return Err(Error::new(format!(
            "{} is a build in a cargo target directory, which aterm frees when nobody uses it, \
             so a job running it would stop starting; install aterm, or pass --program <the \
             aterm a lasting job should run>",
            exe.display()
        )));
    }
    if transient(exe) {
        return Err(Error::new(format!(
            "{} is on a disk image, an App Translocation copy, in a temporary directory or under \
             a cache directory, which goes away, so a job running it would stop starting; run \
             aterm from where it is installed (move aterm.app to /Applications), or pass \
             --program <the aterm a lasting job should run>",
            exe.display()
        )));
    }
    let mut bundle = PathBuf::new();
    let mut found = None;
    let names: Vec<&std::ffi::OsStr> = exe.iter().collect();
    for (index, name) in names.iter().enumerate() {
        bundle.push(name);
        let is_app = Path::new(name).extension().is_some_and(|e| e == "app");
        let has_contents = names.get(index + 1).is_some_and(|n| *n == "Contents");
        if is_app && has_contents && index + 2 < names.len() {
            found = Some(bundle.clone());
            break;
        }
    }
    let Some(bundle) = found else {
        return Ok(exe.to_path_buf());
    };
    let macos = bundle.join("Contents").join("MacOS");
    if exe.parent() == Some(macos.as_path()) {
        return Ok(exe.to_path_buf());
    }
    let own_name = exe.file_name().map(|name| macos.join(name));
    for candidate in own_name
        .into_iter()
        .chain(std::iter::once(macos.join("aterm")))
    {
        if exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::new(format!(
        "{} lies inside {} but that bundle has no executable at {}; pass --program <the aterm a \
         lasting job should run>",
        exe.display(),
        bundle.display(),
        macos.join("aterm").display()
    )))
}

/// Executable a job installed now should run ([`service_program`]).
pub fn program_for_service() -> crate::Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::new(format!("cannot name this executable: {e}; pass --program")))?;
    let exe = exe
        .canonicalize()
        .map_err(|e| Error::new(format!("{}: {e}; pass --program", exe.display())))?;
    service_program(&exe, &|path| std::fs::symlink_metadata(path).is_ok())
}

/// How long `program --version` may take to answer ([`check_program_starts`]).
const STARTS_WITHIN: Duration = Duration::from_secs(30);

/// Check that `program` starts as aterm before a job is pointed at it: one a person names
/// (`agent on --program`, `server on --program`) or one a job keeps ([`kept_program_that_starts`]):
/// `program --version` exits 0 and names a release on its first line (`aterm <version>`). A job
/// whose program never starts keeps this Mac out of sync, or its vault unserved, with nothing
/// said.
/// # Errors
/// It could not be run, did not answer within [`STARTS_WITHIN`], exited otherwise than 0, or
/// named no aterm release. On macOS one killed at launch is named so: a loose copy of an app's
/// executable with the ad hoc signing that lets it run outside its bundle, an app's own
/// executable as a damaged install to put back.
pub fn check_program_starts(program: &Path) -> crate::Result<()> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.arg("--version");
    let ran = aterm_uds::deadline::output_within(&mut command, STARTS_WITHIN)
        .map_err(|error| error.to_string());
    program_started(program, ran)
}

/// The release reported by the executable a job would actually run. Build metadata does
/// not order releases. A running helper's version is not evidence about this destination.
/// # Errors
/// The executable fails to start, times out, or reports no three-part aterm release.
pub fn program_release(program: &Path) -> crate::Result<ProgramRelease> {
    let mut command = aterm_uds::spawnfd::command(program);
    command.arg("--version");
    let out = aterm_uds::deadline::output_within(&mut command, STARTS_WITHIN)
        .map_err(|error| Error::new(error.to_string()))?;
    program_started(program, Ok(out.clone()))?;
    release_answer(&String::from_utf8_lossy(&out.stdout)).ok_or_else(|| {
        Error::new(format!(
            "{} --version named no three-part aterm release",
            program.display()
        ))
    })
}

/// A reported major, minor and patch release, without build metadata.
pub type ProgramRelease = (u64, u64, u64);

fn release_answer(stdout: &str) -> Option<ProgramRelease> {
    let release = stdout.lines().next()?.strip_prefix("aterm ")?.trim();
    let mut parts = release
        .split(['+', '-'])
        .next()?
        .split('.')
        .map(str::parse::<u64>);
    let core = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(core)
}

pub(crate) fn require_program_release(
    program: &Path,
    minimum: ProgramRelease,
    release_of: &dyn Fn(&Path) -> Result<ProgramRelease, String>,
) -> crate::Result<()> {
    let actual = release_of(program).map_err(Error::new)?;
    if actual < minimum {
        return Err(Error::new(format!(
            "refusing to move a service onto {}: its release {}.{}.{} is older than required {}.{}.{}",
            program.display(),
            actual.0,
            actual.1,
            actual.2,
            minimum.0,
            minimum.1,
            minimum.2
        )));
    }
    Ok(())
}

/// [`check_program_starts`]'s verdict on what `program --version` gave.
fn program_started(program: &Path, ran: Result<std::process::Output, String>) -> crate::Result<()> {
    let refused = |why: String| {
        Error::new(format!(
            "{} did not start as aterm ({why}), so a job running it would never start",
            program.display()
        ))
    };
    let out = ran.map_err(refused)?;
    if !out.status.success() {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::process::ExitStatusExt;
            if out.status.signal() == Some(9) {
                return Err(refused(match app_of_executable(program) {
                    Some(app) => format!(
                        "macOS killed it at launch: {} no longer matches its signature, a \
                         damaged or modified install; install aterm again rather than signing it",
                        app.display()
                    ),
                    None => format!(
                        "macOS killed it at launch, as it does a copy of an app's executable \
                         outside its bundle, whose signature names the bundle; if it is such a \
                         copy, sign it ad hoc with codesign --force --sign - {}",
                        program.display()
                    ),
                }));
            }
        }
        return Err(refused(format!("--version: {}", out.status)));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let named = stdout
        .lines()
        .next()
        .and_then(|first| first.strip_prefix("aterm "))
        .and_then(|release| release.chars().next())
        .is_some_and(|first| first.is_ascii_digit());
    if !named {
        return Err(refused("its --version named no aterm release".to_string()));
    }
    Ok(())
}

/// The app whose own executable `program` is (`<X>.app/Contents/MacOS/<name>`), by its path.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn app_of_executable(program: &Path) -> Option<&Path> {
    let macos = program.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then_some(app)
}

/// [`kept_program`], once it starts as aterm ([`check_program_starts`]): a job is put back on a
/// program it keeps (a held snapshot) only while that program runs, and a repair never moves it
/// off it onto another aterm (only [`move_held`] does, for a hold that can no longer do what it
/// was for), so one that cannot start is refused, naming it.
/// # Errors
/// The program the job keeps does not start as aterm.
pub fn kept_program_that_starts(home: &Path, job: Job) -> crate::Result<Option<PathBuf>> {
    kept_starting(kept_program(home, job), job, &check_program_starts)
}

/// [`kept_program_that_starts`] of `kept`, `starts` checking it.
fn kept_starting(
    kept: Option<PathBuf>,
    job: Job,
    starts: &dyn Fn(&Path) -> crate::Result<()>,
) -> crate::Result<Option<PathBuf>> {
    let Some(kept) = kept else {
        return Ok(None);
    };
    starts(&kept).map_err(|error| {
        let on = match job {
            Job::Subscriber => "agent on",
            Job::Server => "server on",
        };
        Error::new(format!(
            "{error}; the {} job runs it: aterm secrets {on} puts it on this aterm, or {on} \
             --program <an aterm that runs>",
            job.label()
        ))
    })?;
    Ok(Some(kept))
}

/// The file that defines `job` under `home`: its launchd plist on macOS ([`plist_path`]), its
/// systemd user unit elsewhere ([`unit_path`]).
pub fn unit_file(home: &Path, job: Job) -> PathBuf {
    if cfg!(target_os = "macos") {
        plist_path(home, job)
    } else {
        unit_path(home, job)
    }
}

/// The program `job`'s unit file starts, when that is an executable file in a lasting place
/// (one [`service_program`] names as itself): what a repair keeps ([`repair_subscriber`]), so a
/// repair never moves a job a person pointed at another aterm (`agent on --program`, a held
/// snapshot) onto the aterm that repairs it; only an explicit [`install`] does, or [`move_held`]
/// for a held approver that can no longer read the vault's admin locker and that no person
/// pinned ([`crate::client::mend_held`]). `None` where the unit names no such program (none, one
/// gone, a build, a copy that goes away).
pub fn kept_program(home: &Path, job: Job) -> Option<PathBuf> {
    kept(
        unit_program(&unit_file(home, job)),
        &|path| std::fs::symlink_metadata(path).is_ok(),
        &|path| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        },
    )
}

/// [`kept_program`] of the program a unit file names, `held`.
fn kept(
    held: Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
    runnable: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    held.filter(|held| {
        runnable(held) && service_program(held, exists).is_ok_and(|named| named == *held)
    })
}

/// Refuse what a unit file cannot name faithfully.
fn check_argv(program: &Path, args: &[String]) -> crate::Result<()> {
    let Some(text) = program.to_str() else {
        return Err(Error::new(format!(
            "{}: not a UTF-8 path",
            program.display()
        )));
    };
    if !program.is_absolute() {
        return Err(Error::new(format!(
            "{text}: the program must be an absolute path"
        )));
    }
    if std::iter::once(text)
        .chain(args.iter().map(String::as_str))
        .any(|word| word.chars().any(char::is_control))
    {
        return Err(Error::new(
            "a job's program and arguments cannot hold control characters",
        ));
    }
    match std::fs::metadata(program) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(Error::new(format!("{text}: not a file"))),
        Err(e) => Err(Error::new(format!("{text}: {e}"))),
    }
}

/// Write `text` to the unit file at `path` (0644, atomically), making its directory.
fn write_unit_file(path: &Path, text: &str) -> crate::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    crate::fsx::write_atomic(path, text.as_bytes(), 0o644)
}

/// A path or a release string is not scheduling capability: an update may have replaced
/// the file while this process still runs the old inode. Kept copies remain capped.
fn subscriber_process_type(program: &Path) -> SubscriberProcessType {
    let current = i32::try_from(std::process::id())
        .ok()
        .and_then(aterm_uds::process::LoadedImage::read);
    subscriber_process_type_for(program, current.as_ref())
}

fn subscriber_process_type_for(
    program: &Path,
    current: Option<&aterm_uds::process::LoadedImage>,
) -> SubscriberProcessType {
    let same = current.is_some_and(|loaded| {
        program
            .canonicalize()
            .ok()
            .zip(loaded.launch_path().canonicalize().ok())
            .is_some_and(|(program, launch)| program == launch)
            && loaded.matches_file(program) == Some(true)
    });
    if same {
        SubscriberProcessType::Interactive
    } else {
        SubscriberProcessType::Background
    }
}

/// Write the job's launchd plist: what [`install`] writes on macOS before it loads the job.
pub fn write_plist(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
) -> crate::Result<PathBuf> {
    check_argv(program, args)?;
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    let plist = plist_path(home, job);
    let log = log_path(home, job);
    write_unit_file(
        &plist,
        &plist_text_in_with_subscriber_process_type(
            job.label(),
            program,
            args,
            &log,
            Domain::Gui,
            None,
            if job == Job::Subscriber {
                subscriber_process_type(program)
            } else {
                SubscriberProcessType::Background
            },
        ),
    )?;
    Ok(plist)
}

/// Write the job's systemd user unit: what [`install`] does on Linux before any `systemctl` call.
pub fn write_unit(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
) -> crate::Result<PathBuf> {
    check_argv(program, args)?;
    let unit = unit_path(home, job);
    write_unit_file(&unit, &systemd_unit(job.description(), program, args))?;
    Ok(unit)
}

/// One child run under [`CHILD_HANG`], its stdin `/dev/null`.
fn run(program: &Path, argv: &[String]) -> crate::Result<std::process::Output> {
    run_at(program, argv, Instant::now())
}

fn run_at(
    program: &Path,
    argv: &[String],
    started: Instant,
) -> crate::Result<std::process::Output> {
    run_until(program, argv, started + CHILD_HANG)
}

fn run_until(
    program: &Path,
    argv: &[String],
    until: Instant,
) -> crate::Result<std::process::Output> {
    if Instant::now() >= until {
        return Err(Error::new("service observation deadline expired"));
    }
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(argv);
    aterm_uds::deadline::output_until(&mut cmd, until)
        .map_err(|e| Error::new(format!("{} {}: {e}", program.display(), argv.join(" "))))
}

/// One child run that must succeed: a refusal is an error carrying what it said.
fn run_ok(program: &Path, argv: &[String]) -> crate::Result<()> {
    succeeded(program, argv, &run(program, argv)?)
}

/// `out`, what `program argv` answered, as [`run_ok`] judges it.
fn succeeded(program: &Path, argv: &[String], out: &std::process::Output) -> crate::Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let said = [&out.stderr, &out.stdout]
        .iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Err(Error::new(format!(
        "{} {} ({}){}{said}",
        program.display(),
        argv.join(" "),
        out.status,
        if said.is_empty() { "" } else { ": " }
    )))
}

#[cfg(target_os = "macos")]
fn launchctl(call: Launchctl<'_>) -> crate::Result<std::process::Output> {
    run(
        Path::new(LAUNCHCTL),
        &launchctl_argv(aterm_uds::peer::our_uid(), call),
    )
}

#[cfg(target_os = "macos")]
fn launchctl_ok(call: Launchctl<'_>) -> crate::Result<()> {
    run_ok(
        Path::new(LAUNCHCTL),
        &launchctl_argv(aterm_uds::peer::our_uid(), call),
    )
}

/// What one `launchctl` call answers, run or (in a test) made up.
#[cfg(any(target_os = "macos", all(test, unix)))]
type LaunchctlRun<'a> = dyn FnMut(Launchctl<'_>) -> crate::Result<std::process::Output> + 'a;

/// launchd's view of `label` in `domain` through `launchctl`: `None` when it holds no such
/// job, else its pid if running.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn launchd_job_with(
    domain: Domain,
    label: &str,
    launchctl: &mut LaunchctlRun<'_>,
) -> crate::Result<Option<Option<u32>>> {
    let out = launchctl(Launchctl::Print(domain, label))?;
    Ok(out
        .status
        .success()
        .then(|| print_pid(&String::from_utf8_lossy(&out.stdout))))
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn settle_launchd_bootout(
    domain: Domain,
    label: &str,
    launchctl: &mut LaunchctlRun<'_>,
) -> crate::Result<()> {
    settle_launchd_bootout_at(domain, label, launchctl, Instant::now(), || {
        std::thread::sleep(Duration::from_millis(100));
    })
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn settle_launchd_bootout_at(
    domain: Domain,
    label: &str,
    launchctl: &mut LaunchctlRun<'_>,
    started: Instant,
    mut pause: impl FnMut(),
) -> crate::Result<()> {
    while launchd_job_with(domain, label, launchctl)?.is_some() {
        if started.elapsed() >= BOOTOUT_SETTLE {
            return Err(Error::new("service bootout did not settle"));
        }
        pause();
    }
    Ok(())
}

/// `launchctl`'s refusal of `call` as an error carrying what it said.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn launchctl_checked(call: Launchctl<'_>, launchctl: &mut LaunchctlRun<'_>) -> crate::Result<()> {
    let out = launchctl(call)?;
    succeeded(
        Path::new(LAUNCHCTL),
        &launchctl_argv(aterm_uds::peer::our_uid(), call),
        &out,
    )
}

#[cfg(not(target_os = "macos"))]
fn systemctl() -> crate::Result<PathBuf> {
    systemctl_path(&SYSTEMCTL_PATHS, &|path| path.is_file())
}

/// Refuses an unresolved account home or a `home` that is not `account`.
pub fn own_home(home: &Path, account: Option<&Path>) -> crate::Result<()> {
    let Some(account) = account else {
        return Err(Error::new(
            "cannot resolve this login's account home for a per-login service",
        ));
    };
    #[cfg(unix)]
    let same = {
        use std::os::unix::fs::MetadataExt as _;
        match (std::fs::metadata(home), std::fs::metadata(account)) {
            (Ok(a), Ok(b)) => Some((a.dev(), a.ino()) == (b.dev(), b.ino())),
            _ => None,
        }
    };
    #[cfg(not(unix))]
    let same = None;
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if same.unwrap_or_else(|| canonical(home) == canonical(account)) {
        return Ok(());
    }
    Err(Error::new(format!(
        "{} is not this login's home ({}): the service is one per login, so a job for it would \
         replace the one {} runs; set it up from the login's own home",
        home.display(),
        account.display(),
        account.display()
    )))
}

/// [`own_home`] against the account database.
fn check_own_home(home: &Path) -> crate::Result<()> {
    #[cfg(unix)]
    {
        own_home(home, aterm_uds::account::home().as_deref())
    }
    #[cfg(not(unix))]
    {
        own_home(home, None)
    }
}

/// Install and start `job` under `home` ([`own_home`], [`plist_path`], [`unit_path`]).
///
/// The subscriber installed is on again: the person's record that it was turned off
/// ([`subscriber_off_path`]) goes.
///
/// A launchd job that runs as this would set it up is left running; a systemd unit is
/// restarted.
pub fn install(home: &Path, job: Job, program: &Path, args: &[String]) -> crate::Result<String> {
    install_restarting(home, job, program, args, false)
}

/// [`install`], restarting the job even when it runs as this would set it up
/// (`aterm secrets agent on --force`).
pub fn reinstall(home: &Path, job: Job, program: &Path, args: &[String]) -> crate::Result<String> {
    install_restarting(home, job, program, args, true)
}

fn install_restarting(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
    restart: bool,
) -> crate::Result<String> {
    check_own_home(home)?;
    match job {
        Job::Subscriber => {
            install_subscriber_with(home, || install_job(home, job, program, args, restart))
        }
        Job::Server => install_server_with(home, || install_job(home, job, program, args, restart)),
    }
}

fn install_server_with(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<String> {
    let _decision = JobDecision::take(home, Job::Server)?;
    let line = install()?;
    Ok(match clear_server_off(home) {
        Ok(()) => line,
        Err(e) => format!("{line}; the record that it was turned off stays ({e})"),
    })
}

fn install_subscriber_with(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<String> {
    let _decision = JobDecision::take(home, Job::Subscriber)?;
    let line = install()?;
    // The job runs either way: a record left behind says less than the job that runs.
    Ok(match clear_subscriber_off(home) {
        Ok(()) => line,
        Err(e) => format!("{line}; the record that it was turned off stays ({e})"),
    })
}

/// Repair a subscriber setup already found missing, preserving a person's Sync
/// off recorded since that probe. `None` means it was left off, not installed.
/// The check and install are ordered with [`turn_off`] and explicit [`install`].
///
/// The job keeps the program its unit file names when that one lasts ([`kept_program`]);
/// `program` runs only where it does not, so a caller asks [`kept_program`] first and names its
/// own program only on `None`.
pub fn repair_subscriber(
    home: &Path,
    program: &Path,
    args: &[String],
) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    repair_subscriber_with(home, || {
        let program = kept_program_that_starts(home, Job::Subscriber)?
            .unwrap_or_else(|| program.to_path_buf());
        install_job(home, Job::Subscriber, &program, args, false)
    })
}

/// Install the absent subscriber, or resume its exact owned unit after a failed creation.
/// `admitted` rechecks the caller's current vault policy, without network work.
/// An Off record, any pin, or any existing unit/job remains untouched. The
/// decision lock orders these checks and the install with explicit On/Off.
pub(crate) fn repair_absent_subscriber(
    home: &Path,
    program: &Path,
    admitted: &mut dyn FnMut() -> crate::Result<bool>,
) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    repair_absent_subscriber_with(
        home,
        program,
        admitted,
        &mut || state(home, Job::Subscriber),
        &mut |admitted| install_absent_subscriber(home, program, admitted),
    )
}

/// An installation that rechecks the same policy at its local effect boundary.
type SubscriberInstall<'a> =
    dyn FnMut(&mut dyn FnMut() -> crate::Result<bool>) -> crate::Result<Option<String>> + 'a;

fn repair_absent_subscriber_with(
    home: &Path,
    program: &Path,
    admitted: &mut dyn FnMut() -> crate::Result<bool>,
    state: &mut dyn FnMut() -> crate::Result<State>,
    install: &mut SubscriberInstall<'_>,
) -> crate::Result<Option<String>> {
    let absent = || -> crate::Result<bool> {
        Ok(
            crate::fsx::kind(&subscriber_off_path(home))? == crate::fsx::Kind::Missing
                && crate::fsx::kind(&pin_path(home, Job::Subscriber))? == crate::fsx::Kind::Missing
                && recoverable_subscriber_unit(home, program)? != Some(false),
        )
    };
    if !admitted()? || !absent()? {
        return Ok(None);
    }
    let _decision = JobDecision::take(home, Job::Subscriber)?;
    let recoverable = match state()? {
        State::Absent => true,
        State::Installed { pid: None, .. } => {
            recoverable_subscriber_unit(home, program)? == Some(true)
        }
        State::Installed { pid: Some(_), .. } => false,
    };
    if !absent()? || !recoverable || !admitted()? || !absent()? {
        return Ok(None);
    }
    install(admitted)
}

enum OwnedUnit {
    Missing,
    Foreign,
    Bytes(Vec<u8>),
}

/// A complete locally owned unit, without following links or admitting writable input.
fn owned_unit(home: &Path, job: Job) -> crate::Result<OwnedUnit> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let path = unit_file(home, job);
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(if crate::fsx::kind(&path)? == crate::fsx::Kind::Missing {
                OwnedUnit::Missing
            } else {
                OwnedUnit::Foreign
            });
        }
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(OwnedUnit::Foreign),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != aterm_uds::peer::our_uid()
        || metadata.nlink() != 1
        || metadata.mode() & 0o6022 != 0
        || metadata.len() > UNIT_FILE_MOST as u64
    {
        return Ok(OwnedUnit::Foreign);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(0)
            .min(UNIT_FILE_MOST)
            .saturating_add(1),
    );
    file.take(UNIT_FILE_MOST as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(if bytes.len() <= UNIT_FILE_MOST {
        OwnedUnit::Bytes(bytes)
    } else {
        OwnedUnit::Foreign
    })
}

/// None is absent; true is exactly an owned definition this writer can create.
fn recoverable_subscriber_unit(home: &Path, program: &Path) -> crate::Result<Option<bool>> {
    let bytes = match owned_unit(home, Job::Subscriber)? {
        OwnedUnit::Missing => return Ok(None),
        OwnedUnit::Foreign => return Ok(Some(false)),
        OwnedUnit::Bytes(bytes) => bytes,
    };
    let args = ["secrets".to_owned(), "watch".to_owned()];
    #[cfg(target_os = "macos")]
    let matches = std::str::from_utf8(&bytes).ok().is_some_and(|definition| {
        owned_plist_shape(home, Job::Subscriber, program, &args, definition).is_some()
    });
    #[cfg(not(target_os = "macos"))]
    let matches = bytes == systemd_unit(Job::Subscriber.description(), program, &args).as_bytes();
    Ok(Some(matches))
}

/// Read launchd's cached source path, top-level program and complete arguments block, as its
/// existing `print` format lists them. A disk plist can have changed since this was loaded.
/// Unknown, duplicate or nested-only fields never establish a matching cached definition.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn printed_subscriber_matches(text: &str, program: &Path, unit: &Path) -> bool {
    let (Some(program), Some(unit)) = (program.to_str(), unit.to_str()) else {
        return false;
    };
    let lines: Vec<_> = text.lines().collect();
    let mut paths = lines
        .iter()
        .filter_map(|line| line.strip_prefix("\tpath = "));
    if paths.next() != Some(unit) || paths.next().is_some() {
        return false;
    }
    let mut programs = lines
        .iter()
        .filter_map(|line| line.strip_prefix("\tprogram = "));
    if programs.next() != Some(program) || programs.next().is_some() {
        return false;
    }
    let mut blocks = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == "\targuments = {");
    let Some((at, _)) = blocks.next() else {
        return false;
    };
    if blocks.next().is_some() {
        return false;
    }
    let first = format!("\t\t{program}");
    lines
        .get(at + 1..at + 5)
        .is_some_and(|args| args == [first.as_str(), "\t\tsecrets", "\t\twatch", "\t}"])
}

#[cfg(any(target_os = "macos", all(test, unix)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CachedSubscriber {
    Absent,
    Owned(Option<u32>),
    Foreign,
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn loaded_subscriber_with(
    domain: Domain,
    program: &Path,
    unit: &Path,
    manager: &mut LaunchctlRun<'_>,
) -> crate::Result<CachedSubscriber> {
    let out = manager(Launchctl::Print(domain, Job::Subscriber.label()))?;
    if !out.status.success() {
        return Ok(if matches!(out.status.code(), Some(112 | 113)) {
            CachedSubscriber::Absent
        } else {
            CachedSubscriber::Foreign
        });
    }
    let Ok(text) = std::str::from_utf8(&out.stdout) else {
        return Ok(CachedSubscriber::Foreign);
    };
    if !printed_subscriber_matches(text, program, unit) {
        return Ok(CachedSubscriber::Foreign);
    }
    Ok(CachedSubscriber::Owned(print_pid(text)))
}

/// Refresh only an exact owned legacy subscriber on the current loaded image. This also
/// covers a healthy same-path job: held-program migration deliberately excludes that case.
/// The caller keeps its current setup choice through this operation.
pub(crate) fn refresh_subscriber_scheduling(
    home: &Path,
    program: &Path,
    admitted: &mut dyn FnMut() -> crate::Result<bool>,
) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    #[cfg(target_os = "macos")]
    {
        refresh_subscriber_scheduling_with(
            home,
            program,
            admitted,
            &mut launchctl,
            aterm_types::update_boundary::Boundary::process(),
            &mut |program| subscriber_process_type(program) == SubscriberProcessType::Interactive,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (program, admitted);
        Ok(None)
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn refresh_subscriber_scheduling_with(
    home: &Path,
    program: &Path,
    admitted: &mut dyn FnMut() -> crate::Result<bool>,
    manager: &mut LaunchctlRun<'_>,
    boundary: &aterm_types::update_boundary::Boundary,
    capable: &mut dyn FnMut(&Path) -> bool,
) -> crate::Result<Option<String>> {
    let job = Job::Subscriber;
    let _decision = JobDecision::take(home, job)?;
    let _boundary = boundary
        .hold(
            aterm_types::update_boundary::GuardedItem::LaunchdBootstrap,
            || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap_or_else(|never| match never {});
    if !admitted()?
        || crate::fsx::kind(&subscriber_off_path(home))? != crate::fsx::Kind::Missing
        || crate::fsx::kind(&pin_path(home, job))? != crate::fsx::Kind::Missing
    {
        return Ok(None);
    }
    let OwnedUnit::Bytes(bytes) = owned_unit(home, job)? else {
        return Ok(None);
    };
    let Ok(definition) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    let args = ["secrets".to_owned(), "watch".to_owned()];
    let Some(shape) = owned_plist_shape(home, job, program, &args, definition) else {
        return Ok(None);
    };
    if shape.policy.subscriber != SubscriberProcessType::Background || !capable(program) {
        return Ok(None);
    }
    // Preserve the chosen executable and all non-scheduling directives. A cached job in the
    // other domain is not this exact owned unit and is never removed through this path.
    let unit = unit_file(home, job);
    if loaded_subscriber_with(shape.domain.other(), program, &unit, manager)?
        != CachedSubscriber::Absent
    {
        return Ok(None);
    }
    let loaded = loaded_subscriber_with(shape.domain, program, &unit, manager)?;
    if loaded == CachedSubscriber::Foreign {
        return Ok(None);
    }
    let bundle = bundle_id_of(program);
    let text = plist_text_with_path(
        job.label(),
        program,
        &args,
        &log_path(home, job),
        shape.domain,
        if shape.attributed {
            bundle.as_deref()
        } else {
            None
        },
        LaunchdPolicy {
            path: shape.policy.path,
            subscriber: SubscriberProcessType::Interactive,
        },
    );
    if !admitted()?
        || !capable(program)
        || !matches!(owned_unit(home, job)?, OwnedUnit::Bytes(current) if current == bytes)
    {
        return Ok(None);
    }
    if matches!(loaded, CachedSubscriber::Owned(_)) {
        if loaded_subscriber_with(shape.domain, program, &unit, manager)? != loaded {
            return Ok(None);
        }
        launchctl_checked(Launchctl::Bootout(shape.domain, job.label()), manager)?;
        settle_launchd_bootout(shape.domain, job.label(), manager)?;
    }
    // A refused bootout must leave the old on-disk policy intact, so the next mend still
    // sees the outstanding migration rather than mistaking disk bytes for loaded policy.
    write_unit_file(&unit_file(home, job), &text)?;
    launchctl_checked(
        Launchctl::Bootstrap(shape.domain, &unit_file(home, job)),
        manager,
    )?;
    Ok(Some(format!(
        "{}: refreshed its subscriber scheduling",
        job.label()
    )))
}

fn install_absent_subscriber(
    home: &Path,
    program: &Path,
    admitted: &mut dyn FnMut() -> crate::Result<bool>,
) -> crate::Result<Option<String>> {
    #[cfg(target_os = "macos")]
    {
        install_absent_launchd(
            home,
            program,
            admitted,
            &mut launchctl,
            aterm_types::update_boundary::Boundary::process(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let args = ["secrets".to_owned(), "watch".to_owned()];
        check_argv(program, &args)?;
        let systemctl = systemctl()?;
        let name = unit_name(Job::Subscriber);
        let loaded = run(
            &systemctl,
            &systemctl_argv(&["show", "-p", "LoadState", "--value", &name]),
        )?;
        // Disk bytes do not establish which definition systemd cached. Do not
        // reload or act on any loaded unit through the missing-job path.
        if String::from_utf8_lossy(&loaded.stdout).trim() != "not-found" {
            return Ok(None);
        }
        let unit = unit_path(home, Job::Subscriber);
        std::fs::create_dir_all(unit.parent().expect("unit parent"))?;
        if !admitted()? {
            return Ok(None);
        }
        if recoverable_subscriber_unit(home, program)?.is_none()
            && !crate::fsx::write_new(
                &unit,
                systemd_unit(Job::Subscriber.description(), program, &args).as_bytes(),
                0o644,
            )?
        {
            return Ok(None);
        }
        if !admitted()? || recoverable_subscriber_unit(home, program)? != Some(true) {
            return Ok(None);
        }
        run_ok(&systemctl, &systemctl_argv(&["daemon-reload"]))?;
        run_ok(&systemctl, &systemctl_argv(&["enable", "--now", &name]))?;
        Ok(Some(format!(
            "{}: installed its missing subscriber",
            unit.display()
        )))
    }
}

/// [`install_absent_subscriber`] on macOS, each `launchctl` call through `launchctl`, its
/// bootstrap holding `boundary`'s guard (this process's, outside a test).
#[cfg(target_os = "macos")]
fn install_absent_launchd(
    home: &Path,
    program: &Path,
    admitted: &mut dyn FnMut() -> crate::Result<bool>,
    launchctl: &mut LaunchctlRun<'_>,
    boundary: &aterm_types::update_boundary::Boundary,
) -> crate::Result<Option<String>> {
    let _boundary = boundary
        .hold(
            aterm_types::update_boundary::GuardedItem::LaunchdBootstrap,
            || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap_or_else(|never| match never {});
    let job = Job::Subscriber;
    let args = ["secrets".to_owned(), "watch".to_owned()];
    check_argv(program, &args)?;
    if launchd_job_with(Domain::Gui, job.label(), launchctl)?.is_some()
        || launchd_job_with(Domain::User, job.label(), launchctl)?.is_some()
    {
        return Ok(None);
    }
    let preferred = if launchctl(Launchctl::PrintDomain(Domain::Gui))?
        .status
        .success()
    {
        Domain::Gui
    } else {
        Domain::User
    };
    let unit = plist_path(home, job);
    std::fs::create_dir_all(unit.parent().expect("plist parent"))?;
    let bundle = bundle_id_of(program);
    let subscriber = subscriber_process_type(program);
    let text_for = |domain| {
        plist_text_in_with_subscriber_process_type(
            job.label(),
            program,
            &args,
            &log_path(home, job),
            domain,
            bundle.as_deref(),
            subscriber,
        )
    };
    let domain = [Domain::Gui, Domain::User]
        .into_iter()
        .find(|domain| {
            read_regular_string(&unit, UNIT_FILE_MOST).as_deref()
                == Some(text_for(*domain).as_str())
        })
        .unwrap_or(preferred);
    let text = text_for(domain);
    if recoverable_subscriber_unit(home, program)?.is_some()
        && read_regular_string(&unit, UNIT_FILE_MOST).as_deref() != Some(text.as_str())
    {
        return Ok(None);
    }
    if !admitted()? {
        return Ok(None);
    }
    if recoverable_subscriber_unit(home, program)?.is_none()
        && !crate::fsx::write_new(&unit, text.as_bytes(), 0o644)?
    {
        return Ok(None);
    }
    if !admitted()? || recoverable_subscriber_unit(home, program)? != Some(true) {
        return Ok(None);
    }
    // No loaded or foreign unit is replaced. All observations and image-policy selection
    // follow the update-boundary wait, so the bootstrap cannot act on an old pre-wait image.
    if !admitted()?
        || recoverable_subscriber_unit(home, program)? != Some(true)
        || launchd_job_with(Domain::Gui, job.label(), launchctl)?.is_some()
        || launchd_job_with(Domain::User, job.label(), launchctl)?.is_some()
    {
        return Ok(None);
    }
    launchctl_checked(Launchctl::Bootstrap(domain, &unit), launchctl)?;
    // RunAtLoad and KeepAlive are in the exact generated plist. Bootstrap starts
    // it; no later kickstart can accidentally act on a replaced loaded job.
    Ok(Some(format!(
        "{}: installed its missing subscriber",
        unit.display()
    )))
}

/// Repair the vault server's job when the program its unit file names is gone (its `aterm.app`
/// moved or deleted, a disk image ejected), which launchd or systemd can then never start again:
/// installed again running `program()`, with the arguments it had, so it serves the same vaults
/// from the same address and store. `None` where nothing is repaired: no server job, one whose
/// program is there (left as it is, running or not, wherever it is), one a person switched off
/// (a plist's `Disabled` key), one in a shape `aterm secrets server on` never writes (no
/// `secrets serve` after its program), one while the system LaunchDaemon `aterm machines apply`
/// installs ([`DAEMON_LABEL`]) is there, installed or running, which serves this Mac's vaults on
/// the port a user job would take ([`daemon_state_for_home`]), or a home that is not this login's
/// own, whose jobs launchd and systemd never read (as [`state`]). The job's decision lock
/// (`JobDecision`) is taken only once its unit file reads as a `secrets serve` job whose program
/// is gone, and the job read again under it, so a Mac with no server job gets no lock file (nor
/// `~/.config/aterm/secrets`) from the mend. `program` is asked only for a repair.
/// # Errors
/// The job's decision lock or the system LaunchDaemon's state cannot be read, or a repair was due
/// and `program` cannot be named or the job did not install.
pub fn repair_server(
    home: &Path,
    program: &dyn Fn() -> crate::Result<PathBuf>,
) -> crate::Result<Option<String>> {
    if check_own_home(home).is_err() {
        return Ok(None);
    }
    repair_server_with(
        home,
        &|path| std::fs::symlink_metadata(path).is_ok(),
        &|| daemon_state_for_home(home),
        |args| install_job(home, Job::Server, &program()?, args, false),
    )
}

/// Start an absent or stopped member server without changing a held program or clearing
/// an Off or pin decision. The unit is read again under the same lock as explicit On/Off.
/// # Errors
/// The policy or existing unit is unreadable, or the retained/chosen program cannot start.
pub(crate) fn ensure_server(
    home: &Path,
    program: &Path,
    args: &[String],
    minimum: ProgramRelease,
) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    ensure_server_with(
        home,
        program,
        args,
        &|| daemon_state_for_home(home),
        &|to| {
            require_program_release(to, minimum, &|path| {
                program_release(path).map_err(|e| e.to_string())
            })
        },
        &mut |to, argv| {
            check_program_starts(to)?;
            install_job(home, Job::Server, to, argv, false)
        },
    )
}

pub(crate) fn ensure_server_with(
    home: &Path,
    program: &Path,
    args: &[String],
    daemon: &dyn Fn() -> crate::Result<State>,
    candidate: &dyn Fn(&Path) -> crate::Result<()>,
    install: &mut dyn FnMut(&Path, &[String]) -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    let _decision = JobDecision::take(home, Job::Server)?;
    if crate::fsx::kind(&server_off_path(home))? != crate::fsx::Kind::Missing
        || matches!(daemon()?, State::Installed { .. })
    {
        return Ok(None);
    }
    let unit = unit_file(home, Job::Server);
    let pin = if crate::fsx::kind(&pin_path(home, Job::Server))? == crate::fsx::Kind::Missing {
        None
    } else {
        Some(
            pinned_to(home, Job::Server)
                .ok_or_else(|| Error::new("cannot safely read the server pin"))?,
        )
    };
    match crate::fsx::kind(&unit)? {
        crate::fsx::Kind::Missing => {
            if pin.is_some() {
                return Ok(None);
            }
            candidate(program)?;
            install(program, args).map(Some)
        }
        _ => {
            let text = read_regular_string(&unit, UNIT_FILE_MOST)
                .ok_or_else(|| Error::new("cannot safely read the existing server job"))?;
            if cfg!(target_os = "macos") && text.contains("<key>Disabled</key>") {
                return Ok(None);
            }
            let argv = if cfg!(target_os = "macos") {
                plist_argv(&text)
            } else {
                exec_argv(&text)
            }
            .ok_or_else(|| Error::new("cannot safely read the existing server job"))?;
            let (kept, argv) = argv
                .split_first()
                .ok_or_else(|| Error::new("existing server job has no program"))?;
            if argv
                .get(..2)
                .is_none_or(|words| words != ["secrets", "serve"])
            {
                return Err(Error::new("existing server job is not a secrets server"));
            }
            let kept = Path::new(kept);
            let canonical =
                |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
            if pin.is_some_and(|pin| canonical(&pin) != canonical(kept)) {
                return Ok(None);
            }
            install(kept, argv).map(Some)
        }
    }
}

/// [`repair_server`], `exists` saying whether anything is at a path, `daemon` reading the system
/// LaunchDaemon's state and `install` installing the job with these arguments.
pub(crate) fn repair_server_with(
    home: &Path,
    exists: &dyn Fn(&Path) -> bool,
    daemon: &dyn Fn() -> crate::Result<State>,
    install: impl FnOnce(&[String]) -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    if server_repair_due(home, exists).is_none() {
        return Ok(None);
    }
    let _decision = JobDecision::take(home, Job::Server)?;
    let Some(args) = server_repair_due(home, exists) else {
        return Ok(None);
    };
    if matches!(daemon()?, State::Installed { .. }) {
        return Ok(None);
    }
    install(&args).map(Some)
}

/// The arguments of `home`'s server job when [`repair_server`] would install it again: its unit
/// file reads as a `secrets serve` job, not switched off, whose program `exists` finds gone.
fn server_repair_due(home: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<Vec<String>> {
    let argv = unit_argv(home, Job::Server)?;
    let (held, args) = argv.split_first()?;
    let serves = args
        .get(..2)
        .is_some_and(|words| words == ["secrets", "serve"]);
    (serves && !exists(Path::new(held))).then(|| args.to_vec())
}

/// The program and arguments `home`'s `job` unit file starts, whoever's home it is: `None` for no
/// unit file, one a person switched off (a plist's `Disabled` key), or a form [`plist_text`] and
/// [`systemd_unit`] never write.
pub(crate) fn unit_argv(home: &Path, job: Job) -> Option<Vec<String>> {
    let text = read_regular_string(&unit_file(home, job), UNIT_FILE_MOST)?;
    if cfg!(target_os = "macos") {
        plist_argv(&text).filter(|_| !text.contains("<key>Disabled</key>"))
    } else {
        exec_argv(&text)
    }
}

/// Stop and remove `home`'s server job while its unit runs arguments `retired` names (after its
/// program) and no pin record ([`pin_path`]) says a person chose it: the member server an earlier
/// automatic setup installed where nothing finds or feeds it (`sync.steward.server.standby` off
/// macOS). No Off record is written, as no person chose off. `None` where nothing was removed,
/// and for a home that is not this login's own, whose jobs its service manager never reads (as
/// [`state`]).
/// # Errors
/// The job's decision lock or pin record cannot be read, or the job did not stop or go.
pub(crate) fn retire_server(
    home: &Path,
    retired: &dyn Fn(&[String]) -> bool,
) -> crate::Result<Option<String>> {
    if check_own_home(home).is_err() {
        return Ok(None);
    }
    retire_server_with(home, retired, || uninstall_job(home, Job::Server))
}

/// Whether [`retire_server`] would remove `home`'s server job now.
/// # Errors
/// The pin record cannot be read.
pub(crate) fn server_retire_due(
    home: &Path,
    retired: &dyn Fn(&[String]) -> bool,
) -> crate::Result<bool> {
    if check_own_home(home).is_err() {
        return Ok(false);
    }
    retire_due(home, retired)
}

/// [`retire_server`] under `home`, whoever's it is, `remove` stopping and removing the job. The
/// job's decision lock (`JobDecision`) is taken only once the unit reads as one to retire, and
/// the unit is read again under it, so an `on`, an `off` or a repair meanwhile stands and a home
/// with no such job gets no lock file.
pub(crate) fn retire_server_with(
    home: &Path,
    retired: &dyn Fn(&[String]) -> bool,
    remove: impl FnOnce() -> crate::Result<bool>,
) -> crate::Result<Option<String>> {
    if !retire_due(home, retired)? {
        return Ok(None);
    }
    let _decision = JobDecision::take(home, Job::Server)?;
    if !retire_due(home, retired)? {
        return Ok(None);
    }
    Ok(remove()?.then(|| format!("removed {}", unit_file(home, Job::Server).display())))
}

fn retire_due(home: &Path, retired: &dyn Fn(&[String]) -> bool) -> crate::Result<bool> {
    let Some(argv) = unit_argv(home, Job::Server) else {
        return Ok(false);
    };
    let Some((_, args)) = argv.split_first() else {
        return Ok(false);
    };
    Ok(retired(args)
        && crate::fsx::kind(&pin_path(home, Job::Server))? == crate::fsx::Kind::Missing)
}

/// A vault job held on another aterm than the one asking ([`held`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// The job.
    pub job: Job,
    /// The program its unit file names, a lasting one ([`kept_program`]), canonical.
    pub program: PathBuf,
    /// Its arguments after the program, as `agent on` or `server on` wrote them.
    pub args: Vec<String>,
    /// The program the person's pin record names ([`pin_path`]), canonical: one naming another
    /// program than `program` (the job was moved since) pins nothing ([`Held::pinned`]), but a
    /// move reads it again before it moves the job ([`move_held`]).
    pub pin: Option<PathBuf>,
}

impl Held {
    /// Whether a person pinned the job to the program it runs, on purpose ([`pin_path`]).
    #[must_use]
    pub fn pinned(&self) -> bool {
        self.pin.as_ref() == Some(&self.program)
    }
}

/// `job` under `home` when it runs a lasting program other than `program` (a held snapshot,
/// [`kept_program`]; `program` is the aterm asking, where it is one a job may run) in the shape
/// `agent on` or `server on` writes (`secrets watch`; `secrets serve …`) and is not switched off
/// (a plist's `Disabled` key): what the window's mend may move onto `program`
/// ([`crate::client::mend_held`]). `None` otherwise, and for a home that is not this login's
/// own, whose jobs launchd and systemd never read (as [`state`]).
pub fn held(home: &Path, job: Job, program: Option<&Path>) -> Option<Held> {
    check_own_home(home).ok()?;
    held_at(home, job, program)
}

/// [`held`] under `home`, whoever's it is.
pub(crate) fn held_at(home: &Path, job: Job, program: Option<&Path>) -> Option<Held> {
    let text = read_regular_string(&unit_file(home, job), UNIT_FILE_MOST)?;
    let canonical = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    held_in(
        job,
        &text,
        kept_program(home, job).map(|kept| canonical(&kept)),
        program.map(canonical).as_deref(),
        pinned_to(home, job).map(|pin| canonical(&pin)),
    )
}

/// [`held`] of the unit file `text`, whose program [`kept_program`] reads as `kept`, the pin
/// record naming `pin` ([`pin_path`]).
fn held_in(
    job: Job,
    text: &str,
    kept: Option<PathBuf>,
    program: Option<&Path>,
    pin: Option<PathBuf>,
) -> Option<Held> {
    let argv = if cfg!(target_os = "macos") {
        plist_argv(text).filter(|_| !text.contains("<key>Disabled</key>"))
    } else {
        exec_argv(text)
    };
    let (_, args) = argv.as_deref().and_then(<[String]>::split_first)?;
    let shaped = match job {
        Job::Subscriber => args == ["secrets", "watch"],
        Job::Server => args
            .get(..2)
            .is_some_and(|words| words == ["secrets", "serve"]),
    };
    let kept = kept.filter(|kept| shaped && program != Some(kept.as_path()))?;
    Some(Held {
        job,
        program: kept,
        args: args.to_vec(),
        pin,
    })
}

/// The record that a person pinned `job` to a program on purpose, naming it:
/// `~/.config/aterm/secrets/<label>.pinned`. `agent on --program <path>` and `server on --program
/// <path>` write it, before their install ([`install_pinned`]); `on` with no `--program` (which
/// puts the job on the aterm that runs it) and `off` remove it. The window's mend never moves a
/// job whose record names the program it runs ([`crate::client::mend_held`]); a record naming
/// another program (the job was moved since) is none.
pub fn pin_path(home: &Path, job: Job) -> PathBuf {
    secrets_dir(home).join(format!("{}.pinned", job.label()))
}

/// The program [`pin_path`]'s record names: `None` where there is none, or it cannot be read.
fn pinned_to(home: &Path, job: Job) -> Option<PathBuf> {
    let bytes = crate::fsx::read_private(&pin_path(home, job), 4096).ok()??;
    let text = std::str::from_utf8(bytes.as_slice()).ok()?;
    let program = Path::new(text.trim_end_matches('\n'));
    program.is_absolute().then(|| program.to_path_buf())
}

/// Record that a person pinned `job` to `program` ([`pin_path`]).
/// # Errors
/// The record could not be written.
pub fn record_pin(home: &Path, job: Job, program: &Path) -> crate::Result<()> {
    let Some(text) = program.to_str() else {
        return Err(Error::new(format!(
            "{}: not a UTF-8 path",
            program.display()
        )));
    };
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    crate::fsx::write_atomic(&pin_path(home, job), format!("{text}\n").as_bytes(), 0o600)
}

/// Remove [`pin_path`]'s record: the job is no longer pinned.
/// # Errors
/// The record is there and could not be removed.
pub fn clear_pin(home: &Path, job: Job) -> crate::Result<()> {
    let path = pin_path(home, job);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// Move the job `held` names off the program it holds onto `program`, the aterm that runs this,
/// with the arguments it had, once [`crate::client::mend_held`] judged the hold can no longer do
/// what it was for. Under the job's decision lock (`JobDecision`, which `on` and `off` take),
/// the job is read again first and moved only while it still is `held` (the same program,
/// arguments and pin record) and, for the subscriber, the person has not turned it off; what a
/// person did while the mend judged it (an `off`, an `on`, an `on --program`, whose pin record
/// comes before its install) stands (`None`). The server moves nothing (`None`) while the system
/// LaunchDaemon `aterm machines apply` installs ([`DAEMON_LABEL`]) is there, installed or
/// running ([`daemon_state_for_home`]): it serves this Mac's vaults, and a user job started
/// beside it would take its port, as `server on` refuses to. launchd boots the job out (its
/// running process ends) and starts it on `program`; where that fails (a bootstrap refused after
/// the bootout, say), the job is installed again on the program it held, so it runs as it did,
/// and the error names both.
/// # Errors
/// The home is not this login's own, the job's decision lock, the subscriber's off record or
/// the system LaunchDaemon's state (for the server) cannot be read, or the job did not start on
/// `program`.
pub fn move_held(home: &Path, held: &Held, program: &Path) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    move_held_with(
        home,
        held,
        program,
        &|| daemon_state_for_home(home),
        &mut |program| install_job(home, held.job, program, &held.args, false),
    )
}

/// [`move_held`] under `home`, `daemon` reading the system LaunchDaemon's state and `install`
/// installing the job on a program.
pub(crate) fn move_held_with(
    home: &Path,
    held: &Held,
    program: &Path,
    daemon: &dyn Fn() -> crate::Result<State>,
    install: &mut dyn FnMut(&Path) -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    let _decision = JobDecision::take(home, held.job)?;
    let off_path = match held.job {
        Job::Subscriber => subscriber_off_path(home),
        Job::Server => server_off_path(home),
    };
    if crate::fsx::kind(&off_path)? != crate::fsx::Kind::Missing {
        return Ok(None);
    }
    if held_at(home, held.job, Some(program)).as_ref() != Some(held) {
        return Ok(None);
    }
    if held.job == Job::Server && matches!(daemon()?, State::Installed { .. }) {
        return Ok(None);
    }
    moved_or_put_back(held, program, install).map(Some)
}

/// [`move_held`], counting the job moved only once `serves` finds it on `program` doing what it
/// did (the server serving its store, `DESIGN-fluid-swarm` §4.3 step 1); where `serves` fails,
/// the job is put back on the program it held, as when it does not start there.
/// # Errors
/// As [`move_held`]'s, or `serves` failed.
pub fn move_held_serving(
    home: &Path,
    held: &Held,
    program: &Path,
    serves: &dyn Fn() -> crate::Result<()>,
) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    move_held_serving_with(
        home,
        held,
        program,
        &|| daemon_state_for_home(home),
        &mut |to| install_job(home, held.job, to, &held.args, false),
        serves,
    )
}

/// [`move_held_serving`] under `home`, `daemon` reading the system LaunchDaemon's state and
/// `install` installing the job on a program.
pub(crate) fn move_held_serving_with(
    home: &Path,
    held: &Held,
    program: &Path,
    daemon: &dyn Fn() -> crate::Result<State>,
    install: &mut dyn FnMut(&Path) -> crate::Result<String>,
    serves: &dyn Fn() -> crate::Result<()>,
) -> crate::Result<Option<String>> {
    move_held_with(home, held, program, daemon, &mut |to| {
        let line = install(to)?;
        if to == program {
            serves()?;
        }
        Ok(line)
    })
}

/// `agent on` and `server on`'s `install` of `job`, with the person's pin ([`pin_path`]): `pin`
/// names the program they pinned it to (`--program`), recorded before the install, so a mend that
/// reads the job meanwhile finds the pin and moves nothing ([`move_held`]); with `None` (no
/// `--program`: the job goes on the aterm that runs it) the record goes once the install ran. An
/// install that fails puts the record back as it was. The install's line, and why the record
/// could not be changed where it could not.
/// # Errors
/// The install failed.
pub fn install_pinned(
    home: &Path,
    job: Job,
    pin: Option<&Path>,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<(String, Option<Error>)> {
    let Some(pin) = pin else {
        let line = install()?;
        return Ok((line, clear_pin(home, job).err()));
    };
    let before = pinned_to(home, job);
    let unrecorded = record_pin(home, job, pin).err();
    match install() {
        Ok(line) => Ok((line, unrecorded)),
        Err(error) => {
            if unrecorded.is_none() {
                // Best effort: the install's error is what the person needs to read.
                let _ = match before {
                    Some(before) => record_pin(home, job, &before),
                    None => clear_pin(home, job),
                };
            }
            Err(error)
        }
    }
}

/// [`move_held`]'s install of `held` onto `program`, `install` installing the job on a
/// program, and on the program it held again where that fails.
fn moved_or_put_back(
    held: &Held,
    program: &Path,
    install: &mut dyn FnMut(&Path) -> crate::Result<String>,
) -> crate::Result<String> {
    let error = match install(program) {
        Ok(line) => return Ok(line),
        Err(error) => error,
    };
    let on = match held.job {
        Job::Subscriber => "agent on",
        Job::Server => "server on",
    };
    Err(match install(&held.program) {
        Ok(_) => Error::new(format!(
            "{error}; the {} job was put back on {}, as it ran",
            held.job.label(),
            held.program.display()
        )),
        Err(back) => Error::new(format!(
            "{error}; putting the {} job back on {} failed too ({back}), so it is down until \
             aterm secrets {on} installs it again",
            held.job.label(),
            held.program.display()
        )),
    })
}

fn repair_subscriber_with(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    let _decision = JobDecision::take(home, Job::Subscriber)?;
    if crate::fsx::kind(&subscriber_off_path(home))? != crate::fsx::Kind::Missing {
        return Ok(None);
    }
    // This is a repair, not an explicit On: it never clears an off decision.
    // A later Off waits for this local install, then records and removes it.
    install().map(Some)
}

/// [`install`]'s job, without the person's record.
fn install_job(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
    restart: bool,
) -> crate::Result<String> {
    #[cfg(target_os = "macos")]
    {
        install_launchd(
            home,
            job,
            (program, args),
            restart,
            &mut launchctl,
            aterm_types::update_boundary::Boundary::process(),
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        // systemd restarts the unit on every install, a changed one or not.
        let _ = restart;
        let systemctl = systemctl()?;
        let unit = write_unit(home, job, program, args)?;
        let name = unit_name(job);
        run_ok(&systemctl, &systemctl_argv(&["daemon-reload"]))?;
        run_ok(&systemctl, &systemctl_argv(&["enable", "--now", &name]))?;
        run_ok(&systemctl, &systemctl_argv(&["restart", &name]))?;
        let mut line = format!(
            "{name}: systemd --user unit {} runs {} {} (log: journalctl --user -u {name})",
            unit.display(),
            program.display(),
            args.join(" ")
        );
        let user = aterm_uds::account::user_name(aterm_uds::peer::our_uid());
        let lingering = user
            .as_deref()
            .is_some_and(|user| Path::new(LINGER_DIR).join(user).exists());
        if let Some(user) = user
            && needs_linger(&|key| std::env::var(key).ok(), lingering)
        {
            let enabled = LOGINCTL_PATHS
                .iter()
                .map(Path::new)
                .find(|path| path.is_file())
                .ok_or_else(|| {
                    Error::new(format!("no loginctl at {}", LOGINCTL_PATHS.join(" or ")))
                })
                .and_then(|loginctl| run_ok(loginctl, &["enable-linger".to_owned(), user.clone()]));
            line.push('\n');
            line.push_str(&linger_line(&user, enabled));
        }
        Ok(line)
    }
}

/// [`install_job`] on macOS, each `launchctl` call through `launchctl`. The job goes in
/// [`Domain::Gui`] while this user is logged in at the Mac's screen, else in [`Domain::User`]
/// (an SSH session's Mac, where `gui/<uid>` is not there to bootstrap into), its plist
/// attributed to the app `program` is the executable of. A job launchd runs there from the
/// plist this would write is left as it is unless `restart` (one `print` in the GUI domain);
/// any other is booted out of whichever domain holds it (so a job set up over SSH moves to the
/// login at the screen, never runs twice), bootstrapped (`RunAtLoad` starts it) and
/// kickstarted, killing a running instance only when `restart`, so a reinstall never cuts a
/// long-poll twice. From its first read of launchd to the kickstart it holds `boundary`'s guard
/// (this process's, outside a test).
#[cfg(any(target_os = "macos", all(test, unix)))]
fn install_launchd(
    home: &Path,
    job: Job,
    (program, args): (&Path, &[String]),
    restart: bool,
    launchctl: &mut LaunchctlRun<'_>,
    boundary: &aterm_types::update_boundary::Boundary,
) -> crate::Result<String> {
    check_argv(program, args)?;
    // From the reads of what launchd has loaded to the kickstart, the job is not repeated
    // safely: it holds the update boundary (init-on-by-default design §6.7). The guard comes
    // first, so a wait there for an update's handoff, up to minutes, comes before anything is
    // read or written, and nothing acts on what was read before it.
    let _boundary = boundary
        .hold(
            aterm_types::update_boundary::GuardedItem::LaunchdBootstrap,
            || Ok::<(), std::convert::Infallible>(()),
        )
        .unwrap_or_else(|never| match never {});
    let label = job.label();
    let plist = plist_path(home, job);
    let log = log_path(home, job);
    let bundle_id = bundle_id_of(program);
    let subscriber = if job == Job::Subscriber {
        subscriber_process_type(program)
    } else {
        SubscriberProcessType::Background
    };
    let text = |domain| {
        plist_text_in_with_subscriber_process_type(
            label,
            program,
            args,
            &log,
            domain,
            bundle_id.as_deref(),
            subscriber,
        )
    };
    let as_held = |domain| {
        let expected = text(domain);
        read_regular_bounded(&plist, expected.len()).is_some_and(|held| held == expected.as_bytes())
    };
    let line = format!(
        "{label}: launchd user agent {} runs {} {} (log {})",
        plist.display(),
        program.display(),
        args.join(" "),
        log.display()
    );
    let in_gui = launchd_job_with(Domain::Gui, label, launchctl)?;
    if !restart && matches!(in_gui, Some(Some(_))) && as_held(Domain::Gui) {
        return Ok(format!("{line}; already running"));
    }
    let gui = in_gui.is_some()
        || launchctl(Launchctl::PrintDomain(Domain::Gui))?
            .status
            .success();
    let domain = if gui { Domain::Gui } else { Domain::User };
    let in_user = launchd_job_with(Domain::User, label, launchctl)?;
    let line = match domain {
        Domain::Gui => line,
        Domain::User => format!(
            "{line}, in {}: with no login at this Mac's screen it runs until the Mac restarts",
            domain.target(aterm_uds::peer::our_uid())
        ),
    };
    if !restart && domain == Domain::User && matches!(in_user, Some(Some(_))) && as_held(domain) {
        return Ok(format!("{line}; already running"));
    }
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    write_unit_file(&plist, &text(domain))?;
    let loaded_in = |held| match held {
        Domain::Gui => in_gui,
        Domain::User => in_user,
    };
    for held in [domain.other(), domain] {
        if loaded_in(held).is_some() {
            let _ = launchctl(Launchctl::Bootout(held, label));
            settle_launchd_bootout(held, label, launchctl)?;
        }
    }
    launchctl_checked(Launchctl::Bootstrap(domain, &plist), launchctl)?;
    let start = if restart {
        Launchctl::Restart(domain, label)
    } else {
        Launchctl::Kickstart(domain, label)
    };
    launchctl_checked(start, launchctl)?;
    Ok(line)
}

/// The bundle identifier of the app `program` is the main executable of
/// (`X.app/Contents/MacOS/<program>`, its `Contents/Info.plist`'s `CFBundleIdentifier`).
#[cfg(unix)]
#[must_use]
pub fn bundle_id_of(program: &Path) -> Option<String> {
    let macos = program.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let in_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|e| e == "app");
    if !in_bundle {
        return None;
    }
    let text = read_regular_string(&contents.join("Info.plist"), INFO_PLIST_MOST)?;
    let tag = "<key>CFBundleIdentifier</key>";
    let rest = text[text.find(tag)? + tag.len()..]
        .trim_start()
        .strip_prefix("<string>")?;
    let id = &rest[..rest.find("</string>")?];
    let plain = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    plain.then(|| id.to_owned())
}

/// The largest `Info.plist` [`bundle_id_of`] reads.
#[cfg(unix)]
const INFO_PLIST_MOST: usize = 256 * 1024;

/// Stop and remove `job`, for this login's own `home` ([`own_home`]).
pub fn uninstall(home: &Path, job: Job) -> crate::Result<bool> {
    check_own_home(home)?;
    let _decision = JobDecision::take(home, job)?;
    uninstall_job(home, job)
}

/// Turn a job off by choice. For the subscriber, publish that choice and remove
/// the job in one local decision, so an older automatic repair cannot undo it.
pub fn turn_off(home: &Path, job: Job) -> crate::Result<bool> {
    check_own_home(home)?;
    match job {
        Job::Subscriber => turn_off_subscriber_with(home, || uninstall_job(home, job)),
        Job::Server => turn_off_server_with(home, || uninstall_job(home, job)),
    }
}

fn turn_off_server_with(
    home: &Path,
    remove: impl FnOnce() -> crate::Result<bool>,
) -> crate::Result<bool> {
    let _decision = JobDecision::take(home, Job::Server)?;
    record_server_off_locked(home)?;
    remove()
}

fn turn_off_subscriber_with(
    home: &Path,
    remove: impl FnOnce() -> crate::Result<bool>,
) -> crate::Result<bool> {
    let _decision = JobDecision::take(home, Job::Subscriber)?;
    record_subscriber_off_locked(home)?;
    remove()
}

/// Exercise explicit subscriber On with an inert local service action.
#[cfg(any(test, debug_assertions))]
pub fn install_subscriber_for_test(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<String> {
    install_subscriber_with(home, install)
}

/// Exercise automatic subscriber repair with an inert local service action.
#[cfg(any(test, debug_assertions))]
pub fn repair_subscriber_for_test(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    repair_subscriber_with(home, install)
}

/// Exercise subscriber Off with an inert local service action.
#[cfg(any(test, debug_assertions))]
pub fn turn_off_subscriber_for_test(
    home: &Path,
    remove: impl FnOnce() -> crate::Result<bool>,
) -> crate::Result<bool> {
    turn_off_subscriber_with(home, remove)
}

fn uninstall_job(home: &Path, job: Job) -> crate::Result<bool> {
    let remove = |path: &Path| match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    };
    #[cfg(target_os = "macos")]
    {
        let label = job.label();
        let mut loaded = false;
        for domain in [Domain::Gui, Domain::User] {
            if launchd_job_with(domain, label, &mut launchctl)?.is_some() {
                launchctl_ok(Launchctl::Bootout(domain, label))?;
                loaded = true;
            }
        }
        let removed = remove(&plist_path(home, job))?;
        Ok(loaded || removed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = unit_path(home, job);
        if crate::fsx::kind(&unit)? == crate::fsx::Kind::Missing {
            return Ok(false);
        }
        let name = unit_name(job);
        if let Ok(systemctl) = systemctl() {
            let _ = run(&systemctl, &systemctl_argv(&["disable", "--now", &name]));
            remove(&unit)?;
            run_ok(&systemctl, &systemctl_argv(&["daemon-reload"]))?;
        } else {
            remove(&unit)?;
        }
        Ok(true)
    }
}

/// `job`'s state: absent for a home that is not this login's own ([`own_home`]).
pub fn state(home: &Path, job: Job) -> crate::Result<State> {
    state_until(home, job, Instant::now() + CHILD_HANG)
}

/// Observe a service without allowing child queries past `until`.
/// # Errors
/// The home, unit, service manager, or deadline cannot be read.
pub fn state_until(home: &Path, job: Job, until: Instant) -> crate::Result<State> {
    label_state_until(home, job.label(), until)
}

/// The node service's state ([`NODE_LABEL`]).
/// # Errors
/// As [`state`].
pub fn node_state(home: &Path) -> crate::Result<State> {
    label_state_until(home, NODE_LABEL, Instant::now() + CHILD_HANG)
}

/// [`state_until`] of the job labelled `label`.
fn label_state_until(home: &Path, label: &str, until: Instant) -> crate::Result<State> {
    // The login's job is not this home's: a scratch `HOME` has none.
    if check_own_home(home).is_err() {
        return Ok(State::Absent);
    }
    #[cfg(target_os = "macos")]
    {
        let unit = home
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{label}.plist"));
        let on_disk = crate::fsx::kind(&unit)? != crate::fsx::Kind::Missing;
        let mut loaded = None;
        for domain in [Domain::Gui, Domain::User] {
            if let Some(pid) = launchd_job_with(domain, label, &mut |call| {
                run_until(
                    Path::new(LAUNCHCTL),
                    &launchctl_argv(aterm_uds::peer::our_uid(), call),
                    until,
                )
            })? {
                loaded = Some((domain, pid));
                break;
            }
        }
        match loaded {
            Some((_, pid)) => Ok(State::Installed { pid, unit }),
            None if on_disk => Ok(State::Installed { pid: None, unit }),
            None => Ok(State::Absent),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let name = format!("{label}.service");
        let unit = home
            .join(".config")
            .join("systemd")
            .join("user")
            .join(&name);
        if crate::fsx::kind(&unit)? == crate::fsx::Kind::Missing {
            return Ok(State::Absent);
        }
        let pid = match systemctl() {
            Ok(systemctl) => {
                let out = run_until(
                    &systemctl,
                    &systemctl_argv(&["show", "-p", "MainPID", "--value", &name]),
                    until,
                )?;
                if out.status.success() {
                    main_pid(&String::from_utf8_lossy(&out.stdout))
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        Ok(State::Installed { pid, unit })
    }
}

/// Read the complete supported unit and the person's decisions for a signed repair.
/// The caller holds [`JobDecision`] across this read and the transaction.
/// # Errors
/// The home, decision records, unit, or executable cannot be read, or the unit is not a
/// definition written by aterm.
pub fn repair_observe(home: &Path, job: Job) -> crate::Result<crate::service_repair::Observed> {
    check_own_home(home)?;
    repair_observe_at(home, job)
}

fn repair_observe_at(home: &Path, job: Job) -> crate::Result<crate::service_repair::Observed> {
    let off_path = match job {
        Job::Subscriber => subscriber_off_path(home),
        Job::Server => server_off_path(home),
    };
    let off = crate::fsx::kind(&off_path)? != crate::fsx::Kind::Missing;
    let pin = crate::fsx::read_private(&pin_path(home, job), 4096)?
        .map(|bytes| {
            String::from_utf8(bytes.as_slice().to_vec())
                .map_err(|_| Error::new("non-UTF-8 service pin"))
        })
        .transpose()?;
    let path = unit_file(home, job);
    let unit = if crate::fsx::kind(&path)? == crate::fsx::Kind::Missing {
        None
    } else {
        let definition = read_regular_string(&path, UNIT_FILE_MOST)
            .ok_or_else(|| Error::new("cannot read complete service unit"))?;
        let argv = repair_argv(&definition)
            .ok_or_else(|| Error::new("unsupported service unit arguments"))?;
        let (program, args) = argv.split_first().ok_or_else(|| Error::new("empty unit"))?;
        let executable_sha256 = repair_image_hash(Path::new(program))?;
        let unit = crate::service_repair::Unit {
            program: program.clone(),
            args: args.to_vec(),
            image: executable_sha256.clone(),
            executable_sha256,
            definition,
        };
        repair_check_unit(home, job, &unit)?;
        Some(unit)
    };
    Ok(crate::service_repair::Observed { unit, off, pin })
}

fn repair_argv(definition: &str) -> Option<Vec<String>> {
    if cfg!(target_os = "macos") {
        plist_argv(definition)
    } else {
        exec_argv(definition)
    }
}

/// The complete supported launchd shape, shared by inspection and replacement.
/// Exact rendering rejects additional manager directives or a custom environment.
struct RepairPlistShape {
    domain: Domain,
    attributed: bool,
    policy: LaunchdPolicy,
}

fn repair_plist_shape(
    home: &Path,
    job: Job,
    unit: &crate::service_repair::Unit,
) -> Option<RepairPlistShape> {
    owned_plist_shape(
        home,
        job,
        Path::new(&unit.program),
        &unit.args,
        &unit.definition,
    )
}

fn owned_plist_shape(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
    definition: &str,
) -> Option<RepairPlistShape> {
    let log = log_path(home, job);
    let bundle = bundle_id_of(program);
    for domain in [Domain::Gui, Domain::User] {
        for (id, attributed) in [(None, false), (bundle.as_deref(), true)] {
            if attributed && id.is_none() {
                continue;
            }
            for path in [LaunchdPath::Managed, LaunchdPath::Inherited] {
                for subscriber in [
                    SubscriberProcessType::Interactive,
                    SubscriberProcessType::Background,
                ] {
                    let policy = LaunchdPolicy { path, subscriber };
                    if definition
                        == plist_text_with_path(
                            job.label(),
                            program,
                            args,
                            &log,
                            domain,
                            id,
                            policy,
                        )
                    {
                        return Some(RepairPlistShape {
                            domain,
                            attributed,
                            policy,
                        });
                    }
                }
            }
        }
    }
    None
}

/// Produce a replacement of a supported unit, retaining its arguments, launchd domain,
/// bundle attribution policy and inherited or managed PATH.
/// # Errors
/// The old definition is unsupported or the new executable cannot be used by a service.
pub fn repair_unit(
    home: &Path,
    job: Job,
    old: &crate::service_repair::Unit,
    program: &Path,
    image: &str,
) -> crate::Result<crate::service_repair::Unit> {
    if image.is_empty() {
        return Err(Error::new("service image identity must not be empty"));
    }
    repair_check_unit(home, job, old)?;
    check_argv(program, &old.args)?;
    let program_text = program
        .to_str()
        .ok_or_else(|| Error::new("non-UTF-8 executable"))?;
    let definition = if cfg!(target_os = "macos") {
        let log = log_path(home, job);
        let shape = repair_plist_shape(home, job, old)
            .ok_or_else(|| Error::new("service definition has unsupported manager directives"))?;
        let new_bundle = bundle_id_of(program);
        plist_text_with_path(
            job.label(),
            program,
            &old.args,
            &log,
            shape.domain,
            if shape.attributed {
                new_bundle.as_deref()
            } else {
                None
            },
            LaunchdPolicy {
                path: shape.policy.path,
                subscriber: if job == Job::Subscriber {
                    subscriber_process_type(program)
                } else {
                    SubscriberProcessType::Background
                },
            },
        )
    } else {
        systemd_unit(job.description(), program, &old.args)
    };
    let next = crate::service_repair::Unit {
        program: program_text.to_owned(),
        args: old.args.clone(),
        definition,
        image: image.to_owned(),
        executable_sha256: repair_image_hash(program)?,
    };
    repair_check_unit(home, job, &next)?;
    Ok(next)
}

fn repair_check_unit(
    home: &Path,
    job: Job,
    unit: &crate::service_repair::Unit,
) -> crate::Result<()> {
    let program = Path::new(&unit.program);
    let argv = repair_argv(&unit.definition).ok_or_else(|| Error::new("unsupported unit"))?;
    if argv.first() != Some(&unit.program) || argv.get(1..) != Some(unit.args.as_slice()) {
        return Err(Error::new(
            "unit executable or arguments disagree with receipt",
        ));
    }
    let shaped = match job {
        Job::Subscriber => unit.args == ["secrets", "watch"],
        Job::Server => unit
            .args
            .get(..2)
            .is_some_and(|words| words == ["secrets", "serve"]),
    };
    if !shaped {
        return Err(Error::new("unit does not run the requested vault service"));
    }
    // Additional manager directives can override argv or add executable hooks. Only complete
    // definitions produced by this service writer are candidates for independent activation.
    let supported = if cfg!(target_os = "macos") {
        repair_plist_shape(home, job, unit).is_some()
    } else {
        unit.definition == systemd_unit(job.description(), program, &unit.args)
    };
    if !supported {
        return Err(Error::new(
            "service definition has unsupported manager directives",
        ));
    }
    Ok(())
}

/// Hash an executable through an open regular file, refusing a concurrent file replacement.
/// # Errors
/// The file cannot be read, is a symlink or non-regular file, or changes while read.
pub fn repair_image_hash(program: &Path) -> crate::Result<String> {
    repair_hash_file(program, false)
}

fn repair_hash_file(program: &Path, mapped_image: bool) -> crate::Result<String> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(if mapped_image {
            libc::O_NONBLOCK | libc::O_CLOEXEC
        } else {
            libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC
        })
        .open(program)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(Error::new("service image is not a regular file"));
    }
    let mut hash = aterm_digest::Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    let named = if mapped_image {
        std::fs::metadata(program)?
    } else {
        std::fs::symlink_metadata(program)?
    };
    let identity = |m: &std::fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    if identity(&before) != identity(&after) || identity(&after) != identity(&named) {
        return Err(Error::new("service image changed while hashing"));
    }
    Ok(aterm_codec::hex::encode_lower(&hash.finalize()))
}

/// Install the journal's exact service definition without changing Off or pin records.
/// The transaction holds [`JobDecision`] and has already persisted its rollback journal.
/// # Errors
/// The definition is unsupported, its image changed, or the manager refuses the restart.
pub fn repair_install(
    home: &Path,
    job: Job,
    unit: &crate::service_repair::Unit,
) -> crate::Result<()> {
    check_own_home(home)?;
    repair_check_unit(home, job, unit)?;
    check_argv(Path::new(&unit.program), &unit.args)?;
    if repair_image_hash(Path::new(&unit.program))? != unit.executable_sha256 {
        return Err(Error::new("service image no longer matches the journal"));
    }
    if job == Job::Server && matches!(daemon_state_for_home(home)?, State::Installed { .. }) {
        return Err(Error::new("a system vault daemon owns this server"));
    }
    #[cfg(target_os = "macos")]
    {
        let boundary = aterm_types::update_boundary::Boundary::process();
        let _guard = boundary
            .hold(
                aterm_types::update_boundary::GuardedItem::LaunchdBootstrap,
                || Ok::<(), std::convert::Infallible>(()),
            )
            .unwrap_or_else(|never| match never {});
        if job == Job::Subscriber
            && repair_plist_shape(home, job, unit)
                .is_some_and(|shape| shape.policy.subscriber == SubscriberProcessType::Interactive)
            && subscriber_process_type(Path::new(&unit.program))
                != SubscriberProcessType::Interactive
        {
            return Err(Error::new(
                "subscriber image no longer supplies its thread scheduling policy",
            ));
        }
        repair_restart_launchd(home, job, unit, &mut launchctl)?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let manager = systemctl()?;
        write_unit_file(&unit_file(home, job), &unit.definition)?;
        run_ok(&manager, &systemctl_argv(&["daemon-reload"]))?;
        let name = unit_name(job);
        run_ok(&manager, &systemctl_argv(&["enable", "--now", &name]))?;
        run_ok(&manager, &systemctl_argv(&["restart", &name]))?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn repair_restart_launchd(
    home: &Path,
    job: Job,
    unit: &crate::service_repair::Unit,
    manager: &mut LaunchctlRun<'_>,
) -> crate::Result<()> {
    let domain = if unit
        .definition
        .contains("\t<key>LimitLoadToSessionType</key>\n\t<string>Background</string>\n")
    {
        Domain::User
    } else {
        Domain::Gui
    };
    // The journal precedes this stop. Every retry restores this exact definition.
    for held in [Domain::Gui, Domain::User] {
        if launchd_job_with(held, job.label(), manager)?.is_some() {
            launchctl_checked(Launchctl::Bootout(held, job.label()), manager)?;
            settle_launchd_bootout(held, job.label(), manager)?;
        }
    }
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    let path = plist_path(home, job);
    write_unit_file(&path, &unit.definition)?;
    launchctl_checked(Launchctl::Bootstrap(domain, &path), manager)?;
    launchctl_checked(Launchctl::Kickstart(domain, job.label()), manager)?;
    Ok(())
}

/// Prove the manager's live process still runs the expected executable and bytes.
/// This is image evidence; the caller additionally verifies authenticated service behavior.
/// # Errors
/// The service is not running, the process changes during the check, or its image differs.
pub fn repair_prove_image(
    home: &Path,
    job: Job,
    unit: &crate::service_repair::Unit,
) -> crate::Result<()> {
    let State::Installed { pid: Some(pid), .. } = state(home, job)? else {
        return Err(Error::new("service has no running process"));
    };
    let pid = i32::try_from(pid).map_err(|_| Error::new("invalid service pid"))?;
    let identity = aterm_uds::process::read_identity(pid)
        .ok_or_else(|| Error::new("service process identity unavailable"))?;
    let expected = std::fs::canonicalize(&unit.program)?;
    let actual = aterm_uds::process::read_process_path(pid)
        .ok_or_else(|| Error::new("service executable unavailable"))?;
    if std::fs::canonicalize(&actual)? != expected
        || repair_image_hash(&actual)? != unit.executable_sha256
    {
        return Err(Error::new("running service image differs from the journal"));
    }
    repair_prove_loaded(pid, &actual, &unit.executable_sha256)?;
    if aterm_uds::process::read_identity(pid) != Some(identity)
        || aterm_uds::process::read_process_path(pid).as_ref() != Some(&actual)
        || !matches!(state(home, job)?, State::Installed { pid: Some(now), .. } if now == pid as u32)
    {
        return Err(Error::new("service process changed during image proof"));
    }
    Ok(())
}

fn repair_prove_loaded(pid: i32, program: &Path, image: &str) -> crate::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let live = aterm_uds::process::read_code_directory_hash(pid)
            .ok_or_else(|| Error::new("running service CodeDirectory hash unavailable"))?;
        let output = run(
            Path::new(aterm_uds::os_path!("codesign")),
            &[
                "--display".to_owned(),
                "--verbose=4".to_owned(),
                program.to_string_lossy().into_owned(),
            ],
        )?;
        if !output.status.success() {
            return Err(Error::new("cannot read service image code signature"));
        }
        let report = String::from_utf8_lossy(&output.stderr);
        let expected = report
            .lines()
            .find_map(|line| line.strip_prefix("CDHash="))
            .ok_or_else(|| Error::new("service image has no CodeDirectory hash"))?;
        if expected != aterm_codec::hex::encode_lower(&live) {
            return Err(Error::new(
                "loaded service CodeDirectory differs from signed image",
            ));
        }
        let _ = image;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = program;
        if repair_hash_file(&PathBuf::from(format!("/proc/{pid}/exe")), true)? != image {
            return Err(Error::new("mapped service image differs from journal"));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (pid, program, image);
        Err(Error::new(
            "loaded service image proof unsupported on this platform",
        ))
    }
}

/// [`DAEMON_LABEL`]'s state for `home`: absent for a scratch `home` that is not this login's own
/// ([`own_home`], so isolated CLI tests ask launchd nothing).
pub fn daemon_state_for_home(home: &Path) -> crate::Result<State> {
    if check_own_home(home).is_err() {
        return Ok(State::Absent);
    }
    daemon_state()
}

/// Observe the system daemon under the caller's observation deadline.
/// # Errors
/// The service manager or its unit cannot be read before `until`.
pub fn daemon_state_for_home_until(home: &Path, until: Instant) -> crate::Result<State> {
    if check_own_home(home).is_err() {
        return Ok(State::Absent);
    }
    #[cfg(target_os = "macos")]
    {
        let plist = Path::new(DAEMON_PLIST);
        let on_disk = crate::fsx::kind(plist)? != crate::fsx::Kind::Missing;
        daemon_state_with(plist, on_disk, &mut |call| {
            run_until(Path::new(LAUNCHCTL), &system_launchctl_argv(call), until)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = until;
        Ok(State::Absent)
    }
}

/// [`DAEMON_LABEL`]'s state in `system` on macOS; [`State::Absent`] elsewhere.
pub fn daemon_state() -> crate::Result<State> {
    #[cfg(target_os = "macos")]
    {
        let plist = Path::new(DAEMON_PLIST);
        let on_disk = crate::fsx::kind(plist)? != crate::fsx::Kind::Missing;
        daemon_state_with(plist, on_disk, &mut |call| {
            run(Path::new(LAUNCHCTL), &system_launchctl_argv(call))
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(State::Absent)
    }
}

/// [`daemon_state`]'s judgement over an injected `launchctl` call and plist check.
pub fn daemon_state_with(
    plist: &Path,
    on_disk: bool,
    launchctl: &mut dyn FnMut(SystemLaunchctl<'_>) -> crate::Result<std::process::Output>,
) -> crate::Result<State> {
    let out = launchctl(SystemLaunchctl::Print(DAEMON_LABEL))?;
    if out.status.success() {
        return Ok(State::Installed {
            pid: print_pid(&String::from_utf8_lossy(&out.stdout)),
            unit: plist.to_path_buf(),
        });
    }
    if on_disk {
        return Ok(State::Installed {
            pid: None,
            unit: plist.to_path_buf(),
        });
    }
    Ok(State::Absent)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
