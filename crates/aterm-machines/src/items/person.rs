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

/// Path to `/usr/bin/sqlite3`.
pub const SQLITE3: &str = "/usr/bin/sqlite3";

/// Formats [`TCC_DB`] as an immutable `file:` URI for lock-free SQLite reads.
#[must_use]
pub fn tcc_target() -> String {
    aterm_uds::foreign_db::immutable_uri(std::path::Path::new(TCC_DB))
}

/// Path to `/bin/launchctl`.
pub const LAUNCHCTL: &str = "/bin/launchctl";

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
/// Returns an error if `app` is neither a known short name nor a valid bundle ID.
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

/// Builds the SQL query reading `access` rows for `clients`.
#[must_use]
pub fn tcc_query(clients: &[&str]) -> String {
    let list: Vec<String> = clients.iter().map(|c| format!("'{c}'")).collect();
    format!(
        "select client,service,auth_value from access where client in ({})",
        list.join(",")
    )
}

/// Read-only `sqlite3` CLI flags used for every TCC probe.
pub const SQLITE3_OPTIONS: [&str; 11] = [
    "-init",
    "/dev/null",
    "-readonly",
    "-safe",
    "-nofollow",
    "-batch",
    "-bail",
    "-noheader",
    "-list",
    "-separator",
    "|",
];

/// Builds the `sqlite3` [`Cmd`] querying `clients` from [`TCC_DB`].
#[must_use]
pub fn tcc_cmd(clients: &[&str]) -> Cmd {
    Cmd::new(SQLITE3)
        .args(SQLITE3_OPTIONS)
        .arg(tcc_target())
        .arg(tcc_query(clients))
        .budget(READ_BUDGET)
}

/// Builds a `sqlite3` [`Cmd`] that probes read access to [`TCC_DB`].
#[must_use]
pub fn tcc_open_cmd() -> Cmd {
    Cmd::new(SQLITE3)
        .args(SQLITE3_OPTIONS)
        .args([tcc_target().as_str(), "select count(*) from access"])
        .budget(READ_BUDGET)
}

/// Builds the `launchctl print-disabled system` [`Cmd`].
#[must_use]
pub fn print_disabled_cmd() -> Cmd {
    Cmd::new(LAUNCHCTL)
        .args(["print-disabled", "system"])
        .budget(READ_BUDGET)
}

/// Why the database did not answer: [`NEEDS_REMOTE_FDA`] when TCC refused this session
/// (`Error: unable to open database "file:…?immutable=1": authorization denied`, exit 1), else
/// what the tool said.
fn tcc_refusal(ran: &Ran) -> String {
    let err = ran.err();
    if err.contains("authorization denied") {
        NEEDS_REMOTE_FDA.to_owned()
    } else {
        format!("the TCC database did not answer: {}", ran.describe())
    }
}

/// Parses `client|service|auth_value` lines from `sqlite3` output.
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

/// The rows for `clients`, or why they could not be read.
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

/// Probes `step` using `run` to execute system commands.
pub fn probe_step(
    step: &PersonStep,
    known: Known<'_>,
    run: &mut dyn FnMut(&Cmd) -> Result<Ran, RunFailed>,
) -> Probe {
    let person = || Probe::Person {
        step: step.id(),
        how: step.how(),
    };
    match step.kind {
        Kind::RemoteLogin => match service_enabled(SSHD_LABEL, run) {
            Read::Value(true) => Probe::Ok { now: "on".into() },
            Read::Value(false) | Read::Absent => person(),
            Read::Unusable(why) => Probe::Unusable { why },
        },
        Kind::RemoteFullDiskAccess => match known.remote_full_disk_access {
            Some(true) => Probe::Ok { now: "on".into() },
            Some(false) => person(),
            None => match run(&tcc_open_cmd()) {
                Ok(ran) if ran.success() => Probe::Ok { now: "on".into() },
                Ok(ran) => match tcc_refusal(&ran) {
                    why if why == NEEDS_REMOTE_FDA => person(),
                    why => Probe::Unusable { why },
                },
                Err(e) => Probe::Unusable {
                    why: format!("the TCC database did not answer: {e}"),
                },
            },
        },
        Kind::ScreenSharing => {
            match service_enabled(SCREEN_SHARING_LABEL, run) {
                Read::Value(true) => {}
                Read::Value(false) | Read::Absent => return person(),
                Read::Unusable(why) => return Probe::Unusable { why },
            }
            let rows = match tcc_rows(&[SCREEN_SHARING_AGENT], run) {
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
            match tcc_rows(&[bundle], run) {
                Ok(rows) => match grant(&rows, bundle, service) {
                    Read::Value(2) => Probe::Ok {
                        now: "granted".into(),
                    },
                    Read::Value(_) | Read::Absent => person(),
                    Read::Unusable(why) => Probe::Unusable { why },
                },
                Err(why) if why == NEEDS_REMOTE_FDA && step.kind == Kind::FullDiskAccess => {
                    aterm_record(step, known.console, run).unwrap_or(Probe::Unusable { why })
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
        probe_step(&self.step, known, &mut |cmd| sys.run(cmd))
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

    /// sqlite3's refusal from a session without Full Disk Access (measured on a Mac whose
    /// terminal lacks it), exit 1.
    const TCC_DENIED: &str = "Error: unable to open database \"file:/Library/Application%20Support/com.apple.TCC/TCC.db?immutable=1\": authorization denied\n";

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

    fn key(clients: &[&str]) -> String {
        tcc_cmd(clients).display()
    }

    fn item(id: &str) -> PersonItem {
        PersonItem {
            step: PersonStep::parse(id).unwrap(),
        }
    }

    #[test]
    fn the_tcc_query_is_one_readonly_sqlite3_run_naming_only_the_wanted_clients() {
        assert_eq!(
            key(&["com.aterm.aterm"]),
            "/usr/bin/sqlite3 -init /dev/null -readonly -safe -nofollow -batch -bail -noheader \
             -list -separator '|' \
             'file:/Library/Application%20Support/com.apple.TCC/TCC.db?immutable=1' \
             'select client,service,auth_value from access where client in ('\\''com.aterm.aterm'\\'')'"
        );
        assert_eq!(
            tcc_open_cmd().display(),
            "/usr/bin/sqlite3 -init /dev/null -readonly -safe -nofollow -batch -bail -noheader \
             -list -separator '|' \
             'file:/Library/Application%20Support/com.apple.TCC/TCC.db?immutable=1' \
             'select count(*) from access'"
        );
    }

    /// THE READ TAKES NO LOCK ON tccd's FILE (macOS: Apple's `/usr/bin/sqlite3`): the
    /// options every TCC read passes, over a scratch database's immutable URI, answer its rows
    /// while the lock a committing writer holds is taken (`EXCLUSIVE`: `PENDING_BYTE` and
    /// the 511 bytes after it).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_tcc_read_takes_no_lock_a_writer_would_meet() {
        use std::os::fd::AsRawFd as _;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("TCC.db");
        let sqlite = |target: &str, sql: &str| {
            aterm_uds::spawnfd::command(SQLITE3)
                .args(SQLITE3_OPTIONS)
                .arg(target)
                .arg(sql)
                .output()
                .expect("sqlite3")
        };
        let made = aterm_uds::spawnfd::command(SQLITE3)
            .arg(&db)
            .arg(
                "CREATE TABLE access (service TEXT, client TEXT, auth_value INTEGER); \
                 INSERT INTO access VALUES ('kTCCServiceDeveloperTool', 'com.aterm.aterm', 2);",
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
        let query = tcc_query(&["com.aterm.aterm"]);
        let control = sqlite(&db.to_string_lossy(), &query);
        assert!(
            String::from_utf8_lossy(&control.stderr).contains("database is locked"),
            "{control:?}"
        );
        let read = sqlite(&aterm_uds::foreign_db::immutable_uri(&db), &query);
        assert!(read.status.success(), "{read:?}");
        assert_eq!(
            parse_tcc(&String::from_utf8_lossy(&read.stdout)),
            Ok(vec![TccRow {
                client: "com.aterm.aterm".into(),
                service: "kTCCServiceDeveloperTool".into(),
                auth_value: 2,
            }])
        );
        drop(held);
    }

    #[test]
    fn a_database_that_fails_to_open_for_another_reason_is_unusable_not_the_fda_switch() {
        let gone = Ran::exit(
            1,
            "Error: unable to open database \"file:/Library/Application%20Support/com.apple.TCC/TCC.db?immutable=1\": unable to open database file\n",
        );
        let sys = Fake::root();
        sys.on(&tcc_open_cmd().display(), gone.clone());
        assert!(matches!(
            item("remote_full_disk_access").probe(&sys, &ctx()),
            Probe::Unusable { .. }
        ));
        let sys = Fake::root();
        sys.on(&key(&["com.aterm.aterm"]), gone);
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
        sys.on(&key(&[SCREEN_SHARING_AGENT]), Ran::ok(TCC_VM));
        assert_eq!(
            item("screen_sharing").probe(&sys, &ctx()),
            Probe::Ok {
                now: "on, with control".into()
            }
        );
        let sys = Fake::root();
        sys.on(&print_disabled_cmd().display(), Ran::ok(PRINT_DISABLED_VM));
        sys.on(
            &key(&[SCREEN_SHARING_AGENT]),
            Ran::ok("com.apple.screensharing.agent|kTCCServiceScreenCapture|2\n"),
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
        sys.on(&key(&["com.aterm.aterm"]), Ran::ok(TCC_VM));
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
        sys.on(
            &key(&["com.aterm.aterm"]),
            Ran::ok("com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n"),
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
        sys.on(
            &key(&["com.aterm.aterm"]),
            Ran::ok("com.aterm.aterm|kTCCServiceSystemPolicyAllFiles|2\n"),
        );
        let Probe::Person { step, how } = item("developer_tools:aterm").probe(&sys, &ctx()) else {
            panic!("aterm is not in Developer Tools");
        };
        assert_eq!(step, "developer_tools:aterm");
        assert!(how.ends_with(PANE_DEVELOPER_TOOLS), "{how}");
        let sys = Fake::root();
        sys.on(
            &key(&["com.aterm.aterm"]),
            Ran::ok("com.aterm.aterm|kTCCServiceDeveloperTool|2\n"),
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
            sys.on(&key(&[SCREEN_SHARING_AGENT]), Ran::exit(1, TCC_DENIED));
            sys.on(&key(&["com.aterm.aterm"]), Ran::exit(1, TCC_DENIED));
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
        sys.on(&tcc_open_cmd().display(), Ran::exit(1, TCC_DENIED));
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
        sys.on(&tcc_open_cmd().display(), Ran::ok("5\n"));
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
        sys.on(&key(&["com.aterm.aterm"]), Ran::exit(1, TCC_DENIED));
        sys.on(record, Ran::ok(PRIVACY_VM));
        assert!(matches!(
            item("full_disk_access:aterm").probe(&sys, &c),
            Probe::Person { .. }
        ));
        let sys = Fake::root();
        sys.on(&key(&["com.aterm.aterm"]), Ran::exit(1, TCC_DENIED));
        sys.on(
            record,
            Ran::ok(&PRIVACY_VM.replace("no_full_disk_access", "read")),
        );
        assert!(matches!(
            item("full_disk_access:aterm").probe(&sys, &c),
            Probe::Ok { .. }
        ));
        let sys = Fake::root();
        sys.on(&key(&["com.aterm.aterm"]), Ran::exit(1, TCC_DENIED));
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
        sys.on(&key(&["com.aterm.aterm"]), Ran::exit(1, TCC_DENIED));
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
