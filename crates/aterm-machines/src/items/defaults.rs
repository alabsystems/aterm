// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `defaults` reads and writes, and the raw `[[defaults]]` rows (design §5.1).

use super::{Activation, Ctx, Item};
use crate::baseline::fill_str;
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::plist::{self, Entries};
use crate::port::{Cmd, Stat, System};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The one tool every preference read and write goes through.
pub const DEFAULTS: &str = aterm_uds::os_path!("defaults");
/// Relaunches a process after its preferences changed.
pub const KILLALL: &str = aterm_uds::os_path!("killall");
/// Asks whether a user has a GUI login session (`launchctl print gui/<uid>`).
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");
/// Reads a system plist file directly when `defaults export` printed an empty domain.
pub const PLUTIL: &str = aterm_uds::os_path!("plutil");

/// Preference domains stored in an App-Sandboxed container (`~/Library/Containers/<id>/Data/Library/Preferences`), paired with app names.
pub const CONTAINED: [(&str, &str); 1] =
    [("com.apple.notificationcenterui", "Notification Center")];

fn container_of(domain: &str) -> Option<&str> {
    if let Some((_, app)) = CONTAINED
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(domain))
    {
        return Some(app);
    }
    let parts: Vec<&str> = Path::new(domain)
        .components()
        .filter_map(|part| match part {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    parts.windows(3).find_map(|at| {
        (at[0].eq_ignore_ascii_case("Library")
            && (at[1].eq_ignore_ascii_case("Containers")
                || at[1].eq_ignore_ascii_case("Group Containers")))
        .then(|| {
            CONTAINED
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(at[2]))
                .map_or(at[2], |(_, app)| *app)
        })
    })
}

/// Whether `domain` is in another app's sandbox container.
#[must_use]
pub fn contained(domain: &str) -> bool {
    container_of(domain).is_some()
}

/// Why a process with `full_disk_access` ([`crate::model::HostFacts::full_disk_access`]) leaves `domain` unread and unwritten.
#[must_use]
pub fn withheld(domain: &str, full_disk_access: Option<bool>) -> Option<String> {
    if full_disk_access == Some(true) {
        return None;
    }
    container_of(domain).map(|app| {
        format!(
            "{app} keeps {domain} in its own app container, which macOS opens only to a \
                 process whose app holds Full Disk Access (aterm, for System State or a command \
                 in an aterm tab; else the app this ran from); left untouched until that app \
                 has it"
        )
    })
}

const NO_SESSION: i32 = 112;
const WHEEL: u32 = 0;
const ADMIN: u32 = 80;

/// Where a preference lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// The user's own domain, written as that user.
    User,
    /// The user's per-host (`-currentHost`, ByHost) domain, written as that user.
    Host,
    /// `/Library/Preferences/<domain>`, written as root.
    System,
}

/// One `[[defaults]]` row.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// Preference domain or plist path without `.plist` (under `/Library/Preferences` when `system`).
    pub domain: String,
    /// The key.
    pub key: String,
    /// The wanted value.
    pub value: aterm_toml::Value,
    /// Where it lives.
    pub scope: Scope,
    /// A process to relaunch after a change (`Dock`, `Finder`, `SystemUIServer`).
    #[serde(default)]
    pub restart: Option<String>,
}

/// One preference key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The domain as `defaults` takes it.
    pub domain: String,
    /// The key.
    pub key: String,
    /// Where it lives.
    pub scope: Scope,
    /// The account a user or host preference belongs to.
    pub user: Option<User>,
}

impl Target {
    /// `user`'s own preference `domain` `key`.
    #[must_use]
    pub fn user(domain: &str, key: &str, user: &User) -> Self {
        Self {
            domain: domain.to_owned(),
            key: key.to_owned(),
            scope: Scope::User,
            user: Some(user.clone()),
        }
    }

    /// `user`'s per-host (ByHost) preference `domain` `key`.
    #[must_use]
    pub fn host(domain: &str, key: &str, user: &User) -> Self {
        Self {
            scope: Scope::Host,
            ..Self::user(domain, key, user)
        }
    }

    /// The system preference `key` of `domain`.
    #[must_use]
    pub fn system(domain: &str, key: &str) -> Self {
        let path = if domain.starts_with('/') {
            domain.to_owned()
        } else {
            format!("/Library/Preferences/{domain}")
        };
        Self {
            domain: path.strip_suffix(".plist").unwrap_or(&path).to_owned(),
            key: key.to_owned(),
            scope: Scope::System,
            user: None,
        }
    }

    fn tool(&self) -> Cmd {
        let mut cmd = Cmd::new(DEFAULTS);
        if self.scope == Scope::Host {
            cmd = cmd.arg("-currentHost");
        }
        match &self.user {
            Some(user) => cmd.as_user(user),
            None => cmd,
        }
    }

    /// `defaults [-currentHost] export <domain> -`.
    #[must_use]
    pub fn export_cmd(&self) -> Cmd {
        self.tool().args(["export", self.domain.as_str(), "-"])
    }

    /// The `defaults write` that stores `value`.
    #[must_use]
    pub fn write_cmd(&self, value: &PValue) -> Cmd {
        let cmd = self
            .tool()
            .args(["write", self.domain.as_str(), self.key.as_str()]);
        match value {
            PValue::Bool(b) => cmd.args(["-bool", if *b { "true" } else { "false" }]),
            PValue::Int(i) if *i >= 0 => cmd.arg("-int").arg(i.to_string()),
            PValue::Str(s) => cmd.arg("-string").arg(s.as_str()),
            other => cmd.arg(plist::fragment(other)),
        }
    }

    /// `defaults [-currentHost] delete <domain> <key>`.
    #[must_use]
    pub fn delete_cmd(&self) -> Cmd {
        self.tool()
            .args(["delete", self.domain.as_str(), self.key.as_str()])
    }

    /// The plist file a system preference lives in.
    #[must_use]
    pub fn plist_path(&self) -> Option<PathBuf> {
        (self.scope == Scope::System).then(|| PathBuf::from(format!("{}.plist", self.domain)))
    }

    /// Every key of the target's domain.
    ///
    /// # Errors
    /// One line when the domain cannot be read, or an empty answer cannot be trusted.
    pub fn entries(&self, sys: &dyn System) -> Result<Entries, String> {
        let file = match self.plist_path() {
            None => None,
            Some(path) => Some((system_file(sys, &path)?, path)),
        };
        let export = self.export_cmd();
        let entries = read_domain(sys, &export)?;
        if entries.is_empty()
            && let Some((Some(_), path)) = &file
        {
            confirm_empty(sys, path, &export)?;
        }
        Ok(entries)
    }

    /// What the host holds under the key.
    pub fn read(&self, sys: &dyn System) -> Read<PValue> {
        self.read_in(&self.entries(sys))
    }

    /// What `entries`, a reading of the key's domain ([`Target::entries`]), holds under the key.
    #[must_use]
    pub fn read_in(&self, entries: &Result<Entries, String>) -> Read<PValue> {
        match entries {
            Err(why) => Read::Unusable(why.clone()),
            Ok(entries) => match entries.iter().find(|(k, _)| *k == self.key) {
                None => Read::Absent,
                Some((_, Ok(v))) => Read::Value(v.clone()),
                Some((_, Err(why))) => Read::Unusable(format!("{}: {why}", self.describe())),
            },
        }
    }

    /// `domain key`, with `-currentHost` for a ByHost key, for titles.
    #[must_use]
    pub fn describe(&self) -> String {
        let host = if self.scope == Scope::Host {
            "-currentHost "
        } else {
            ""
        };
        format!("{host}{} {}", self.domain, self.key)
    }
}

/// One reading of several keys: each domain read once per scope and account ([`Target::entries`])
/// and each key answered from that, as [`Target::read`] answers it. A reading of one moment: a
/// later reading (a read-back, a check that nothing changed since a snapshot) takes a new one.
#[derive(Debug, Default)]
pub struct Exports(Vec<(Target, Result<Entries, String>)>);

impl Exports {
    /// What the host holds under `target`'s key, its domain read at the first key asked of it.
    pub fn read(&mut self, sys: &dyn System, target: &Target) -> Read<PValue> {
        let at = self
            .0
            .iter()
            .position(|(seen, _)| {
                seen.domain == target.domain
                    && seen.scope == target.scope
                    && seen.user == target.user
            })
            .unwrap_or_else(|| {
                self.0.push((target.clone(), target.entries(sys)));
                self.0.len() - 1
            });
        target.read_in(&self.0[at].1)
    }
}

/// The entries of the dictionary `export` (a `defaults … export <domain> -`) prints.
///
/// # Errors
/// One line when the export did not run, failed, or printed something other than a dictionary.
pub fn read_domain(sys: &dyn System, export: &Cmd) -> Result<Entries, String> {
    let ran = sys
        .run(export)
        .map_err(|e| format!("{}: {e}", export.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", export.display(), ran.describe()));
    }
    plist::parse_domain(&ran.stdout).map_err(|why| format!("{}: {why}", export.display()))
}

/// Refuse a system plist `path` a root read or write could be redirected from.
///
/// # Errors
/// One line naming the directory, or the path, that fails.
pub fn check_system_path(sys: &dyn System, path: &Path) -> Result<(), String> {
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(format!(
            "{} is not an absolute path without `..`",
            path.display()
        ));
    }
    for dir in path.ancestors().skip(1) {
        let s = match sys.stat(dir) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{} cannot be checked: {e}", dir.display())),
        };
        let why = if s.symlink {
            "is a symbolic link (name the directory it points to)".to_owned()
        } else if !s.dir {
            "is not a directory".to_owned()
        } else if s.uid != 0 {
            format!("is owned by uid {}, not root", s.uid)
        } else if s.mode & 0o002 != 0 {
            "is writable by everyone".to_owned()
        } else if s.mode & 0o020 != 0 && s.gid != WHEEL && s.gid != ADMIN {
            format!("is writable by group {}", s.gid)
        } else {
            continue;
        };
        return Err(format!(
            "{} {why}, so a root write below it could be redirected",
            dir.display()
        ));
    }
    Ok(())
}

/// Check a system plist `path` before root reads or writes it.
///
/// # Errors
/// One line saying what fails.
pub fn system_file(sys: &dyn System, path: &Path) -> Result<Option<Stat>, String> {
    check_system_path(sys, path)?;
    match sys.stat(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{} cannot be checked: {e}", path.display())),
        Ok(s) if s.symlink || s.dir => Err(format!("{} is not a regular file", path.display())),
        Ok(s) => Ok(Some(s)),
    }
}

/// Confirm an empty export of `path` using `plutil`.
///
/// # Errors
/// One line when the file is unreadable or not empty.
fn confirm_empty(sys: &dyn System, path: &Path, export: &Cmd) -> Result<(), String> {
    let read = Cmd::new(PLUTIL)
        .args(["-convert", "xml1", "-o", "-"])
        .arg(path.as_os_str());
    let ran = sys
        .run(&read)
        .map_err(|e| format!("{}: {e}", read.display()))?;
    if !ran.success() {
        return Err(format!(
            "{} printed an empty domain, but {} exists: {}",
            export.display(),
            path.display(),
            ran.describe()
        ));
    }
    match plist::parse_domain(&ran.stdout) {
        Ok(entries) if entries.is_empty() => Ok(()),
        Ok(entries) => Err(format!(
            "{} printed an empty domain, but {} holds {} keys",
            export.display(),
            path.display(),
            entries.len()
        )),
        Err(why) => Err(format!("{}: {why}", read.display())),
    }
}

/// What a read renders as in a report.
#[must_use]
pub fn render(read: &Read<PValue>) -> String {
    match read {
        Read::Value(v) => v.to_string(),
        Read::Absent => "absent".into(),
        Read::Unusable(why) => format!("unknown ({why})"),
    }
}

/// What a preference should hold.
#[derive(Debug, Clone, PartialEq)]
pub enum Want {
    /// This value (compared with [`PValue::same`]).
    Value(PValue),
    /// Nothing: the key is deleted, so the OS default applies.
    Unset,
}

impl Want {
    /// Whether `read` already satisfies this.
    ///
    /// # Errors
    /// The read's reason when it was [`Read::Unusable`].
    pub fn satisfied_by(&self, read: &Read<PValue>) -> Result<bool, String> {
        match (self, read) {
            (_, Read::Unusable(why)) => Err(why.clone()),
            (Self::Value(want), Read::Value(have)) => Ok(have.same(want)),
            (Self::Unset, Read::Absent) => Ok(true),
            (Self::Value(_), Read::Absent) | (Self::Unset, Read::Value(_)) => Ok(false),
        }
    }
}

impl std::fmt::Display for Want {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(v) => write!(f, "{v}"),
            Self::Unset => f.write_str("absent"),
        }
    }
}

/// One preference item.
#[derive(Debug)]
pub struct Pref {
    family: &'static str,
    id: String,
    title: String,
    /// The key.
    pub target: Target,
    /// What it should hold.
    pub want: Want,
    /// A process in the user's session to relaunch after a change (`Dock`, `sharingd`).
    pub restart: Option<String>,
    /// When the change is felt.
    pub effect: Effect,
    /// Where this row was verified (macOS version and source), for compiled rows.
    pub provenance: String,
    /// What the probe that planned the change read: the value the revert line restores.
    before: Mutex<Option<Read<PValue>>>,
    /// The relaunch of [`Pref::restart`] that makes the change felt: the row's own, or the
    /// run's one for its account and process ([`Pref::shared`]).
    relaunch: Option<Arc<Relaunch>>,
}

impl Pref {
    /// A preference item.
    #[must_use]
    pub fn new(
        family: &'static str,
        id: String,
        title: String,
        target: Target,
        want: Want,
    ) -> Self {
        let effect = if target.scope == Scope::System {
            Effect::Now
        } else {
            Effect::Login
        };
        Self {
            family,
            id,
            title,
            target,
            want,
            restart: None,
            effect,
            provenance: String::new(),
            before: Mutex::new(None),
            relaunch: None,
        }
    }

    /// Relaunch `process` in the user's session after a change, once the engine applied
    /// it ([`Relaunch`]): this row's own relaunch, until [`Pref::shared`] makes it the
    /// run's one.
    #[must_use]
    pub fn restart(mut self, process: impl Into<String>) -> Self {
        let process = process.into();
        self.relaunch = self.target.user.as_ref().map(|user| {
            Arc::new(Relaunch::new(
                format!("{}.relaunch", self.id),
                user,
                &process,
            ))
        });
        self.restart = Some(process);
        self.effect = Effect::Relaunch;
        self
    }

    /// Relaunch this row's process with every other row of the run whose writes it reads:
    /// `relaunches`' one for the row's account and process, so a run that changes several
    /// of them relaunches it once, after the last.
    #[must_use]
    pub fn shared(mut self, relaunches: &Relaunches) -> Self {
        if let (Some(process), Some(user)) = (&self.restart, &self.target.user) {
            self.relaunch = Some(relaunches.of(user, process));
        }
        self
    }

    /// When the change is felt.
    #[must_use]
    pub const fn felt(mut self, effect: Effect) -> Self {
        self.effect = effect;
        self
    }

    /// Where the row was verified.
    #[must_use]
    pub fn provenance(mut self, provenance: impl Into<String>) -> Self {
        self.provenance = provenance.into();
        self
    }

    fn deferred(&self, ctx: &Ctx) -> Option<String> {
        disrupts(self.restart.as_deref()?, self.target.user.as_ref()?, ctx)
    }
}

/// Why a change that relaunches `user`'s `process` must wait: `user` is at the console
/// while the Mac is in use (design §5.3).
#[must_use]
pub fn disrupts(process: &str, user: &User, ctx: &Ctx) -> Option<String> {
    if ctx.disrupt {
        return None;
    }
    let at_console = ctx.console.as_ref().is_some_and(|c| c.name == user.name)
        || ctx.facts.console_user.as_deref() == Some(user.name.as_str());
    at_console.then(|| {
        format!(
            "relaunches {process} for {}, who is at the console; waits for the Mac to be idle (or --now)",
            user.name
        )
    })
}

/// The one relaunch of a process in a user's session that makes felt the writes of every
/// row it activates (docs/DESIGN-init-system-2026-10-20.md §3.3): the engine applies each
/// of those rows, runs it once after the last, and reads them back only once the process is
/// up again and what it reads of their keys is still
/// ([`crate::init_activation::relaunch_reading`]). A relaunch after each row's write would
/// relaunch it once per row and read each row back while the process was still starting.
#[derive(Debug)]
pub struct Relaunch {
    id: String,
    user: User,
    process: String,
    /// The rows that wrote in this run, each by its id and key, in run order.
    owed: Mutex<Vec<(String, Target)>>,
}

impl Relaunch {
    fn new(id: String, user: &User, process: &str) -> Self {
        Self {
            id,
            user: user.clone(),
            process: process.to_owned(),
            owed: Mutex::new(Vec::new()),
        }
    }

    /// The row `id` wrote `target`: this run relaunches the process for it.
    pub fn owe(&self, id: &str, target: &Target) {
        let mut owed = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
        if !owed.iter().any(|(seen, _)| seen == id) {
            owed.push((id.to_owned(), target.clone()));
        }
    }
}

impl Activation for Relaunch {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn pending(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<Option<(String, String)>, String> {
        let owed = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
        if owed.is_empty() {
            return Ok(None);
        }
        let rows: Vec<&str> = owed.iter().map(|(id, _)| id.as_str()).collect();
        Ok(Some((
            format!(
                "{} running with the preferences it read at its start",
                self.process
            ),
            format!(
                "{} relaunched once for {}, after {} wrote, and up again",
                self.process,
                self.user.name,
                rows.join(", ")
            ),
        )))
    }

    fn activate(&self, sys: &dyn System, ctx: &Ctx) -> Result<Vec<(String, String)>, String> {
        let owed = std::mem::take(&mut *self.owed.lock().unwrap_or_else(PoisonError::into_inner));
        let keys: Vec<Target> = owed.iter().map(|(_, target)| target.clone()).collect();
        let session = Session::of(&self.user, ctx);
        Ok(
            match crate::init_activation::relaunch_reading(
                sys,
                &self.user,
                session,
                &self.process,
                &keys,
            ) {
                Ok(()) => Vec::new(),
                Err(why) => owed.into_iter().map(|(id, _)| (id, why.clone())).collect(),
            },
        )
    }
}

/// The relaunches of one run, one per account and process ([`Pref::shared`]).
#[derive(Debug, Default)]
pub struct Relaunches(Mutex<Vec<Arc<Relaunch>>>);

impl Relaunches {
    /// The run's relaunch of `user`'s `process`.
    #[must_use]
    pub fn of(&self, user: &User, process: &str) -> Arc<Relaunch> {
        let mut all = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = all
            .iter()
            .find(|relaunch| relaunch.user.name == user.name && relaunch.process == process)
        {
            return Arc::clone(found);
        }
        let relaunch = Arc::new(Relaunch::new(
            format!("relaunch.{process}@{}", user.name),
            user,
            process,
        ));
        all.push(Arc::clone(&relaunch));
        relaunch
    }
}

impl Item for Pref {
    fn preference_changes(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
    ) -> Result<Vec<(Target, Read<PValue>)>, String> {
        let read = self.target.read(sys);
        Ok(if self.want.satisfied_by(&read)? {
            Vec::new()
        } else {
            vec![(self.target.clone(), read)]
        })
    }
    fn id(&self) -> String {
        self.id.clone()
    }

    fn family(&self) -> &'static str {
        self.family
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let read = self.target.read(sys);
        match self.want.satisfied_by(&read) {
            Err(why) => Probe::Unusable { why },
            Ok(true) => Probe::Ok { now: render(&read) },
            Ok(false) => {
                let from = render(&read);
                *self.before.lock().unwrap_or_else(PoisonError::into_inner) = Some(read);
                match self.deferred(ctx) {
                    Some(why) => Probe::Deferred { why },
                    None => Probe::Change {
                        from,
                        to: self.want.to_string(),
                    },
                }
            }
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = match &self.want {
            Want::Value(v) => self.target.write_cmd(v),
            Want::Unset => self.target.delete_cmd(),
        };
        let kept = match self.target.plist_path() {
            None => None,
            Some(path) => system_file(sys, &path)
                .map_err(|why| format!("not written: {why}"))?
                .map(|s| (path, s)),
        };
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        let gone = self.want == Want::Unset
            && (ran.err().contains("Could not find key") || ran.err().contains("not found"));
        if !ran.success() && !gone {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        if let Some((path, was)) = kept {
            keep_owner_and_mode(sys, &path, was)?;
        }
        if let Some(relaunch) = &self.relaunch {
            relaunch.owe(&self.id, &self.target);
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        let before = self
            .before
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        revert_line(&self.target, &before, self.restart.as_deref())
    }

    fn effect(&self) -> Effect {
        self.effect
    }

    fn activation(&self) -> Option<Arc<dyn Activation>> {
        self.relaunch
            .as_ref()
            .map(|relaunch| Arc::clone(relaunch) as Arc<dyn Activation>)
    }
}

/// The command that puts `target` back to `before`, saying which kind of revert it is.
#[must_use]
pub fn revert_line(
    target: &Target,
    before: &Read<PValue>,
    restart: Option<&str>,
) -> Option<String> {
    let relaunch = restart
        .map(|p| format!("; then relaunch {p}"))
        .unwrap_or_default();
    match before {
        Read::Value(v) => Some(format!(
            "{}  # restores the value before this run{relaunch}",
            target.write_cmd(v).display()
        )),
        Read::Absent => Some(format!(
            "{}  # it was not set before this run{relaunch}",
            target.delete_cmd().display()
        )),
        Read::Unusable(_) => None,
    }
}

fn keep_owner_and_mode(sys: &dyn System, path: &Path, was: Stat) -> Result<(), String> {
    let mode = was.mode & 0o777;
    match sys.set_owner_and_mode(path, was.uid, was.gid, mode) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "written, but restoring {}'s owner {}:{} and mode {mode:o} failed: {e}",
            path.display(),
            was.uid,
            was.gid
        )),
        Ok(()) => Ok(()),
    }
}

/// What [`relaunch`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signalled {
    /// `killall` signalled every one of `user`'s processes of that name.
    Every,
    /// `killall` exited 1: it matched none, or one of them refused the signal. Whether the
    /// old processes are gone is the caller's to see, never this answer's to vouch for.
    NotEvery,
    /// `user` has no login session: nothing was signalled, and nothing would start again.
    NoSession,
}

/// Whether a user's login session is known before a relaunch ([`relaunch`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    /// The user owns this Mac's console in the run's context ([`Ctx::console`]): their login
    /// session runs, and nothing asks.
    AtConsole,
    /// `launchctl print gui/<uid>` says whether there is one.
    Ask,
}

impl Session {
    /// [`Session::AtConsole`] when `ctx`'s console account is `user`, else [`Session::Ask`].
    #[must_use]
    pub fn of(user: &User, ctx: &Ctx) -> Self {
        if ctx
            .console
            .as_ref()
            .is_some_and(|console| console.uid == user.uid)
        {
            Self::AtConsole
        } else {
            Self::Ask
        }
    }
}

/// Relaunch `process` in `user`'s login session, if there is one (`session`), saying what was
/// signalled.
///
/// # Errors
/// One line when the session check or the relaunch did not work.
pub fn relaunch(
    sys: &dyn System,
    user: &User,
    session: Session,
    process: &str,
) -> Result<Signalled, String> {
    if session == Session::Ask {
        let ask = Cmd::new(LAUNCHCTL)
            .arg("print")
            .arg(format!("gui/{}", user.uid));
        match sys.run(&ask) {
            Ok(ran) if ran.success() => {}
            Ok(ran) if ran.status == Some(NO_SESSION) => return Ok(Signalled::NoSession),
            Ok(ran) => return Err(format!("{}: {}", ask.display(), ran.describe())),
            Err(e) => return Err(format!("{}: {e}", ask.display())),
        }
    }
    let kill = Cmd::new(KILLALL)
        .args(["-u", user.name.as_str(), process])
        .as_user(user)
        .in_session();
    match sys.run(&kill) {
        Ok(ran) if ran.success() => Ok(Signalled::Every),
        Ok(ran) if ran.status == Some(1) => Ok(Signalled::NotEvery),
        Ok(ran) => Err(format!("{}: {}", kill.display(), ran.describe())),
        Err(e) => Err(format!("{}: {e}", kill.display())),
    }
}

/// An item whose precondition is missing, so it only ever probes [`Probe::Blocked`].
#[derive(Debug, Clone)]
pub struct Unmet {
    /// The family.
    pub family: &'static str,
    /// The id the item would have had.
    pub id: String,
    /// One human line.
    pub title: String,
    /// The missing precondition.
    pub why: String,
}

impl Item for Unmet {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn family(&self) -> &'static str {
        self.family
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn probe(&self, _sys: &dyn System, _ctx: &Ctx) -> Probe {
        Probe::Blocked {
            why: self.why.clone(),
        }
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err(self.why.clone())
    }
}

/// Convert a TOML value to a property-list value with `{key}` placeholders filled from `vars`.
///
/// # Errors
/// One line for a date-time a plist `<date>` cannot hold (anything but UTC to the second).
pub fn to_pvalue(value: &aterm_toml::Value, vars: &[(&str, &str)]) -> Result<PValue, String> {
    Ok(match value {
        aterm_toml::Value::String(s) => PValue::Str(fill_str(s, vars)),
        aterm_toml::Value::Integer(i) => PValue::Int(*i),
        aterm_toml::Value::Float(f) => PValue::Real(*f),
        aterm_toml::Value::Boolean(b) => PValue::Bool(*b),
        aterm_toml::Value::Datetime(dt) => {
            let utc = matches!(dt.offset, Some(aterm_toml::Offset::Z));
            let whole = dt.time.is_some_and(|t| t.nanosecond == 0);
            if !(utc && dt.date.is_some() && whole) {
                return Err(format!(
                    "{dt}: a plist date is UTC to the second, like 2026-10-16T22:40:34Z"
                ));
            }
            PValue::Date(dt.to_string())
        }
        aterm_toml::Value::Array(items) => PValue::Array(
            items
                .iter()
                .map(|v| to_pvalue(v, vars))
                .collect::<Result<_, _>>()?,
        ),
        aterm_toml::Value::Table(table) => PValue::Dict(
            table
                .iter()
                .map(|(k, v)| Ok((fill_str(k, vars), to_pvalue(v, vars)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

fn plain_process_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// The items `rows` ask for, each relaunch its rows share their own.
///
/// # Errors
/// One line naming the row that cannot be expanded.
pub fn expand_rows(rows: &[Row], ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    expand_rows_with(rows, ctx, &Relaunches::default())
}

/// The items `rows` ask for, each process a row restarts relaunched once per account with
/// every other row of the run in `relaunches` that restarts it ([`Pref::shared`]).
///
/// # Errors
/// One line naming the row that cannot be expanded.
pub fn expand_rows_with(
    rows: &[Row],
    ctx: &Ctx,
    relaunches: &Relaunches,
) -> Result<Vec<Box<dyn Item>>, String> {
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for (n, row) in rows.iter().enumerate() {
        let at = format!("[[defaults]] row {} ({} {})", n + 1, row.domain, row.key);
        if row.domain.is_empty() || row.key.is_empty() {
            return Err(format!("{at}: needs a domain and a key"));
        }
        if let Some(p) = &row.restart
            && !plain_process_name(p)
        {
            return Err(format!(
                "{at}: restart = {p:?} is not a process name (Dock, Finder, SystemUIServer)"
            ));
        }
        match row.scope {
            Scope::System => {
                if row.restart.is_some() {
                    return Err(format!(
                        "{at}: restart names a process in a user's session, and a system row has no user"
                    ));
                }
                if Path::new(&row.domain)
                    .components()
                    .any(|c| c == Component::ParentDir)
                {
                    return Err(format!(
                        "{at}: a system domain is a name under /Library/Preferences or an absolute path, without `..`"
                    ));
                }
                let want =
                    Want::Value(to_pvalue(&row.value, &[]).map_err(|e| format!("{at}: {e}"))?);
                let target = Target::system(&row.domain, &row.key);
                let title = format!("{} = {want} (system)", target.describe());
                let id = format!("defaults.system.{}.{}", row.domain, row.key);
                if let Some(why) = withheld(&target.domain, ctx.facts.full_disk_access) {
                    items.push(Box::new(Unmet {
                        family: "defaults",
                        id,
                        title,
                        why,
                    }));
                    continue;
                }
                items.push(Box::new(Pref::new("defaults", id, title, target, want)));
            }
            Scope::User | Scope::Host => {
                for user in &ctx.users {
                    let vars = [("user", user.name.as_str())];
                    let domain = fill_str(&row.domain, &vars);
                    let key = fill_str(&row.key, &vars);
                    if Path::new(&domain)
                        .components()
                        .any(|c| c == Component::ParentDir)
                    {
                        return Err(format!(
                            "{at}: a user or host domain is a name or a path, without `..`"
                        ));
                    }
                    if row.scope == Scope::Host && domain.contains('/') {
                        return Err(format!(
                            "{at}: a host (-currentHost) domain is a name, not a path"
                        ));
                    }
                    let want = Want::Value(
                        to_pvalue(&row.value, &vars).map_err(|e| format!("{at}: {e}"))?,
                    );
                    let (target, byhost) = if row.scope == Scope::Host {
                        (Target::host(&domain, &key, user), "byhost.")
                    } else {
                        (Target::user(&domain, &key, user), "")
                    };
                    let title = format!("{} = {want} for {}", target.describe(), user.name);
                    let id = format!("defaults.{byhost}{domain}.{key}@{}", user.name);
                    if let Some(why) = withheld(&domain, ctx.facts.full_disk_access) {
                        items.push(Box::new(Unmet {
                            family: "defaults",
                            id,
                            title,
                            why,
                        }));
                        continue;
                    }
                    let mut pref = Pref::new("defaults", id, title, target, want);
                    if let Some(p) = &row.restart {
                        pref = pref.restart(p.as_str()).shared(relaunches);
                    }
                    items.push(Box::new(pref));
                }
            }
        }
    }
    Ok(items)
}

#[cfg(test)]
#[path = "defaults_tests.rs"]
pub(crate) mod tests;
