// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The steps only a person can do, probed as root from the system TCC database so the
//! controller can count them (design §6).
//!
//! Every item here is probe-only: it reads, decides `Ok` or [`Probe::Person`], and its apply
//! refuses. The probe is written once ([`probe_step`]) over a "run this command" closure, so
//! the agent probes through its [`System`] port and the controller's `screen` and `person
//! --here` flows re-probe through signed `exec` requests with the same code.

use super::{Ctx, Item};
use crate::model::{Probe, Read, User};
use crate::port::{Cmd, Ran, RunFailed, System};
use serde::Deserialize;
use std::time::Duration;

/// The table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Remote Login on (always true by the time the agent runs; listed for the checklist).
    #[serde(default)]
    pub remote_login: Option<bool>,
    /// Remote Login's "Allow full disk access for remote users".
    #[serde(default)]
    pub remote_full_disk_access: Option<bool>,
    /// Screen Sharing turned on in System Settings, with its PostEvent and ScreenCapture grants.
    #[serde(default)]
    pub screen_sharing: Option<bool>,
    /// Apps that need Full Disk Access (`"aterm"`).
    #[serde(default)]
    pub full_disk_access: Vec<String>,
    /// Apps to add to Developer Tools (`"aterm"`).
    #[serde(default)]
    pub developer_tools: Vec<String>,
}

/// The system TCC database. Root over SSH can read it only with Remote Login's "Allow full
/// disk access for remote users" on (design §1.7).
pub const TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// The reader. `-readonly` so a probe can never write the database.
pub const SQLITE3: &str = "/usr/bin/sqlite3";

/// The database as the reader opens it: [`TCC_DB`] as an IMMUTABLE URI
/// (`aterm_uds::foreign_db::immutable_uri`), so sqlite3 takes no lock on tccd's live file.
/// tccd writes it in rollback-journal mode with no busy timeout, and a reader's `SHARED`
/// lock that its `COMMIT` meets fails that `COMMIT` and loses the write (measured,
/// `docs/measured/real-claude-lab-2026-10-15.md`, 2026-10-20). One command over SSH has no
/// look at the file before and after it (the guard of the local read,
/// `aterm_uds::foreign_db::read`), so a read that meets one of tccd's writes (a single row,
/// rare) can answer rows of either side of it or fail (`Unusable`); the probe is one look the
/// controller takes again, and either answer leaves the step to the person or done. A session
/// without Full Disk Access is refused it as it is the path (`... authorization denied`, exit
/// 1, measured).
#[must_use]
pub fn tcc_target() -> String {
    aterm_uds::foreign_db::immutable_uri(std::path::Path::new(TCC_DB))
}

/// launchd's control tool.
pub const LAUNCHCTL: &str = "/bin/launchctl";

/// The why of every TCC-backed step while the database is closed to this session.
pub const NEEDS_REMOTE_FDA: &str = "turn on Allow full disk access for remote users";

/// aterm's own executable, whose `ctl privacy --json` record is the fallback for aterm's Full
/// Disk Access when the TCC database is closed (design §6).
pub const ATERM_EXE: &str = "/Applications/aterm.app/Contents/MacOS/aterm";

/// Full Disk Access, through the modern Settings extension at its `Privacy_AllFiles` anchor
/// (the spelling `aterm-gui`'s privacy menu opens; the scheme has a dot).
pub const PANE_FULL_DISK_ACCESS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles";

/// The Developer Tools list at its `Privacy_DevTools` anchor (the spelling of
/// `aterm_containment::consent::DEVTOOLS_SETTINGS_URL`).
pub const PANE_DEVELOPER_TOOLS: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_DevTools";

/// Sharing ▸ Remote Login. The Sharing extension (`com.apple.Sharing-Settings.extension`,
/// legacy id `com.apple.preferences.sharing`) declares the `Services_RemoteLogin` and
/// `Services_ScreenSharing` anchors on 27. VM: opening this URL loaded the Sharing extension.
pub const PANE_REMOTE_LOGIN: &str =
    "x-apple.systempreferences:com.apple.Sharing-Settings.extension?Services_RemoteLogin";

/// Sharing ▸ Screen Sharing.
pub const PANE_SCREEN_SHARING: &str =
    "x-apple.systempreferences:com.apple.Sharing-Settings.extension?Services_ScreenSharing";

/// The launchd label of Remote Login (`/System/Library/LaunchDaemons/ssh.plist`, which ships
/// `Disabled = true`).
pub const SSHD_LABEL: &str = "com.openssh.sshd";

/// The launchd label of Screen Sharing (also shipped `Disabled = true`).
pub const SCREEN_SHARING_LABEL: &str = "com.apple.screensharing";

/// The TCC client Screen Sharing's control and view grants belong to.
pub const SCREEN_SHARING_AGENT: &str = "com.apple.screensharing.agent";

/// How long one read may take. A probe never waits for a person; this only catches a hung
/// tool.
const READ_BUDGET: Duration = Duration::from_secs(20);

/// The app names a baseline may use, and the bundle id TCC lists each under.
pub const APPS: [(&str, &str); 1] = [("aterm", "com.aterm.aterm")];

/// The five kinds of step, in the order a person does them: Remote Login and its FDA switch
/// first (they are what lets anything be read), Screen Sharing, then every app's Full Disk
/// Access before any Developer Tools entry (aterm is not launched before its FDA switch, so
/// System Settings never offers "Quit & Reopen").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Sharing ▸ Remote Login.
    RemoteLogin,
    /// Sharing ▸ Remote Login ⓘ ▸ Allow full disk access for remote users.
    RemoteFullDiskAccess,
    /// Sharing ▸ Screen Sharing (which writes the agent's PostEvent and ScreenCapture rows).
    ScreenSharing,
    /// Privacy & Security ▸ Full Disk Access, for one app.
    FullDiskAccess,
    /// Privacy & Security ▸ Developer Tools, for one app.
    DeveloperTools,
}

impl Kind {
    /// Every kind, in order.
    pub const ALL: [Self; 5] = [
        Self::RemoteLogin,
        Self::RemoteFullDiskAccess,
        Self::ScreenSharing,
        Self::FullDiskAccess,
        Self::DeveloperTools,
    ];

    /// The step id (before `:<app>` for the per-app kinds).
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

    /// Where the switch is, in words.
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

    /// The pane's `x-apple.systempreferences` URL.
    #[must_use]
    pub const fn pane(self) -> &'static str {
        match self {
            Self::RemoteLogin | Self::RemoteFullDiskAccess => PANE_REMOTE_LOGIN,
            Self::ScreenSharing => PANE_SCREEN_SHARING,
            Self::FullDiskAccess => PANE_FULL_DISK_ACCESS,
            Self::DeveloperTools => PANE_DEVELOPER_TOOLS,
        }
    }

    /// Whether the switch can only be flipped by a person at the Mac's own keyboard: Remote
    /// Login carries the connection itself, and Screen Sharing is what remote control needs.
    #[must_use]
    pub const fn at_console(self) -> bool {
        matches!(self, Self::RemoteLogin | Self::ScreenSharing)
    }

    /// Whether reading this step needs the TCC database.
    #[must_use]
    pub const fn reads_tcc(self) -> bool {
        matches!(
            self,
            Self::ScreenSharing | Self::FullDiskAccess | Self::DeveloperTools
        )
    }
}

/// One person step: its kind and, for the per-app kinds, the app.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PersonStep {
    /// What kind of switch.
    pub kind: Kind,
    /// The app as the baseline names it (`aterm`), for the per-app kinds.
    pub app: Option<String>,
    /// The bundle id TCC lists the app under, for the per-app kinds.
    pub bundle: Option<String>,
}

impl PersonStep {
    /// A step with no app.
    #[must_use]
    pub const fn of(kind: Kind) -> Self {
        Self {
            kind,
            app: None,
            bundle: None,
        }
    }

    /// A per-app step.
    ///
    /// # Errors
    /// One line when `app` is neither a known name nor a bundle id.
    pub fn for_app(kind: Kind, app: &str) -> Result<Self, String> {
        Ok(Self {
            kind,
            app: Some(app.to_owned()),
            bundle: Some(bundle_id(app)?),
        })
    }

    /// Parse a step id: `remote_login`, `remote_full_disk_access`, `screen_sharing`,
    /// `full_disk_access:<app>` or `developer_tools:<app>`.
    ///
    /// # Errors
    /// One line naming what is not a step.
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

    /// The step id, as `aterm machines screen <host> <step>` takes it.
    #[must_use]
    pub fn id(&self) -> String {
        match &self.app {
            Some(app) => format!("{}:{app}", self.kind.name()),
            None => self.kind.name().to_owned(),
        }
    }

    /// One human line for the report.
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

    /// What the person does, in words, then the pane's URL.
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

/// The bundle id TCC lists `app` under: a name from [`APPS`], or a bundle id itself.
///
/// # Errors
/// One line when `app` is neither. A bundle id is letters, digits, `.`, `-` and `_` only, so
/// it can be quoted into the TCC query as it is.
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

/// One `access` row: who, which service, and its `auth_value` (2 is allowed, 0 denied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TccRow {
    /// The bundle id or path.
    pub client: String,
    /// `kTCCService…`.
    pub service: String,
    /// `auth_value`.
    pub auth_value: i64,
}

/// The TCC query for `clients` (bundle ids; each is checked by [`bundle_id`]'s rules, so no
/// quoting is needed beyond the single quotes).
#[must_use]
pub fn tcc_query(clients: &[&str]) -> String {
    let list: Vec<String> = clients.iter().map(|c| format!("'{c}'")).collect();
    format!(
        "select client,service,auth_value from access where client in ({})",
        list.join(",")
    )
}

/// The options every TCC read passes before the database, as
/// `aterm_containment::consent`'s Developer Tools probe does: `-init /dev/null` so the
/// account's `.sqliterc` (sqlite3 finds it through the passwd home, not `$HOME`: VM, one under
/// a scratch `HOME=` did not run) cannot change the output mode or run a command, since an
/// init file runs even beside a query argument (VM: `sqlite3 -init <file> :memory: 'select
/// 1'` ran its `.print` and `.mode line`); `-safe` so nothing can touch another file;
/// `-nofollow`; and one `|`-separated line per row with no header. VM (sqlite 3.54.0): this
/// argv as root read the same three rows as a bare `-readonly`, exit 0.
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

/// The command that reads `clients`' rows.
#[must_use]
pub fn tcc_cmd(clients: &[&str]) -> Cmd {
    Cmd::new(SQLITE3)
        .args(SQLITE3_OPTIONS)
        .arg(tcc_target())
        .arg(tcc_query(clients))
        .budget(READ_BUDGET)
}

/// The command that only opens the database: Remote Login's FDA switch, measured.
#[must_use]
pub fn tcc_open_cmd() -> Cmd {
    Cmd::new(SQLITE3)
        .args(SQLITE3_OPTIONS)
        .args([tcc_target().as_str(), "select count(*) from access"])
        .budget(READ_BUDGET)
}

/// `launchctl print-disabled system`: the enabled/disabled override of each system service.
#[must_use]
pub fn print_disabled_cmd() -> Cmd {
    Cmd::new(LAUNCHCTL)
        .args(["print-disabled", "system"])
        .budget(READ_BUDGET)
}

/// Why the database did not answer: [`NEEDS_REMOTE_FDA`] when TCC refused this session
/// (`Error: unable to open database "file:…?immutable=1": authorization denied`, exit 1), else
/// what the tool said. Only `authorization denied` is that refusal: every open failure starts `unable to
/// open database`, and a missing or unreadable file (`…: unable to open database file`,
/// measured on a path that does not exist) says nothing about the switch.
fn tcc_refusal(ran: &Ran) -> String {
    let err = ran.err();
    if err.contains("authorization denied") {
        NEEDS_REMOTE_FDA.to_owned()
    } else {
        format!("the TCC database did not answer: {}", ran.describe())
    }
}

/// The rows a TCC query printed (`client|service|auth_value`, one per line).
///
/// # Errors
/// One line when a line is not such a row.
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

/// `client`'s `auth_value` for `service`: the value, `Absent` when TCC holds no row (the app
/// was never listed, which is the default: not granted).
#[must_use]
pub fn grant(rows: &[TccRow], client: &str, service: &str) -> Read<i64> {
    rows.iter()
        .find(|r| r.client == client && r.service == service)
        .map_or(Read::Absent, |r| Read::Value(r.auth_value))
}

/// Whether `label` is enabled in `launchctl print-disabled system`'s output: `Absent` when
/// the service has no override (its plist's own `Disabled` holds, which for Remote Login and
/// Screen Sharing is true). Older macOS printed `true` for disabled and `false` for enabled.
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

/// What a probe may know without reading: the facts the agent gathered, and the console
/// user for aterm's own record. The controller's re-probe passes neither, so every read is
/// fresh.
#[derive(Debug, Clone, Copy, Default)]
pub struct Known<'a> {
    /// [`crate::model::HostFacts::remote_full_disk_access`].
    pub remote_full_disk_access: Option<bool>,
    /// The account at the console.
    pub console: Option<&'a User>,
}

/// Probe `step`, running each read through `run` (the agent's port, or a signed `exec` on
/// the controller). Never waits: one read per question, each bounded.
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
/// has FDA and `read` after (design §1.8). `None` when there is no such record to read.
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
    // The console user's running aterm answers, whichever it is: a dev build's record is
    // about another bundle's grant, not this step's.
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

/// The steps `table` asks for, in [`Kind`] order.
///
/// # Errors
/// One line naming an app that is neither a known name nor a bundle id.
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

/// The items `table` asks for. A `false` asks for nothing: these items only count switches a
/// person turns on, and never turn anything off.
///
/// # Errors
/// One line naming what cannot be expanded.
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
    /// the 511 bytes after it). NEGATIVE CONTROL: the same options over the file's path meet
    /// that lock (`database is locked`): that reader held `SHARED`, the lock that fails the
    /// `COMMIT` of a writer with no busy timeout, as tccd is.
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
        // sqlite3 on a path that does not exist (measured): the open fails, but TCC did not
        // refuse anything, so the switch is unmeasured.
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
        // View-only: the control row is missing (Screen Sharing turned on by launchctl).
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
        // Never turned on: the service has no override, and TCC is not even read.
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
        // A record from another bundle (a dev build running for the console user) says
        // nothing about com.aterm.aterm's grant: the database's refusal stands.
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
        // No aterm running: the database's refusal stands.
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
