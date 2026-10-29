// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manual macOS TCC and System Settings probes (`person` baseline items).

use super::{Ctx, Item};
use crate::model::{Probe, Read, User};
use crate::port::{Cmd, Ran, RunFailed, System};
use serde::Deserialize;
use std::time::Duration;

/// `[person]` baseline configuration table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Whether Remote Login must be enabled.
    #[serde(default)]
    pub remote_login: Option<bool>,
    /// Whether Remote Login's "Allow full disk access for remote users" must be enabled.
    #[serde(default)]
    pub remote_full_disk_access: Option<bool>,
    /// Whether Screen Sharing and its TCC grants must be enabled.
    #[serde(default)]
    pub screen_sharing: Option<bool>,
    /// Applications requiring Full Disk Access.
    #[serde(default)]
    pub full_disk_access: Vec<String>,
    /// Applications requiring Developer Tools access.
    #[serde(default)]
    pub developer_tools: Vec<String>,
}

/// Path to the system TCC SQLite database.
pub const TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// Maximum size of a TCC SQLite snapshot read into memory (`TCC.db` or its `-wal`).
const TCC_DB_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum lock-free snapshot retries when `tccd` is writing concurrently.
const TCC_DB_READS: usize = 3;

/// Pause between snapshot retries when `tccd` updates `TCC.db` mid-read.
const TCC_DB_PAUSE: Duration = Duration::from_millis(10);

/// Path to `/bin/launchctl`.
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// Diagnostic message when the system TCC database cannot be opened over SSH.
pub const NEEDS_REMOTE_FDA: &str = "turn on Allow full disk access for remote users";

/// Path to the installed `aterm` executable.
pub const ATERM_EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// System Settings URL for Privacy & Security ▸ Full Disk Access.
pub const PANE_FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles";

/// System Settings URL for Privacy & Security ▸ Developer Tools.
pub const PANE_DEVELOPER_TOOLS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_DevTools";

/// System Settings URL for General ▸ Sharing ▸ Remote Login.
pub const PANE_REMOTE_LOGIN: &str =
    "x-apple.systempreferences:com.apple.Sharing-Settings.extension?Services_RemoteLogin";

/// System Settings URL for General ▸ Sharing ▸ Screen Sharing.
pub const PANE_SCREEN_SHARING: &str =
    "x-apple.systempreferences:com.apple.Sharing-Settings.extension?Services_ScreenSharing";

/// `launchd` service label for Remote Login (`sshd`).
pub const SSHD_LABEL: &str = "com.openssh.sshd";

/// `launchd` service label for Screen Sharing.
pub const SCREEN_SHARING_LABEL: &str = "com.apple.screensharing";

/// TCC client identifier for the Screen Sharing agent.
pub const SCREEN_SHARING_AGENT: &str = "com.apple.screensharing.agent";

/// How long one read may take. A probe never waits for a person; this only catches a hung
/// tool.
const READ_BUDGET: Duration = Duration::from_secs(20);

/// Known application short names and their macOS bundle identifiers.
pub const APPS: [(&str, &str); 1] = [("aterm", "com.aterm.aterm")];

/// Kind of manual macOS permission or sharing step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// General ▸ Sharing ▸ Remote Login.
    RemoteLogin,
    /// General ▸ Sharing ▸ Remote Login ▸ Allow full disk access for remote users.
    RemoteFullDiskAccess,
    /// General ▸ Sharing ▸ Screen Sharing.
    ScreenSharing,
    /// Privacy & Security ▸ Full Disk Access for an app.
    FullDiskAccess,
    /// Privacy & Security ▸ Developer Tools for an app.
    DeveloperTools,
}

impl Kind {
    /// All step kinds in execution order.
    pub const ALL: [Self; 5] = [
        Self::RemoteLogin,
        Self::RemoteFullDiskAccess,
        Self::ScreenSharing,
        Self::FullDiskAccess,
        Self::DeveloperTools,
    ];

    /// Returns the step identifier prefix.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RemoteLogin => "remote_login",
            Self::RemoteFullDiskAccess => "remote_full_disk_access",
            Self::ScreenSharing => "screen_sharing",
            Self::FullDiskAccess => "full_disk_access",
            Self::DeveloperTools => "developer_tools",
        }
    }

    /// Returns human-readable instructions for locating the setting.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::RemoteLogin => {
                "System Settings \u{25b8} General \u{25b8} Sharing \u{25b8} Remote Login"
            }
            Self::RemoteFullDiskAccess => {
                "System Settings \u{25b8} General \u{25b8} Sharing \u{25b8} Remote Login \u{24d8} \u{25b8} Allow full disk access for remote users"
            }
            Self::ScreenSharing => {
                "System Settings \u{25b8} General \u{25b8} Sharing \u{25b8} Screen Sharing"
            }
            Self::FullDiskAccess => {
                "System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access"
            }
            Self::DeveloperTools => {
                "System Settings \u{25b8} Privacy & Security \u{25b8} Developer Tools"
            }
        }
    }

    /// Returns the `x-apple.systempreferences` URL for the setting pane.
    #[must_use]
    pub const fn pane(self) -> &'static str {
        match self {
            Self::RemoteLogin | Self::RemoteFullDiskAccess => PANE_REMOTE_LOGIN,
            Self::ScreenSharing => PANE_SCREEN_SHARING,
            Self::FullDiskAccess => PANE_FULL_DISK_ACCESS,
            Self::DeveloperTools => PANE_DEVELOPER_TOOLS,
        }
    }

    /// Returns whether this step requires physical console access.
    #[must_use]
    pub const fn at_console(self) -> bool {
        matches!(self, Self::RemoteLogin | Self::ScreenSharing)
    }

    /// Returns whether probing this step reads the system TCC database.
    #[must_use]
    pub const fn reads_tcc(self) -> bool {
        matches!(
            self,
            Self::ScreenSharing | Self::FullDiskAccess | Self::DeveloperTools
        )
    }
}

/// Manual setup step with an optional target application.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PersonStep {
    /// Step kind.
    pub kind: Kind,
    /// Application name as written in the baseline (`None` for system-wide steps).
    pub app: Option<String>,
    /// Resolved macOS bundle identifier (`None` for system-wide steps).
    pub bundle: Option<String>,
}

impl PersonStep {
    /// Creates a system-wide step with no target application.
    #[must_use]
    pub const fn of(kind: Kind) -> Self {
        Self {
            kind,
            app: None,
            bundle: None,
        }
    }

    /// Creates a per-application step.
    ///
    /// # Errors
    /// Returns an error if `app` is neither a known short name nor a valid bundle ID.
    pub fn for_app(kind: Kind, app: &str) -> Result<Self, String> {
        Ok(Self {
            kind,
            app: Some(app.to_owned()),
            bundle: Some(bundle_id(app)?),
        })
    }

    /// Parses a step identifier (`<kind>` or `<kind>:<app>`).
    ///
    /// # Errors
    /// Returns an error if `id` does not name a valid step and application.
    pub fn parse(id: &str) -> Result<Self, String> {
        let (name, app) = match id.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (id, None),
        };
        let kind = Kind::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .ok_or_else(|| {
                format!(
                    "{id:?} is not a person step (remote_login, remote_full_disk_access, \
                     screen_sharing, full_disk_access:<app>, developer_tools:<app>)"
                )
            })?;
        let per_app = matches!(kind, Kind::FullDiskAccess | Kind::DeveloperTools);
        match (per_app, app) {
            (true, Some(app)) => Self::for_app(kind, app),
            (true, None) => Err(format!("{id:?} names no app ({name}:<app>)")),
            (false, None) => Ok(Self::of(kind)),
            (false, Some(_)) => Err(format!("{name} takes no app")),
        }
    }

    /// Formats the canonical step identifier.
    #[must_use]
    pub fn id(&self) -> String {
        match &self.app {
            Some(app) => format!("{}:{app}", self.kind.name()),
            None => self.kind.name().to_owned(),
        }
    }

    /// Formats a human-readable summary line for reports.
    #[must_use]
    pub fn title(&self) -> String {
        let app = self.app.as_deref().unwrap_or("");
        match self.kind {
            Kind::RemoteLogin => "Remote Login on".to_owned(),
            Kind::RemoteFullDiskAccess => "full disk access for remote users".to_owned(),
            Kind::ScreenSharing => "Screen Sharing on, with control".to_owned(),
            Kind::FullDiskAccess => format!("Full Disk Access for {app}"),
            Kind::DeveloperTools => format!("{app} in Developer Tools"),
        }
    }

    /// Formats instructions and the System Settings pane URL.
    #[must_use]
    pub fn how(&self) -> String {
        let app = self.app.as_deref().unwrap_or("");
        let what = match self.kind {
            Kind::RemoteLogin => format!("turn on Remote Login in {}", self.kind.words()),
            Kind::RemoteFullDiskAccess => format!("turn on {}", self.kind.words()),
            Kind::ScreenSharing => format!(
                "turn on Screen Sharing in {}, with access for administrators",
                self.kind.words()
            ),
            Kind::FullDiskAccess | Kind::DeveloperTools => format!(
                "switch on {app} in {} (add it with + if it is not listed)",
                self.kind.words()
            ),
        };
        format!("{what}: {}", self.kind.pane())
    }
}

/// Resolves `app` from [`APPS`] or validates it as a bundle identifier.
///
/// # Errors
/// Returns an error if `app` is neither a known short name nor a valid bundle ID. A bundle
/// id is letters, digits, `.`, `-` and `_` only, so it can be quoted into the TCC query as
/// it is.
pub fn bundle_id(app: &str) -> Result<String, String> {
    if let Some((_, bundle)) = APPS.iter().find(|(name, _)| name.eq_ignore_ascii_case(app)) {
        return Ok((*bundle).to_owned());
    }
    let plain = app
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if app.contains('.') && plain && !app.starts_with('.') && !app.ends_with('.') {
        Ok(app.to_owned())
    } else {
        Err(format!(
            "person: unknown app {app:?}: name it by its bundle id (com.example.App), or one of {}",
            APPS.map(|(n, _)| n).join(", ")
        ))
    }
}

/// Row read from the TCC `access` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccRow {
    /// Client bundle ID or path.
    pub client: String,
    /// TCC service name (`kTCCService…`).
    pub service: String,
    /// Authorization value (`2` allowed, `0` denied).
    pub auth_value: i64,
}

/// Builds the host agent [`Cmd`] reading `clients` from [`TCC_DB`] in-process.
#[must_use]
pub fn tcc_cmd(clients: &[&str]) -> Cmd {
    Cmd::new(crate::protocol::AGENT_EXE)
        .args(["machines", "agent", "tcc"])
        .args(clients)
        .budget(READ_BUDGET)
}

/// Builds a host agent [`Cmd`] that probes read access to [`TCC_DB`] in-process.
#[must_use]
pub fn tcc_open_cmd() -> Cmd {
    tcc_cmd(&[])
}

struct TccColumns {
    client: usize,
    service: usize,
    auth_value: usize,
}

fn tcc_columns(schema: &aterm_uds::sqlite_read::TableSchema) -> Option<TccColumns> {
    Some(TccColumns {
        client: schema.column_index("client")?,
        service: schema.column_index("service")?,
        auth_value: schema.column_index("auth_value")?,
    })
}

/// Scans an in-memory SQLite `TCC.db` snapshot (`main` and optional `wal`) for `clients`'
/// rows in `access` (`clients = &[]` validates that `access` opens and scans).
///
/// # Errors
/// Returns an error if `main` and `wal` are not a readable SQLite 3 database with an
/// `access(client, service, auth_value)` table or a matched row holds malformed cells.
pub fn scan_tcc_bytes(
    main: &[u8],
    wal: Option<&[u8]>,
    clients: &[&str],
) -> Result<Vec<TccRow>, String> {
    use aterm_uds::sqlite_read::{SqlValue, scan_table_with};
    let scanned = scan_table_with(main, wal, "access", tcc_columns, |cols, row| {
        if clients.is_empty() {
            return None;
        }
        let client_cell = row.get(cols.client)?;
        let client_str = match client_cell {
            SqlValue::Text(s) => s.as_ref(),
            SqlValue::Blob(b) => match std::str::from_utf8(b) {
                Ok(s) => s,
                Err(_) => return Some(Err("the TCC database holds non-UTF-8 client bytes".into())),
            },
            SqlValue::Null | SqlValue::Int(_) | SqlValue::Float(_) => {
                return Some(Err("the TCC database holds a non-text client cell".into()));
            }
        };
        if !clients.contains(&client_str) {
            return None;
        }
        let service = match row.get(cols.service)? {
            SqlValue::Text(s) => s.as_ref().to_owned(),
            SqlValue::Blob(b) => match std::str::from_utf8(b) {
                Ok(s) => s.to_owned(),
                Err(_) => {
                    return Some(Err("the TCC database holds non-UTF-8 service bytes".into()));
                }
            },
            SqlValue::Null | SqlValue::Int(_) | SqlValue::Float(_) => {
                return Some(Err("the TCC database holds a non-text service cell".into()));
            }
        };
        let Some(auth_value) = row.get(cols.auth_value)?.as_int() else {
            return Some(Err("the TCC database holds a non-integer auth_value".into()));
        };
        Some(Ok(TccRow {
            client: client_str.to_owned(),
            service,
            auth_value,
        }))
    })
    .ok_or_else(|| "the TCC database did not answer: unreadable SQLite access table".to_owned())?;
    scanned.into_iter().collect()
}

fn open_tcc_no_link(db: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    options.open(db)
}

/// Reads `clients`' rows from `db` (`clients = &[]` checks read access and schema) in-process
/// with no lock, temporary copy, or subprocess.
///
/// # Errors
/// Returns [`NEEDS_REMOTE_FDA`] when TCC denies opening `db` (`EPERM`), or a diagnostic line
/// when `db` cannot be opened or decoded.
pub fn read_tcc_file(db: &std::path::Path, clients: &[&str]) -> Result<Vec<TccRow>, String> {
    use aterm_uds::foreign_db::{Unread, read_bytes};
    if let Err(e) = open_tcc_no_link(db) {
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::EPERM) {
            return Err(NEEDS_REMOTE_FDA.to_owned());
        }
        return Err(format!(
            "the TCC database did not answer: {}: {e}",
            db.display()
        ));
    }
    for attempt in 0..TCC_DB_READS {
        if attempt > 0 {
            std::thread::sleep(TCC_DB_PAUSE);
        }
        match read_bytes(db, TCC_DB_MAX_BYTES, |main, wal| {
            Some(scan_tcc_bytes(main, wal, clients))
        }) {
            Ok(result) => return result,
            Err(Unread::Changed | Unread::Writing) => {}
            Err(Unread::Missing) => {
                return Err(format!(
                    "the TCC database did not answer: {}: not a regular file",
                    db.display()
                ));
            }
            Err(Unread::Failed) => {
                return Err(format!(
                    "the TCC database did not answer: {}: unreadable database snapshot",
                    db.display()
                ));
            }
        }
    }
    Err(format!(
        "the TCC database did not answer: {}: changed during read",
        db.display()
    ))
}

/// `aterm machines agent tcc [client...]`: reads `clients` from [`TCC_DB`] through `sys`.
pub fn agent_tcc(
    sys: &dyn System,
    clients: &[&str],
    out: &mut dyn std::io::Write,
) -> std::process::ExitCode {
    for client in clients {
        if let Err(why) = bundle_id(client) {
            return crate::agent::refuse(out, &why);
        }
    }
    match sys.tcc_access(clients) {
        Ok(rows) => {
            for row in rows {
                let _ = writeln!(out, "{}|{}|{}", row.client, row.service, row.auth_value);
            }
            let _ = out.flush();
            std::process::ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(out, "not read: {why}");
            let _ = out.flush();
            std::process::ExitCode::FAILURE
        }
    }
}

/// Builds the `launchctl print-disabled system` [`Cmd`].
#[must_use]
pub fn print_disabled_cmd() -> Cmd {
    Cmd::new(LAUNCHCTL)
        .args(["print-disabled", "system"])
        .budget(READ_BUDGET)
}

/// Why the remote TCC read did not answer: the agent's own `not read: <why>` (including
/// [`NEEDS_REMOTE_FDA`]), else how the command ended.
fn tcc_refusal(ran: &Ran) -> String {
    if let Some(why) = ran
        .out()
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .and_then(|l| l.strip_prefix("not read: "))
    {
        return why.to_owned();
    }
    let err = ran.err();
    if err.contains("authorization denied") {
        NEEDS_REMOTE_FDA.to_owned()
    } else {
        format!("the TCC database did not answer: {}", ran.describe())
    }
}

/// Parses `client|service|auth_value` lines from agent TCC output.
///
/// # Errors
/// Returns an error if any non-empty line is malformed.
pub fn parse_tcc(text: &str) -> Result<Vec<TccRow>, String> {
    let mut rows = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.rsplitn(3, '|');
        let (Some(value), Some(service), Some(client)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("the TCC database printed {line:?}, not a row"));
        };
        let auth_value = value
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("the TCC database printed {line:?}, not a row"))?;
        rows.push(TccRow {
            client: client.to_owned(),
            service: service.to_owned(),
            auth_value,
        });
    }
    Ok(rows)
}

/// The rows for `clients` read through `run`, or why they could not be read.
fn tcc_rows(
    clients: &[&str],
    run: &mut dyn FnMut(&Cmd) -> Result<Ran, RunFailed>,
) -> Result<Vec<TccRow>, String> {
    match run(&tcc_cmd(clients)) {
        Ok(ran) if ran.success() => parse_tcc(&ran.out()),
        Ok(ran) => Err(tcc_refusal(&ran)),
        Err(e) => Err(format!("the TCC database did not answer: {e}")),
    }
}

/// Looks up `client`'s `auth_value` for `service` in `rows`.
#[must_use]
pub fn grant(rows: &[TccRow], client: &str, service: &str) -> Read<i64> {
    rows.iter()
        .find(|r| r.client == client && r.service == service)
        .map_or(Read::Absent, |r| Read::Value(r.auth_value))
}

/// Parses whether `label` is enabled in `launchctl print-disabled system` output.
#[must_use]
pub fn launchd_enabled(text: &str, label: &str) -> Read<bool> {
    if !text.contains("disabled services = {") {
        return Read::Unusable(format!(
            "launchctl print-disabled printed {:?}, not its service list",
            text.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
        ));
    }
    let quoted = format!("\"{label}\"");
    for line in text.lines() {
        let Some((name, value)) = line.split_once("=>") else {
            continue;
        };
        if name.trim() != quoted {
            continue;
        }
        return match value.trim() {
            "enabled" | "false" => Read::Value(true),
            "disabled" | "true" => Read::Value(false),
            other => Read::Unusable(format!("launchctl says {label} is {other:?}")),
        };
    }
    Read::Absent
}

/// Whether the system service `label` is enabled, read through `run`.
fn service_enabled(label: &str, run: &mut dyn FnMut(&Cmd) -> Result<Ran, RunFailed>) -> Read<bool> {
    match run(&print_disabled_cmd()) {
        Ok(ran) if ran.success() => launchd_enabled(&ran.out(), label),
        Ok(ran) => Read::Unusable(format!("launchctl print-disabled: {}", ran.describe())),
        Err(e) => Read::Unusable(format!("launchctl print-disabled: {e}")),
    }
}

/// Pre-gathered host facts and console user passed into [`probe_step`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Known<'a> {
    /// Cached result of the Remote Login Full Disk Access check.
    pub remote_full_disk_access: Option<bool>,
    /// Console user account, if any.
    pub console: Option<&'a User>,
}

trait StepPort {
    fn run_cmd(&mut self, cmd: &Cmd) -> Result<Ran, RunFailed>;
    fn tcc_access(&mut self, clients: &[&str]) -> Result<Vec<TccRow>, String>;
}

struct CmdPort<'a>(&'a mut dyn FnMut(&Cmd) -> Result<Ran, RunFailed>);

impl StepPort for CmdPort<'_> {
    fn run_cmd(&mut self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        (self.0)(cmd)
    }

    fn tcc_access(&mut self, clients: &[&str]) -> Result<Vec<TccRow>, String> {
        tcc_rows(clients, self.0)
    }
}

struct SysPort<'a>(&'a dyn System);

impl StepPort for SysPort<'_> {
    fn run_cmd(&mut self, cmd: &Cmd) -> Result<Ran, RunFailed> {
        self.0.run(cmd)
    }

    fn tcc_access(&mut self, clients: &[&str]) -> Result<Vec<TccRow>, String> {
        self.0.tcc_access(clients)
    }
}

/// Probes `step` using `run` to execute system and agent commands.
pub fn probe_step(
    step: &PersonStep,
    known: Known<'_>,
    run: &mut dyn FnMut(&Cmd) -> Result<Ran, RunFailed>,
) -> Probe {
    probe_step_port(step, known, &mut CmdPort(run))
}

fn probe_step_port(step: &PersonStep, known: Known<'_>, port: &mut dyn StepPort) -> Probe {
    let person = || Probe::Person {
        step: step.id(),
        how: step.how(),
    };
    match step.kind {
        Kind::RemoteLogin => match service_enabled(SSHD_LABEL, &mut |cmd| port.run_cmd(cmd)) {
            Read::Value(true) => Probe::Ok { now: "on".into() },
            Read::Value(false) | Read::Absent => person(),
            Read::Unusable(why) => Probe::Unusable { why },
        },
        Kind::RemoteFullDiskAccess => match known.remote_full_disk_access {
            Some(true) => Probe::Ok { now: "on".into() },
            Some(false) => person(),
            None => match port.tcc_access(&[]) {
                Ok(_) => Probe::Ok { now: "on".into() },
                Err(why) if why == NEEDS_REMOTE_FDA => person(),
                Err(why) => Probe::Unusable { why },
            },
        },
        Kind::ScreenSharing => {
            match service_enabled(SCREEN_SHARING_LABEL, &mut |cmd| port.run_cmd(cmd)) {
                Read::Value(true) => {}
                Read::Value(false) | Read::Absent => return person(),
                Read::Unusable(why) => return Probe::Unusable { why },
            }
            let rows = match port.tcc_access(&[SCREEN_SHARING_AGENT]) {
                Ok(rows) => rows,
                Err(why) => return Probe::Unusable { why },
            };
            let control = grant(&rows, SCREEN_SHARING_AGENT, "kTCCServicePostEvent");
            let view = grant(&rows, SCREEN_SHARING_AGENT, "kTCCServiceScreenCapture");
            if control == Read::Value(2) && view == Read::Value(2) {
                Probe::Ok {
                    now: "on, with control".into(),
                }
            } else {
                person()
            }
        }
        Kind::FullDiskAccess | Kind::DeveloperTools => {
            let service = if step.kind == Kind::FullDiskAccess {
                "kTCCServiceSystemPolicyAllFiles"
            } else {
                "kTCCServiceDeveloperTool"
            };
            let bundle = step.bundle.as_deref().unwrap_or("");
            match port.tcc_access(&[bundle]) {
                Ok(rows) => match grant(&rows, bundle, service) {
                    Read::Value(2) => Probe::Ok {
                        now: "granted".into(),
                    },
                    Read::Value(_) | Read::Absent => person(),
                    Read::Unusable(why) => Probe::Unusable { why },
                },
                Err(why) if why == NEEDS_REMOTE_FDA && step.kind == Kind::FullDiskAccess => {
                    aterm_record(step, known.console, &mut |cmd| port.run_cmd(cmd))
                        .unwrap_or(Probe::Unusable { why })
                }
                Err(why) => Probe::Unusable { why },
            }
        }
    }
}

/// aterm's own Full Disk Access record (design §6's fallback), read as the console user from
/// that user's running aterm: `devtools.list_probe` reads `no_full_disk_access` until aterm
/// has FDA and `read` after (design §1.8).
fn aterm_record(
    step: &PersonStep,
    console: Option<&User>,
    run: &mut dyn FnMut(&Cmd) -> Result<Ran, RunFailed>,
) -> Option<Probe> {
    if step.bundle.as_deref() != Some(APPS[0].1) {
        return None;
    }
    let cmd = Cmd::new(ATERM_EXE)
        .args(["ctl", "privacy", "--json"])
        .as_user(console?)
        .budget(READ_BUDGET);
    let ran = run(&cmd).ok().filter(Ran::success)?;
    let record: aterm_json::Value = aterm_json::from_str(&ran.out()).ok()?;
    if record.pointer("/identity/bundle_id")?.as_str()? != APPS[0].1 {
        return None;
    }
    match record.pointer("/devtools/list_probe")?.as_str()? {
        "read" => Some(Probe::Ok {
            now: "granted (aterm's own record)".into(),
        }),
        "no_full_disk_access" => Some(Probe::Person {
            step: step.id(),
            how: step.how(),
        }),
        _ => None,
    }
}

/// One person step as an item.
struct PersonItem {
    step: PersonStep,
}

impl Item for PersonItem {
    fn id(&self) -> String {
        format!("person.{}", self.step.id())
    }

    fn family(&self) -> &'static str {
        "person"
    }

    fn title(&self) -> String {
        self.step.title()
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let known = Known {
            remote_full_disk_access: ctx.facts.remote_full_disk_access,
            console: ctx.console.as_ref(),
        };
        probe_step_port(&self.step, known, &mut SysPort(sys))
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(format!("only a person can do this: {}", self.step.how()))
    }
}

/// Expands `table` into ordered [`PersonStep`]s.
///
/// # Errors
/// Returns an error if any configured application name is invalid.
pub fn steps(table: &Table) -> Result<Vec<PersonStep>, String> {
    let mut steps = Vec::new();
    for (wanted, kind) in [
        (table.remote_login, Kind::RemoteLogin),
        (table.remote_full_disk_access, Kind::RemoteFullDiskAccess),
        (table.screen_sharing, Kind::ScreenSharing),
    ] {
        if wanted == Some(true) {
            steps.push(PersonStep::of(kind));
        }
    }
    for app in &table.full_disk_access {
        steps.push(PersonStep::for_app(Kind::FullDiskAccess, app)?);
    }
    for app in &table.developer_tools {
        steps.push(PersonStep::for_app(Kind::DeveloperTools, app)?);
    }
    Ok(steps)
}

/// Expands `table` into boxed [`Item`]s.
///
/// # Errors
/// Returns an error if any configured application name is invalid.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    Ok(steps(table)?
        .into_iter()
        .map(|step| Box::new(PersonItem { step }) as Box<dyn Item>)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::HostFacts;
    use crate::port::Fake;

    /// `launchctl print-disabled system`, captured in the macOS 27.0 VM (26A428) with Remote
    /// Login and Screen Sharing turned on in System Settings.
    const PRINT_DISABLED_VM: &str = "\n\tdisabled services = {\n\t\t\"com.apple.screensharing\" => enabled\n\t\t\"com.apple.CSCSupportd\" => disabled\n\t\t\"com.apple.mdmclient.daemon.runatboot\" => disabled\n\t\t\"com.apple.ftpd\" => disabled\n\t\t\"com.openssh.sshd\" => enabled\n\t}\n";

    /// The same on a Mac (27.0.1) where Screen Sharing was never turned on: no override.
    const PRINT_DISABLED_NO_SCREEN_SHARING: &str = "\n\tdisabled services = {\n\t\t\"com.apple.CSCSupportd\" => disabled\n\t\t\"com.apple.ftpd\" => disabled\n\t\t\"com.apple.mdmclient.daemon.runatboot\" => disabled\n\t\t\"com.openssh.sshd\" => enabled\n\t\t\"com.apple.dhcp6d\" => enabled\n\t}\n";

    /// The VM's rows for the screen-sharing agent and aterm, read as root over SSH with remote
    /// FDA on: Screen Sharing granted, aterm listed under Full Disk Access but switched off.
    const TCC_VM: &str = "com.apple.screensharing.agent|kTCCServicePostEvent|2\ncom.apple.screensharing.agent|kTCCServiceScreenCapture|2\ncom.aterm.aterm|kTCCServiceSystemPolicyAllFiles|0\n";

    fn ctx() -> Ctx {
        Ctx {
            facts: HostFacts::default(),
            users: Vec::new(),
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    fn admin() -> User {
        User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }
    }

    fn item(id: &str) -> PersonItem {
        PersonItem {
            step: PersonStep::parse(id).unwrap(),
        }
    }

    fn rows_of(text: &str) -> Result<Vec<TccRow>, String> {
        parse_tcc(text)
    }

    fn denied() -> Result<Vec<TccRow>, String> {
        Err(NEEDS_REMOTE_FDA.to_owned())
    }

    #[test]
    fn the_remote_tcc_cmd_routes_through_the_agents_native_tcc_verb() {
        assert_eq!(
            tcc_cmd(&["com.aterm.aterm"]).display(),
            "/Library/Management/aterm/bin/aterm machines agent tcc com.aterm.aterm"
        );
        assert_eq!(
            tcc_open_cmd().display(),
            "/Library/Management/aterm/bin/aterm machines agent tcc"
        );

        let sys = Fake::root();
        sys.on_tcc(
            &["com.aterm.aterm"],
            rows_of("com.aterm.aterm|kTCCServiceDeveloperTool|2\n"),
        );
        let mut out = Vec::new();
        assert_eq!(
            agent_tcc(&sys, &["com.aterm.aterm"], &mut out),
            std::process::ExitCode::SUCCESS
        );
        let step = PersonStep::parse("developer_tools:aterm").unwrap();
        let probe = probe_step(&step, Known::default(), &mut |cmd| {
            assert_eq!(cmd, &tcc_cmd(&["com.aterm.aterm"]));
            Ok(Ran::ok(&String::from_utf8(out.clone()).unwrap()))
        });
        assert_eq!(
            probe,
            Probe::Ok {
                now: "granted".into()
            }
        );

        let refused_sys = Fake::root();
        refused_sys.on_tcc(&[], denied());
        let mut refused_out = Vec::new();
        assert_eq!(
            agent_tcc(&refused_sys, &[], &mut refused_out),
            std::process::ExitCode::FAILURE
        );
        let remote_fda = PersonStep::parse("remote_full_disk_access").unwrap();
        let fda_probe = probe_step(&remote_fda, Known::default(), &mut |cmd| {
            assert_eq!(cmd, &tcc_open_cmd());
            Ok(Ran {
                status: Some(1),
                stdout: refused_out.clone(),
                stderr: Vec::new(),
            })
        });
        assert!(matches!(fda_probe, Probe::Person { .. }), "{fda_probe:?}");

        let mut bad_out = Vec::new();
        assert_eq!(
            agent_tcc(&Fake::root(), &["com.x');drop"], &mut bad_out),
            std::process::ExitCode::from(crate::agent::REFUSED)
        );
        assert!(String::from_utf8_lossy(&bad_out).starts_with("refused: "));
    }

    /// THE READ TAKES NO LOCK ON tccd's FILE AND SPAWNS NO SUBPROCESS: [`read_tcc_file`]
    /// answers rows from a WAL-backed database while the `EXCLUSIVE` byte-range lock a
    /// committing writer holds (`PENDING_BYTE` and the 511 bytes after it) is taken.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_tcc_read_takes_no_lock_a_writer_would_meet() {
        use std::os::fd::AsRawFd as _;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("TCC.db");
        let made = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .arg(&db)
            .arg(
                "PRAGMA journal_mode=WAL; \
                 CREATE TABLE access (service TEXT, client TEXT, client_type INTEGER, auth_value INTEGER); \
                 INSERT INTO access VALUES ('kTCCServiceDeveloperTool', 'com.aterm.aterm', 0, 2); \
                 INSERT INTO access VALUES ('kTCCServicePostEvent', 'com.apple.screensharing.agent', 0, 2);",
            )
            .output()
            .expect("made");
        assert!(made.status.success(), "{made:?}");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&db)
            .expect("open to lock");
        // SAFETY: a zeroed `flock` is a valid value of the plain C struct.
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = 0x4000_0000;
        fl.l_len = 512;
        // SAFETY: `fl` is valid for the call and the descriptor is open.
        let rc = unsafe { libc::fcntl(held.as_raw_fd(), libc::F_SETLK, &raw const fl) };
        assert_eq!(rc, 0, "the writer's lock is taken");
        let control = aterm_uds::spawnfd::command("/usr/bin/sqlite3")
            .args(["-readonly", "-bail"])
            .arg(&db)
            .arg("SELECT client, service, auth_value FROM access WHERE client = 'com.aterm.aterm'")
            .output()
            .expect("sqlite3 control");
        assert!(
            String::from_utf8_lossy(&control.stderr).contains("database is locked"),
            "{control:?}"
        );
        assert_eq!(read_tcc_file(&db, &[]), Ok(Vec::new()));
        assert_eq!(
            read_tcc_file(&db, &["com.aterm.aterm"]),
            Ok(vec![TccRow {
                client: "com.aterm.aterm".into(),
                service: "kTCCServiceDeveloperTool".into(),
                auth_value: 2,
            }])
        );
        drop(held);

        let corrupt = dir.path().join("corrupt.db");
        std::fs::write(&corrupt, b"not a sqlite file").expect("corrupt");
        let bad = read_tcc_file(&corrupt, &["com.aterm.aterm"]).unwrap_err();
        assert_ne!(bad, NEEDS_REMOTE_FDA);
    }

    #[test]
    fn a_database_that_fails_to_open_for_another_reason_is_unusable_not_the_fda_switch() {
        let gone = Err("the TCC database did not answer: unable to open database file".to_owned());
        let sys = Fake::root();
        sys.on_tcc(&[], gone.clone());
        assert!(matches!(
            item("remote_full_disk_access").probe(&sys, &ctx()),
            Probe::Unusable { .. }
        ));
        let sys = Fake::root();
        sys.on_tcc(&["com.aterm.aterm"], gone);
        let probe = item("developer_tools:aterm").probe(&sys, &ctx());
        assert!(
            matches!(&probe, Probe::Unusable { why } if why != NEEDS_REMOTE_FDA),
            "{probe:?}"
        );
    }

    #[test]
    fn rows_parse_from_the_vms_output() {
        let rows = parse_tcc(TCC_VM).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            grant(&rows, "com.aterm.aterm", "kTCCServiceSystemPolicyAllFiles"),
            Read::Value(0)
        );
        assert_eq!(
            grant(&rows, "com.aterm.aterm", "kTCCServiceDeveloperTool"),
            Read::Absent
        );
        assert!(parse_tcc("not a row\n").is_err());
    }

    #[test]
    fn launchd_overrides_read_enabled_disabled_and_absent_apart() {
        assert_eq!(
            launchd_enabled(PRINT_DISABLED_VM, SSHD_LABEL),
            Read::Value(true)
        );
        assert_eq!(
            launchd_enabled(PRINT_DISABLED_VM, "com.apple.ftpd"),
            Read::Value(false)
        );
        assert_eq!(
            launchd_enabled(PRINT_DISABLED_NO_SCREEN_SHARING, SCREEN_SHARING_LABEL),
            Read::Absent
        );
        assert!(matches!(
            launchd_enabled("Bad request.\n", SSHD_LABEL),
            Read::Unusable(_)
        ));
    }

    #[test]
    fn remote_login_reads_ok_from_the_vm_and_is_a_person_step_when_off() {
        let sys = Fake::root();
        sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
        assert_eq!(
            item("remote_login").probe(&sys, &ctx()),
            Probe::Ok { now: "on".into() }
        );
        let sys = Fake::root();
        sys.on(
            &print_disabled_cmd().display(),
            Ran::ok(&PRINT_DISABLED_VM.replace("\"com.openssh.sshd\" => enabled", "")),
        );
        let Probe::Person { step, how } = item("remote_login").probe(&sys, &ctx()) else {
            panic!("expected a person step");
        };
        assert_eq!(step, "remote_login");
        assert!(how.contains("Sharing \u{25b8} Remote Login"), "{how}");
        assert!(how.ends_with(PANE_REMOTE_LOGIN), "{how}");
    }

    #[test]
    fn screen_sharing_needs_the_service_on_and_both_agent_grants() {
        let sys = Fake::root();
        sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
        sys.on_tcc(&[SCREEN_SHARING_AGENT], rows_of(TCC_VM));
        assert_eq!(
            item("screen_sharing").probe(&sys, &ctx()),
            Probe::Ok {
                now: "on, with control".into()
            }
        );
        let sys = Fake::root();
        sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
        sys.on_tcc(
            &[SCREEN_SHARING_AGENT],
            rows_of("com.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
        );
        assert!(matches!(
            item("screen_sharing").probe(&sys, &ctx()),
            Probe::Person { .. }
        ));
        let sys = Fake::root();
        sys.on(
            &print_disabled_cmd().display(),
            Ran::ok(PRINT_DISABLED_NO_SCREEN_SHARING),
        );
        assert!(matches!(
            item("screen_sharing").probe(&sys, &ctx()),
            Probe::Person { .. }
        ));
        assert_eq!(sys.calls().len(), 1);
    }

    #[test]
    fn full_disk_access_is_granted_only_at_auth_value_two() {
        let sys = Fake::root();
        sys.on_tcc(&["com.aterm.aterm"], rows_of(TCC_VM));
        let Probe::Person { step, how } = item("full_disk_access:aterm").probe(&sys, &ctx()) else {
            panic!("aterm is listed but off in the VM");
        };
        assert_eq!(step, "full_disk_access:aterm");
        assert!(
            how.contains("Privacy & Security \u{25b8} Full Disk Access"),
            "{how}"
        );
        assert!(how.ends_with(PANE_FULL_DISK_ACCESS), "{how}");

        let sys = Fake::root();
        sys.on_tcc(
            &["com.aterm.aterm"],
            rows_of("com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n"),
        );
        assert_eq!(
            item("full_disk_access:aterm").probe(&sys, &ctx()),
            Probe::Ok {
                now: "granted".into()
            }
        );
    }

    #[test]
    fn developer_tools_reads_its_own_service_and_an_unlisted_app_is_a_person_step() {
        let sys = Fake::root();
        sys.on_tcc(
            &["com.aterm.aterm"],
            rows_of("com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n"),
        );
        let Probe::Person { step, how } = item("developer_tools:aterm").probe(&sys, &ctx()) else {
            panic!("aterm is not in Developer Tools");
        };
        assert_eq!(step, "developer_tools:aterm");
        assert!(how.ends_with(PANE_DEVELOPER_TOOLS), "{how}");
        let sys = Fake::root();
        sys.on_tcc(
            &["com.aterm.aterm"],
            rows_of("com.aterm.aterm|kTCCServiceDeveloperTool|2\n"),
        );
        assert!(matches!(
            item("developer_tools:aterm").probe(&sys, &ctx()),
            Probe::Ok { .. }
        ));
    }

    #[test]
    fn a_closed_tcc_database_is_unusable_never_a_grant_or_a_person_step() {
        for id in [
            "screen_sharing",
            "full_disk_access:aterm",
            "developer_tools:aterm",
        ] {
            let sys = Fake::root();
            sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
            sys.on_tcc(&[SCREEN_SHARING_AGENT], denied());
            sys.on_tcc(&["com.aterm.aterm"], denied());
            assert_eq!(
                item(id).probe(&sys, &ctx()),
                Probe::Unusable {
                    why: NEEDS_REMOTE_FDA.into()
                },
                "{id}"
            );
        }
    }

    #[test]
    fn remote_fda_comes_from_the_facts_or_from_opening_the_database() {
        let mut c = ctx();
        c.facts.remote_full_disk_access = Some(true);
        let sys = Fake::root();
        assert!(matches!(
            item("remote_full_disk_access").probe(&sys, &c),
            Probe::Ok { .. }
        ));
        assert!(sys.calls().is_empty(), "the fact answers without a read");
        let sys = Fake::root();
        sys.on_tcc(&[], denied());
        let Probe::Person { step, how } = item("remote_full_disk_access").probe(&sys, &ctx())
        else {
            panic!("a refused open is the person step");
        };
        assert_eq!(step, "remote_full_disk_access");
        assert!(
            how.contains("Allow full disk access for remote users"),
            "{how}"
        );
        let sys = Fake::root();
        sys.on_tcc(&[], Ok(Vec::new()));
        assert!(matches!(
            item("remote_full_disk_access").probe(&sys, &ctx()),
            Probe::Ok { .. }
        ));
    }

    /// `aterm ctl privacy --json` in the VM (aterm 0.104.0, no FDA yet), cut to the fields
    /// the fallback reads.
    const PRIVACY_VM: &str = r#"{"schema":1,"platform":"macos","os":"27.0","identity":{"bundle_id":"com.aterm.aterm"},"full_disk_access":"unknown","probe":"open_errno","devtools":{"state":"unknown","subject":"com.aterm.aterm","grant":"unknown","list":"unknown","list_probe":"no_full_disk_access"}}"#;

    #[test]
    fn with_the_database_closed_aterms_own_record_answers_for_its_full_disk_access() {
        let mut c = ctx();
        c.console = Some(admin());
        let record = "admin$ /Applications/aterm.app/Contents/MacOS/aterm ctl privacy --json";
        let sys = Fake::root();
        sys.on_tcc(&["com.aterm.aterm"], denied());
        sys.on(record, Ran::ok(PRIVACY_VM));
        assert!(matches!(
            item("full_disk_access:aterm").probe(&sys, &c),
            Probe::Person { .. }
        ));
        let sys = Fake::root();
        sys.on_tcc(&["com.aterm.aterm"], denied());
        sys.on(
            record,
            Ran::ok(&PRIVACY_VM.replace("no_full_disk_access", "read")),
        );
        assert!(matches!(
            item("full_disk_access:aterm").probe(&sys, &c),
            Probe::Ok { .. }
        ));
        let sys = Fake::root();
        sys.on_tcc(&["com.aterm.aterm"], denied());
        sys.on(
            record,
            Ran::ok(
                &PRIVACY_VM
                    .replace(
                        "\"bundle_id\":\"com.aterm.aterm\"",
                        "\"bundle_id\":\"com.aterm.aterm.dev\"",
                    )
                    .replace("no_full_disk_access", "read"),
            ),
        );
        assert_eq!(
            item("full_disk_access:aterm").probe(&sys, &c),
            Probe::Unusable {
                why: NEEDS_REMOTE_FDA.into()
            }
        );
        let sys = Fake::root();
        sys.on_tcc(&["com.aterm.aterm"], denied());
        assert_eq!(
            item("full_disk_access:aterm").probe(&sys, &c),
            Probe::Unusable {
                why: NEEDS_REMOTE_FDA.into()
            }
        );
    }

    #[test]
    fn a_probe_that_cannot_run_its_reader_is_unusable() {
        let sys = Fake::root();
        let probe = item("full_disk_access:aterm").probe(&sys, &ctx());
        assert!(matches!(probe, Probe::Unusable { .. }), "{probe:?}");
        let probe = item("remote_login").probe(&sys, &ctx());
        assert!(matches!(probe, Probe::Unusable { .. }), "{probe:?}");
    }

    #[test]
    fn apply_refuses_because_only_a_person_can_flip_the_switch() {
        let err = item("developer_tools:aterm")
            .apply(&Fake::root(), &ctx())
            .unwrap_err();
        assert!(err.starts_with("only a person can do this"), "{err}");
    }

    #[test]
    fn step_ids_round_trip_and_bad_ones_are_refused() {
        for id in [
            "remote_login",
            "remote_full_disk_access",
            "screen_sharing",
            "full_disk_access:aterm",
            "developer_tools:com.example.Tool",
        ] {
            assert_eq!(PersonStep::parse(id).unwrap().id(), id);
        }
        assert!(PersonStep::parse("full_disk_access").is_err());
        assert!(PersonStep::parse("remote_login:aterm").is_err());
        assert!(PersonStep::parse("full_disk_access:emacs").is_err());
        assert!(PersonStep::parse("full_disk_access:com.x');drop").is_err());
        assert!(PersonStep::parse("firewall").is_err());
    }

    #[test]
    fn the_table_expands_in_person_order_with_fda_before_developer_tools() {
        let table = Table {
            remote_login: Some(true),
            remote_full_disk_access: Some(true),
            screen_sharing: Some(false),
            full_disk_access: vec!["aterm".into()],
            developer_tools: vec!["aterm".into()],
        };
        let ids: Vec<String> = expand(&table, &ctx())
            .unwrap()
            .iter()
            .map(|i| i.id())
            .collect();
        assert_eq!(
            ids,
            [
                "person.remote_login",
                "person.remote_full_disk_access",
                "person.full_disk_access:aterm",
                "person.developer_tools:aterm",
            ]
        );
        let bad = Table {
            full_disk_access: vec!["Emacs".into()],
            ..Table::default()
        };
        assert!(expand(&bad, &ctx()).is_err());
    }
}
