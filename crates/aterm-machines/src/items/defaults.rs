// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `defaults` reads and writes, and the raw `[[defaults]]` rows (design §5.1).
//!
//! [`Pref`] is the one preference item the families built on `defaults` share (the
//! consumer catalog, Setup Assistant, Software Update and the raw rows). Its read is
//! `defaults [-currentHost] export <domain> -`, run as the preference's user (design
//! §3.5), or as root for a system domain, and parsed by [`crate::plist::parse_domain`].
//! A failed or unparsable export reads [`Read::Unusable`] and is never taken for the
//! default, and so does a key whose value this crate cannot hold.
//!
//! On macOS 27 `defaults export` prints an empty `<dict/>` and exits 0 both for a domain
//! that does not exist and for one the reader may not read (measured in the 27.0 VM: the
//! admin's export of the root-only `/Library/Preferences/com.apple.apsd` prints `<dict/>`;
//! `plutil` on the same file says "you don't have permission to view it"). A missing key
//! in a domain that printed keys reads [`Read::Absent`]. An empty system domain reads
//! `Absent` only when its plist does not exist or `plutil` reads it as empty; otherwise
//! it is `Unusable`. A user or host domain is read as the user it belongs to, so a file
//! that user may not read is one their own programs may not read either; an empty export
//! of one is still not told apart from a missing domain. One such domain is measured: an
//! App-Sandboxed app's own domain lives in its container, which macOS guards as data from
//! other apps, so a reader whose responsible app holds no Full Disk Access gets an empty
//! export and the person a warning ([`CONTAINED`]). Those domains, and a path into any app's
//! container or group container, are read and written only by a pass whose responsible app
//! holds the grant ([`withheld`]): the Widgets row, the doctor's own reads, writes, rollback
//! and Restore, and a raw `[[defaults]]` row alike, by name or by a path, which has no `..`
//! ([`expand_rows`] refuses one with it, for every scope).
//!
//! Writes go through `defaults` too, never behind cfprefsd's back. A per-user write runs
//! as that user: run as root, even with `HOME` pointing at the user's home, `defaults`
//! writes root's own domain in `/var/root/Library/Preferences` (measured in the VM). A
//! system plist is read and written only below directories that root owns and that no one
//! else may write but the wheel or admin group, and never through a link. cfprefsd itself
//! still writes it as root by name, so in a directory the admin group may write
//! (`/Library/Application Support/CrashReporter`) a member of admin could swap the name
//! between this check and that write.

use super::{Ctx, Item};
use crate::baseline::fill_str;
use crate::model::{Effect, PValue, Probe, Read, User};
use crate::plist::{self, Entries};
use crate::port::{Cmd, Stat, System};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The one tool every preference read and write goes through.
pub const DEFAULTS: &str = "/usr/bin/defaults";
/// Relaunches a process after its preferences changed.
pub const KILLALL: &str = "/usr/bin/killall";
/// Asks whether a user has a GUI login session (`launchctl print gui/<uid>`).
pub const LAUNCHCTL: &str = "/bin/launchctl";
/// Reads a system plist file directly when `defaults export` printed an empty domain.
pub const PLUTIL: &str = "/usr/bin/plutil";

/// The preference domains macOS keeps in an App-Sandboxed app's container
/// (`~/Library/Containers/<id>/Data/Library/Preferences`), each with its app's name. macOS
/// guards a container as data from other apps (`kTCCServiceSystemPolicyAppDataDetailed`,
/// which does not allow prompting). Measured 2026-10-22 in a macOS 27.0 VM, a fresh account
/// and a dev build of aterm: aterm's child `defaults export com.apple.notificationcenterui -`
/// was denied (tccd: responsible aterm, accessing `/usr/bin/defaults`, requesting
/// `sandboxd`), printed an empty domain (181 bytes; `defaults read … widgets`: domain not
/// found), and macOS showed the person a notice that aterm was prevented from accessing data
/// from other apps (a dialog on 26.6); over SSH, whose `sshd-keygen-wrapper` holds Full Disk
/// Access, it printed all 8053 bytes. Once aterm was granted Full Disk Access, the same read
/// printed all of it with no relaunch: the grant lifts even a denial macOS already recorded.
/// `com.apple.WindowManager` raised no such request.
pub const CONTAINED: [(&str, &str); 1] =
    [("com.apple.notificationcenterui", "Notification Center")];

/// The app whose sandbox container `domain` is in: one of [`CONTAINED`] by name (in any
/// case, as `defaults` finds the domain), or a path
/// into any app's container or group container (`…/Library/Containers/<id>/…`,
/// `…/Library/Group Containers/<id>/…`: the roots macOS guards as data from other apps,
/// `aterm_containment::sbpl::APP_DATA_ROOTS`, matched without regard to case as the volume
/// names them), by its [`CONTAINED`] name, else by its id. The match is lexical, so a path
/// that reaches a container through `..` is not one: [`expand_rows`] refuses `..` in a
/// domain. Only a container's is measured for `defaults`; a group container is gated alike
/// by the App Data model (docs/DESIGN-macos-tcc-prompts-2026-08-30.md §1.6d), not measured.
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

/// Whether `domain` is in another app's sandbox container: one of [`CONTAINED`], or a path
/// into an app's container or group container ([`container_of`]).
#[must_use]
pub fn contained(domain: &str) -> bool {
    container_of(domain).is_some()
}

/// Why a process whose Full Disk Access is `full_disk_access`
/// ([`crate::model::HostFacts::full_disk_access`]) leaves `domain` alone, neither read nor
/// written: it is in another app's container ([`contained`]: one of [`CONTAINED`], by name or
/// by a path into it, or a path into any app's container or group container) and the grant
/// is not known to be held. `None`: it may touch it. The
/// grant is the one macOS checks: that of the app responsible for the process running the
/// pass (aterm for System State and a command in an aterm tab, the terminal for one run
/// elsewhere, `sshd-keygen-wrapper` for one over SSH), measured once per run.
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

/// `launchctl print gui/<uid>`'s exit when the user has no GUI session (27.0 VM: "Could
/// not find domain for user gui: 599"). A usage error exits 64.
const NO_SESSION: i32 = 112;
/// The groups a directory above a system plist may be writable by: wheel, and admin (stock
/// macOS's `/Library/Application Support/CrashReporter` is `root:admin 0775`).
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
    /// The domain (`com.apple.dock`, `NSGlobalDomain`; for `user`, also a plist path without
    /// `.plist`) or, for `system`, a domain under `/Library/Preferences` or an absolute plist
    /// path without `.plist`; never with a `..`.
    pub domain: String,
    /// The key.
    pub key: String,
    /// The wanted value: a TOML boolean, integer, float, string, array or table, or a
    /// UTC date-time (`2026-10-16T22:40:34Z`) for a plist `<date>`.
    pub value: aterm_toml::Value,
    /// Where it lives.
    pub scope: Scope,
    /// A process to relaunch after a change (`Dock`, `Finder`, `SystemUIServer`).
    #[serde(default)]
    pub restart: Option<String>,
}

/// One preference key: its domain, its key, and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The domain as `defaults` takes it: a name (`com.apple.dock`, `-g`), or for a
    /// system preference an absolute path without `.plist`.
    pub domain: String,
    /// The key.
    pub key: String,
    /// Where it lives.
    pub scope: Scope,
    /// The account a user or host preference belongs to; `None` for a system one.
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

    /// The system preference `key` of `domain`: a name under `/Library/Preferences` or
    /// an absolute path, with or without `.plist`.
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

    /// `defaults [-currentHost]`, as the target's user.
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

    /// `defaults [-currentHost] export <domain> -`: the whole domain as XML.
    #[must_use]
    pub fn export_cmd(&self) -> Cmd {
        self.tool().args(["export", self.domain.as_str(), "-"])
    }

    /// The `defaults write` that stores `value`. Scalars use the typed flags; a real goes
    /// as a `<real>` fragment, since `-float` stores a 32-bit float, and data, dates,
    /// arrays and dictionaries go as fragments.
    #[must_use]
    pub fn write_cmd(&self, value: &PValue) -> Cmd {
        let cmd = self
            .tool()
            .args(["write", self.domain.as_str(), self.key.as_str()]);
        match value {
            PValue::Bool(b) => cmd.args(["-bool", if *b { "true" } else { "false" }]),
            // `-int` was measured with a positive value; a negative one goes as the
            // `<integer>` fragment the VM read back.
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

    /// Every key of the target's domain. For a system domain the plist is checked first
    /// ([`system_file`]), and an empty export of one whose file exists is checked against
    /// the file ([`confirm_empty`]).
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
        match self.entries(sys) {
            Err(why) => Read::Unusable(why),
            Ok(entries) => match entries.into_iter().find(|(k, _)| *k == self.key) {
                None => Read::Absent,
                Some((_, Ok(v))) => Read::Value(v),
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

/// The entries of the dictionary `export` (a `defaults … export <domain> -`) prints. macOS
/// 27's export prints `<dict/>` and exits 0 for a domain that does not exist and for one
/// the reader may not read alike, so an empty answer alone is not "nothing set":
/// [`Target::entries`] checks a system domain's.
///
/// # Errors
/// One line when the export did not run, failed, or printed something other than a
/// dictionary.
pub fn read_domain(sys: &dyn System, export: &Cmd) -> Result<Entries, String> {
    let ran = sys
        .run(export)
        .map_err(|e| format!("{}: {e}", export.display()))?;
    if !ran.success() {
        return Err(format!("{}: {}", export.display(), ran.describe()));
    }
    plist::parse_domain(&ran.stdout).map_err(|why| format!("{}: {why}", export.display()))
}

/// Refuse a system plist `path` a root read or write could be redirected from: `path` must
/// be absolute with no `..`, and each directory above it that exists must be a directory,
/// not a link, owned by root and writable by no one but root and the wheel or admin group.
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
            // Not there yet: the directories above it decide.
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

/// Check a system plist `path` before root reads or writes it: the directories above it
/// ([`check_system_path`]), then the file, which must be a regular file when it exists.
/// Returns its `lstat`, or `None` when there is no file.
///
/// # Errors
/// One line saying what fails.
pub fn system_file(sys: &dyn System, path: &Path) -> Result<Option<Stat>, String> {
    check_system_path(sys, path)?;
    match sys.stat(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{} cannot be checked: {e}", path.display())),
        // A root read or write through a link would reach wherever the link points.
        Ok(s) if s.symlink || s.dir => Err(format!("{} is not a regular file", path.display())),
        Ok(s) => Ok(Some(s)),
    }
}

/// Accept an empty export of the system domain whose plist `path` exists only if `plutil`
/// reads that file as an empty dictionary. `defaults export` prints the same `<dict/>`
/// for a domain the reader may not read, and `plutil` says why it cannot.
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

/// What a read renders as in a report: the value, or `absent`.
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
    /// Whether `read` already satisfies this; `Err` when the read cannot tell.
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

/// One preference item: a key that should hold a value (or nothing), read, written and
/// read back through `defaults` as its owner.
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
}

impl Pref {
    /// A preference item. Its effect starts as `Login` for a user or host key and `Now`
    /// for a system one; [`Pref::restart`] makes it `Relaunch`.
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
        }
    }

    /// Relaunch `process` in the user's session after a change.
    #[must_use]
    pub fn restart(mut self, process: impl Into<String>) -> Self {
        self.restart = Some(process.into());
        self.effect = Effect::Relaunch;
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

    /// Why a change must wait: it relaunches a process of the user at the console while
    /// the Mac is in use (design §5.3).
    fn deferred(&self, ctx: &Ctx) -> Option<String> {
        let process = self.restart.as_ref()?;
        let user = self.target.user.as_ref()?;
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
        // Checked again here: the probe that planned this ran a while ago.
        let kept = match self.target.plist_path() {
            None => None,
            Some(path) => system_file(sys, &path)
                .map_err(|why| format!("not written: {why}"))?
                .map(|s| (path, s)),
        };
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        // Deleting a key that is already gone leaves what was wanted.
        let gone = self.want == Want::Unset
            && (ran.err().contains("Could not find key") || ran.err().contains("not found"));
        if !ran.success() && !gone {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        if let Some((path, was)) = kept {
            keep_owner_and_mode(sys, &path, was)?;
        }
        if let (Some(process), Some(user)) = (&self.restart, &self.target.user) {
            relaunch(sys, user, process)?;
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
}

/// The command that puts `target` back to `before`, saying which kind of revert it is:
/// the write of the value the probe read, or the delete of a key that was not set.
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

/// Put a system preference file's owner and mode back as they were before `defaults`
/// wrote it. cfprefsd rewrites the file whole: in the 27.0 VM a write to
/// `/Library/Application Support/CrashReporter/DiagnosticMessagesHistory` turned it from
/// `root:admin 0664` into `root:wheel 0600`. That directory is `root:admin 0775`, so the
/// owner and mode go through one descriptor ([`System::set_owner_and_mode`]), never a
/// path a member of admin could swap for a link to another file in between; the
/// permission bits never include setuid, setgid or sticky.
fn keep_owner_and_mode(sys: &dyn System, path: &Path, was: Stat) -> Result<(), String> {
    let mode = was.mode & 0o777;
    match sys.set_owner_and_mode(path, was.uid, was.gid, mode) {
        // A delete that emptied the domain may take the file with it.
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

/// Relaunch `process` in `user`'s login session, if there is one. Without a session the
/// preference is read at the next login, so nothing is relaunched.
///
/// # Errors
/// One line when the session check or the relaunch did not work.
pub fn relaunch(sys: &dyn System, user: &User, process: &str) -> Result<(), String> {
    let session = Cmd::new(LAUNCHCTL)
        .arg("print")
        .arg(format!("gui/{}", user.uid));
    match sys.run(&session) {
        Ok(ran) if ran.success() => {}
        // "Could not find domain for user gui: <uid>": nobody is logged in.
        Ok(ran) if ran.status == Some(NO_SESSION) => return Ok(()),
        Ok(ran) => {
            return Err(format!(
                "written, but {}: {}",
                session.display(),
                ran.describe()
            ));
        }
        Err(e) => return Err(format!("written, but {}: {e}", session.display())),
    }
    let kill = Cmd::new(KILLALL)
        .args(["-u", user.name.as_str(), process])
        .as_user(user)
        .in_session();
    match sys.run(&kill) {
        // 1: "No matching processes belonging to you were found": nothing to relaunch.
        Ok(ran) if ran.success() || ran.status == Some(1) => Ok(()),
        Ok(ran) => Err(format!(
            "written, but {}: {}",
            kill.display(),
            ran.describe()
        )),
        Err(e) => Err(format!("written, but {}: {e}", kill.display())),
    }
}

/// An item whose precondition is missing, so it only ever probes [`Probe::Blocked`]: a
/// row that cannot be built on this host is reported rather than dropped.
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

/// A TOML value as the property-list value `defaults` stores, with `{key}` placeholders
/// in its strings filled from `vars`.
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

/// Whether `name` can be handed to `killall` as a process name: no path, no option.
fn plain_process_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
}

/// The items `rows` ask for: one per user in `ctx.users` for a user or host row (with
/// `{user}` filled in its domain, key and strings), one for a system row.
///
/// # Errors
/// One line naming the row that cannot be expanded.
pub fn expand_rows(rows: &[Row], ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
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
                // Gated on the file the row resolves to: a name is one under
                // /Library/Preferences, never a container, and a path into one waits for the
                // grant as a user row's does.
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
                    // A `..` would reach another app's container by a path the gate below,
                    // which reads the path as written, does not see as one.
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
                    // Another app's container is neither read nor written without the
                    // grant, by this row as by the Widgets row: the row is reported, blocked.
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
                        pref = pref.restart(p.as_str());
                    }
                    items.push(Box::new(pref));
                }
            }
        }
    }
    Ok(items)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::model::{HostFacts, Mode, Outcome};
    use crate::port::{Fake, Ran};

    pub(crate) fn user(name: &str, uid: u32) -> User {
        User {
            name: name.into(),
            uid,
            gid: 20,
            home: format!("/Users/{name}"),
            shell: "/bin/zsh".into(),
            admin: uid == 501,
        }
    }

    pub(crate) fn ctx(users: &[User]) -> Ctx {
        Ctx {
            facts: HostFacts {
                os_version: "27.0".into(),
                os_build: "26A428".into(),
                arch: "arm64".into(),
                ..HostFacts::default()
            },
            users: users.to_vec(),
            console: None,
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt: true,
        }
    }

    /// A document as `defaults export` prints it.
    pub(crate) fn export(body: &str) -> Ran {
        Ran::ok(&format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n{body}\n</plist>\n"
        ))
    }

    /// What the 27.0 VM's `defaults export` printed for a domain that does not exist.
    pub(crate) fn missing() -> Ran {
        export("<dict/>")
    }

    /// `com.apple.AdLib` for the VM's admin, as `defaults export com.apple.AdLib -`
    /// printed it.
    const VM_ADLIB: &str = "<dict>\n\t<key>CKDPIDSyncState</key>\n\t<integer>1</integer>\n\t<key>CKSubscribedAccountsScrubbed</key>\n\t<true/>\n\t<key>allowApplePersonalizedAdvertising</key>\n\t<false/>\n\t<key>allowIdentifierForAdvertising</key>\n\t<true/>\n\t<key>personalizedAdsMigrated</key>\n\t<true/>\n</dict>";

    fn run(
        items: &[Box<dyn Item>],
        sys: &Fake,
        ctx: &Ctx,
        mode: Mode,
    ) -> Vec<crate::model::ItemReport> {
        engine::run(items, mode, sys, ctx, &mut NoJournal)
    }

    fn one(pref: Pref) -> Vec<Box<dyn Item>> {
        vec![Box::new(pref)]
    }

    fn row(text: &str) -> Vec<Row> {
        #[derive(Deserialize)]
        struct Doc {
            defaults: Vec<Row>,
        }
        aterm_toml::from_str::<Doc>(text).unwrap().defaults
    }

    #[test]
    fn a_missing_domain_and_a_missing_key_read_absent_and_a_present_key_reads_its_value() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        sys.on(
            "admin$ /usr/bin/defaults export com.example.none -",
            missing(),
        );
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.AdLib -",
            export(VM_ADLIB),
        );
        let none = Target::user("com.example.none", "k", &admin);
        assert_eq!(none.read(&sys), Read::Absent);
        let ads = Target::user(
            "com.apple.AdLib",
            "allowApplePersonalizedAdvertising",
            &admin,
        );
        assert_eq!(ads.read(&sys), Read::Value(PValue::Bool(false)));
        let other = Target::user("com.apple.AdLib", "noSuchKey", &admin);
        assert_eq!(other.read(&sys), Read::Absent);
    }

    #[test]
    fn a_failed_or_garbled_export_is_unusable_never_absent() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.dock -",
            Ran::exit(1, "Error: Need a path to write to"),
        );
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.finder -",
            Ran::ok("<plist><dict><key>x</key>"),
        );
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.list -",
            export("<array/>"),
        );
        for (domain, needle) in [
            ("com.apple.dock", "exit 1: Error: Need a path"),
            ("com.apple.finder", "plist: at byte"),
            ("com.apple.list", "not a dictionary"),
            ("com.apple.unscripted", "no reply scripted"),
        ] {
            match Target::user(domain, "k", &admin).read(&sys) {
                Read::Unusable(why) => assert!(why.contains(needle), "{domain}: {why}"),
                other => panic!("{domain}: {other:?}"),
            }
        }
        let pref = Pref::new(
            "defaults",
            "x".into(),
            "x".into(),
            Target::user("com.apple.dock", "autohide", &admin),
            Want::Value(PValue::Bool(true)),
        );
        let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Unknown);
        assert!(!sys.calls().iter().any(|c| c.contains(" write ")));
    }

    #[test]
    fn each_type_is_written_with_the_flag_or_fragment_the_vm_accepted() {
        let admin = user("admin", 501);
        let t = Target::user("com.example", "k", &admin);
        let line = |v: PValue| t.write_cmd(&v).display();
        assert_eq!(
            line(PValue::Bool(false)),
            "admin$ /usr/bin/defaults write com.example k -bool false"
        );
        assert_eq!(
            line(PValue::Int(30_064_771_129)),
            "admin$ /usr/bin/defaults write com.example k -int 30064771129"
        );
        assert_eq!(
            line(PValue::Int(-3)),
            "admin$ /usr/bin/defaults write com.example k '<integer>-3</integer>'"
        );
        assert_eq!(
            line(PValue::Str("Contacts Only".into())),
            "admin$ /usr/bin/defaults write com.example k -string 'Contacts Only'"
        );
        assert_eq!(
            line(PValue::Real(0.1)),
            "admin$ /usr/bin/defaults write com.example k '<real>0.10000000000000001</real>'"
        );
        assert_eq!(
            line(PValue::Date("2026-10-16T22:40:34Z".into())),
            "admin$ /usr/bin/defaults write com.example k '<date>2026-10-16T22:40:34Z</date>'"
        );
        assert_eq!(
            line(PValue::Array(vec![PValue::Int(1)])),
            "admin$ /usr/bin/defaults write com.example k '<array><integer>1</integer></array>'"
        );
        let host = Target::host("-g", "k", &admin);
        assert_eq!(
            host.delete_cmd().display(),
            "admin$ /usr/bin/defaults -currentHost delete -g k"
        );
        assert_eq!(
            host.export_cmd().display(),
            "admin$ /usr/bin/defaults -currentHost export -g -"
        );
        let sys = Target::system("com.apple.SoftwareUpdate", "AutomaticDownload");
        assert_eq!(
            sys.write_cmd(&PValue::Bool(true)).display(),
            "/usr/bin/defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool true"
        );
        assert_eq!(
            Target::system("/Library/Preferences/com.apple.Bluetooth.plist", "k").domain,
            "/Library/Preferences/com.apple.Bluetooth"
        );
    }

    #[test]
    fn a_change_is_written_as_the_user_and_counts_only_once_it_reads_back() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        let export_key = "admin$ /usr/bin/defaults export com.apple.desktopservices -";
        sys.on(export_key, missing());
        sys.on(
            export_key,
            export("<dict>\n\t<key>DSDontWriteNetworkStores</key>\n\t<true/>\n</dict>"),
        );
        sys.on(
            "admin$ /usr/bin/defaults write com.apple.desktopservices DSDontWriteNetworkStores -bool true",
            Ran::ok(""),
        );
        let pref = Pref::new(
            "defaults",
            "d".into(),
            "d".into(),
            Target::user(
                "com.apple.desktopservices",
                "DSDontWriteNetworkStores",
                &admin,
            ),
            Want::Value(PValue::Bool(true)),
        );
        let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(rows[0].from.as_deref(), Some("absent"));
        assert_eq!(rows[0].to.as_deref(), Some("true"));
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(
                "admin$ /usr/bin/defaults delete com.apple.desktopservices DSDontWriteNetworkStores  # it was not set before this run"
            )
        );
        assert_eq!(rows[0].effect, Effect::Login);
    }

    #[test]
    fn a_write_that_does_not_read_back_fails_with_what_it_reads() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.AdLib -",
            export(VM_ADLIB),
        );
        sys.on(
            "admin$ /usr/bin/defaults write com.apple.AdLib allowApplePersonalizedAdvertising -bool true",
            Ran::ok(""),
        );
        let pref = Pref::new(
            "defaults",
            "ads".into(),
            "ads".into(),
            Target::user(
                "com.apple.AdLib",
                "allowApplePersonalizedAdvertising",
                &admin,
            ),
            Want::Value(PValue::Bool(true)),
        );
        let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert_eq!(
            rows[0].why.as_deref(),
            Some("applied, but it still reads false")
        );
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(
                "admin$ /usr/bin/defaults write com.apple.AdLib allowApplePersonalizedAdvertising -bool false  # restores the value before this run"
            )
        );
    }

    #[test]
    fn a_boolean_held_as_the_integer_macos_reads_for_it_is_left_alone() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.x -",
            export("<dict>\n\t<key>k</key>\n\t<integer>0</integer>\n</dict>"),
        );
        let pref = Pref::new(
            "defaults",
            "x".into(),
            "x".into(),
            Target::user("com.apple.x", "k", &admin),
            Want::Value(PValue::Bool(false)),
        );
        let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(sys.calls().len(), 1);
    }

    #[test]
    fn an_unset_want_deletes_the_key_and_a_key_already_gone_is_not_an_error() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        let key = "admin$ /usr/bin/defaults export com.apple.sharingd -";
        sys.on(
            key,
            export("<dict>\n\t<key>DiscoverableMode</key>\n\t<string>Off</string>\n</dict>"),
        );
        sys.on(key, missing());
        sys.on(
            "admin$ /usr/bin/defaults delete com.apple.sharingd DiscoverableMode",
            Ran::exit(
                1,
                "Error: Could not find key 'DiscoverableMode' in domain 'com.apple.sharingd'.\nDefaults have not been changed.",
            ),
        );
        let pref = Pref::new(
            "defaults",
            "a".into(),
            "a".into(),
            Target::user("com.apple.sharingd", "DiscoverableMode", &admin),
            Want::Unset,
        );
        let rows = run(&one(pref), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(rows[0].from.as_deref(), Some("\"Off\""));
        assert_eq!(rows[0].to.as_deref(), Some("absent"));
        assert_eq!(
            rows[0].revert.as_deref(),
            Some(
                "admin$ /usr/bin/defaults write com.apple.sharingd DiscoverableMode -string Off  # restores the value before this run"
            )
        );
    }

    #[test]
    fn a_system_write_runs_as_root_and_puts_the_files_owner_and_mode_back() {
        let sys = Fake::root();
        let path = "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory.plist";
        // The VM's file before the write: root:admin 0664.
        sys.stat_of(
            path,
            Stat {
                uid: 0,
                gid: 80,
                mode: 0o664,
                dir: false,
                symlink: false,
                len: 156,
            },
        );
        let export_key = "/usr/bin/defaults export '/Library/Application Support/CrashReporter/DiagnosticMessagesHistory' -";
        // What the VM's export printed (as root) before the write: no AutoSubmit.
        let before = "<dict>\n\t<key>LastCleanupCalled</key>\n\t<date>2026-10-16T23:02:33Z</date>\n\t<key>LastFullSubmissionCalled</key>\n\t<date>2026-10-16T22:41:38Z</date>\n</dict>";
        sys.on(export_key, export(before));
        sys.on(
            export_key,
            export("<dict>\n\t<key>AutoSubmit</key>\n\t<false/>\n</dict>"),
        );
        sys.on(
            "/usr/bin/defaults write '/Library/Application Support/CrashReporter/DiagnosticMessagesHistory' AutoSubmit -bool false",
            Ran::ok(""),
        );
        let pref = Pref::new(
            "consumer",
            "consumer.analytics.auto-submit".into(),
            "t".into(),
            Target::system(
                "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory",
                "AutoSubmit",
            ),
            Want::Value(PValue::Bool(false)),
        );
        let rows = run(&one(pref), &sys, &ctx(&[]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        let calls = sys.calls();
        let write = calls.iter().position(|c| c.contains(" write ")).unwrap();
        let own = calls
            .iter()
            .position(|c| c == &format!("own 0:80 664 {path}"))
            .unwrap();
        assert!(write < own, "{calls:?}");
        assert_eq!(rows[0].effect, Effect::Now);
    }

    fn dir(uid: u32, gid: u32, mode: u32) -> Stat {
        Stat {
            uid,
            gid,
            mode,
            dir: true,
            symlink: false,
            len: 0,
        }
    }

    #[test]
    fn an_empty_export_of_a_system_domain_whose_file_exists_is_read_from_the_file() {
        // `defaults export` prints `<dict/>` for an unreadable domain as for a missing one
        // (the 27.0 VM's admin, exporting the root-only com.apple.apsd).
        let path = "/Library/Preferences/com.apple.apsd.plist";
        let export_key = "/usr/bin/defaults export /Library/Preferences/com.apple.apsd -";
        let plutil = format!("/usr/bin/plutil -convert xml1 -o - {path}");
        let target = Target::system("com.apple.apsd", "k");

        let sys = Fake::root();
        sys.file(path, b"bplist00");
        sys.on(export_key, missing());
        sys.on(
            &plutil,
            Ran::exit(
                1,
                "/Library/Preferences/com.apple.apsd.plist: (The file \u{201c}com.apple.apsd.plist\u{201d} couldn\u{2019}t be opened because you don\u{2019}t have permission to view it.)",
            ),
        );
        match target.read(&sys) {
            Read::Unusable(why) => assert!(why.contains("permission to view it"), "{why}"),
            other => panic!("an unreadable domain read {other:?}"),
        }
        // An Unset want is never satisfied by an unreadable domain.
        let pref = Pref::new(
            "defaults",
            "x".into(),
            "x".into(),
            target.clone(),
            Want::Unset,
        );
        assert!(matches!(
            pref.probe(&sys, &ctx(&[])),
            Probe::Unusable { .. }
        ));

        let sys = Fake::root();
        sys.file(path, b"bplist00");
        sys.on(export_key, missing());
        sys.on(&plutil, missing());
        assert_eq!(target.read(&sys), Read::Absent);

        let sys = Fake::root();
        sys.file(path, b"bplist00");
        sys.on(export_key, missing());
        sys.on(
            &plutil,
            export("<dict>\n\t<key>k</key>\n\t<true/>\n</dict>"),
        );
        match target.read(&sys) {
            Read::Unusable(why) => assert!(why.contains("holds 1 keys"), "{why}"),
            other => panic!("{other:?}"),
        }

        // No file at all: the domain does not exist, and plutil is not asked.
        let sys = Fake::root();
        sys.on(export_key, missing());
        assert_eq!(target.read(&sys), Read::Absent);
        assert_eq!(sys.calls(), [export_key]);
    }

    #[test]
    fn a_key_whose_value_this_crate_cannot_hold_is_unusable_and_its_neighbours_read() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        // As `com.apple.ncprefs` holds it on a 27.0.1 Mac.
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.ncprefs -",
            export("<dict>\n\t<key>big</key>\n\t<integer>9223372036854775815</integer>\n\t<key>k</key>\n\t<integer>3</integer>\n</dict>"),
        );
        assert_eq!(
            Target::user("com.apple.ncprefs", "k", &admin).read(&sys),
            Read::Value(PValue::Int(3))
        );
        match Target::user("com.apple.ncprefs", "big", &admin).read(&sys) {
            Read::Unusable(why) => assert!(why.contains("64-bit signed range"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_system_plist_below_a_directory_others_control_is_neither_read_nor_written() {
        for (dir_path, stat, needle) in [
            ("/Users//Shared", dir(0, 0, 0o1777), "writable by everyone"),
            ("/Users//alice", dir(501, 20, 0o755), "owned by uid 501"),
            ("/opt/x", dir(0, 20, 0o775), "writable by group 20"),
            (
                "/var",
                Stat {
                    symlink: true,
                    dir: false,
                    ..dir(0, 0, 0o755)
                },
                "symbolic link",
            ),
        ] {
            let sys = Fake::root();
            sys.stat_of(dir_path, stat);
            let target = Target::system(&format!("{dir_path}/com.example"), "k");
            match target.read(&sys) {
                Read::Unusable(why) => assert!(why.contains(needle), "{dir_path}: {why}"),
                other => panic!("{dir_path}: {other:?}"),
            }
            assert!(sys.calls().is_empty(), "{dir_path}: {:?}", sys.calls());
        }
        // Stock macOS's CrashReporter directory: root-owned, writable by admin.
        let sys = Fake::root();
        sys.stat_of(
            "/Library/Application Support/CrashReporter",
            dir(0, 80, 0o775),
        );
        assert!(
            check_system_path(
                &sys,
                Path::new(
                    "/Library/Application Support/CrashReporter/DiagnosticMessagesHistory.plist"
                )
            )
            .is_ok()
        );
        assert!(
            check_system_path(&sys, Path::new("/Library/Preferences/../../etc/x.plist")).is_err()
        );
    }

    #[test]
    fn a_system_plist_that_is_a_link_is_neither_read_nor_written_through() {
        let sys = Fake::root();
        let path = "/Library/Preferences/com.example.plist";
        sys.stat_of(
            path,
            Stat {
                uid: 501,
                gid: 80,
                mode: 0o755,
                dir: false,
                symlink: true,
                len: 20,
            },
        );
        sys.on(
            "/usr/bin/defaults export /Library/Preferences/com.example -",
            export("<dict>\n\t<key>k</key>\n\t<false/>\n</dict>"),
        );
        let pref = Pref::new(
            "defaults",
            "x".into(),
            "x".into(),
            Target::system("com.example", "k"),
            Want::Value(PValue::Bool(true)),
        );
        assert!(
            matches!(pref.probe(&sys, &ctx(&[])), Probe::Unusable { why } if why.contains("not a regular file"))
        );
        // The apply checks again, for a link planted after the probe.
        let err = pref.apply(&sys, &ctx(&[])).unwrap_err();
        assert!(err.starts_with("not written: "), "{err}");
        assert!(sys.calls().is_empty(), "{:?}", sys.calls());
    }

    fn sharingd(admin: &User) -> Pref {
        Pref::new(
            "consumer",
            "consumer.airdrop@admin".into(),
            "airdrop".into(),
            Target::user("com.apple.sharingd", "DiscoverableMode", admin),
            Want::Value(PValue::Str("Off".into())),
        )
        .restart("sharingd")
    }

    fn script_airdrop(sys: &Fake) {
        let key = "admin$ /usr/bin/defaults export com.apple.sharingd -";
        sys.on(key, missing());
        sys.on(
            key,
            export("<dict>\n\t<key>DiscoverableMode</key>\n\t<string>Off</string>\n</dict>"),
        );
        sys.on(
            "admin$ /usr/bin/defaults write com.apple.sharingd DiscoverableMode -string Off",
            Ran::ok(""),
        );
    }

    #[test]
    fn a_change_relaunches_its_process_in_the_users_session_after_the_write() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        script_airdrop(&sys);
        sys.on("/bin/launchctl print gui/501", Ran::ok("gui/501 = {\n"));
        sys.on("admin@gui$ /usr/bin/killall -u admin sharingd", Ran::ok(""));
        let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert_eq!(rows[0].effect, Effect::Relaunch);
        let calls = sys.calls();
        let write = calls.iter().position(|c| c.contains(" write ")).unwrap();
        let kill = calls.iter().position(|c| c.contains("killall")).unwrap();
        assert!(write < kill, "{calls:?}");
        assert!(
            rows[0]
                .revert
                .as_deref()
                .unwrap()
                .ends_with("; then relaunch sharingd")
        );
    }

    #[test]
    fn without_a_login_session_nothing_is_relaunched() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        script_airdrop(&sys);
        // What the VM's launchctl answered for a uid with no GUI session.
        sys.on(
            "/bin/launchctl print gui/501",
            Ran::exit(112, "Bad request.\nCould not find domain for user gui: 501"),
        );
        let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
        assert!(!sys.calls().iter().any(|c| c.contains("killall")));
    }

    #[test]
    fn a_process_that_is_not_running_needs_no_relaunch() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        script_airdrop(&sys);
        sys.on("/bin/launchctl print gui/501", Ran::ok(""));
        sys.on(
            "admin@gui$ /usr/bin/killall -u admin sharingd",
            Ran::exit(1, "No matching processes belonging to you were found"),
        );
        let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{:?}", rows[0].why);
    }

    #[test]
    fn a_session_check_that_fails_for_another_reason_is_a_failure_not_no_session() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        script_airdrop(&sys);
        // 64 is launchctl's usage error (the VM's `launchctl print gui/abc`).
        sys.on(
            "/bin/launchctl print gui/501",
            Ran::exit(64, "Unrecognized target specifier."),
        );
        let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0].why.as_deref().unwrap().contains("exit 64"),
            "{:?}",
            rows[0].why
        );
    }

    #[test]
    fn a_failed_write_relaunches_nothing() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.sharingd -",
            missing(),
        );
        sys.on(
            "admin$ /usr/bin/defaults write com.apple.sharingd DiscoverableMode -string Off",
            Ran::exit(1, "Could not write domain"),
        );
        let rows = run(&one(sharingd(&admin)), &sys, &ctx(&[admin]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Failed);
        assert!(
            rows[0]
                .why
                .as_deref()
                .unwrap()
                .contains("Could not write domain")
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("launchctl") || c.contains("killall"))
        );
    }

    #[test]
    fn a_relaunch_of_the_console_users_process_waits_while_the_mac_is_in_use() {
        let admin = user("admin", 501);
        let sys = Fake::root();
        sys.on(
            "admin$ /usr/bin/defaults export com.apple.sharingd -",
            missing(),
        );
        let mut busy = ctx(std::slice::from_ref(&admin));
        busy.disrupt = false;
        busy.console = Some(admin.clone());
        let rows = run(&one(sharingd(&admin)), &sys, &busy, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Deferred);
        assert!(rows[0].why.as_deref().unwrap().contains("at the console"));
        assert!(!sys.calls().iter().any(|c| c.contains(" write ")));
        // Someone else at the console, or nobody: it goes ahead.
        let mut other = busy.clone();
        other.console = Some(user("bob", 502));
        let rows = run(&one(sharingd(&admin)), &sys, &other, Mode::Plan);
        assert_eq!(rows[0].outcome, Outcome::Planned);
    }

    #[test]
    fn user_rows_expand_per_user_with_their_name_filled_and_system_rows_once() {
        let rows = row(r#"
[[defaults]]
domain = "com.apple.dock"
key = "autohide"
value = true
scope = "user"
restart = "Dock"

[[defaults]]
domain = "com.example.{user}"
key = "home"
value = { path = "/Users/{user}/x", n = [1, 2.5] }
scope = "host"

[[defaults]]
domain = "com.apple.loginwindow"
key = "GuestEnabled"
value = false
scope = "system"

[[defaults]]
domain = "com.example"
key = "when"
value = 2026-10-16T22:40:34Z
scope = "user"
"#);
        let users = [user("alice", 501), user("bob", 502)];
        let items = expand_rows(&rows, &ctx(&users)).unwrap();
        let ids: Vec<String> = items.iter().map(|i| i.id()).collect();
        assert_eq!(
            ids,
            [
                "defaults.com.apple.dock.autohide@alice",
                "defaults.com.apple.dock.autohide@bob",
                "defaults.byhost.com.example.alice.home@alice",
                "defaults.byhost.com.example.bob.home@bob",
                "defaults.system.com.apple.loginwindow.GuestEnabled",
                "defaults.com.example.when@alice",
                "defaults.com.example.when@bob",
            ]
        );
        assert_eq!(items[0].effect(), Effect::Relaunch);
        assert_eq!(items[4].effect(), Effect::Now);
        let sys = Fake::root();
        sys.on(
            "bob$ /usr/bin/defaults -currentHost export com.example.bob -",
            missing(),
        );
        let probe = items[3].probe(&sys, &ctx(&users));
        assert_eq!(
            probe,
            Probe::Change {
                from: "absent".into(),
                to: "{n = [1, 2.5], path = \"/Users//bob/x\"}".into(),
            }
        );
        assert_eq!(
            items[4].title(),
            "/Library/Preferences/com.apple.loginwindow GuestEnabled = false (system)"
        );
    }

    #[test]
    fn rows_that_cannot_be_applied_as_written_are_refused() {
        let users = [user("alice", 501)];
        for (text, needle) in [
            (
                "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 1\nscope = \"system\"\nrestart = \"Dock\"\n",
                "a system row has no user",
            ),
            (
                "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 1\nscope = \"user\"\nrestart = \"-9\"\n",
                "not a process name",
            ),
            (
                "[[defaults]]\ndomain = \"/Users//x\"\nkey = \"k\"\nvalue = 1\nscope = \"host\"\n",
                "a name, not a path",
            ),
            (
                "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 2026-10-16T22:40:34-07:00\nscope = \"user\"\n",
                "UTC to the second",
            ),
            (
                "[[defaults]]\ndomain = \"\"\nkey = \"k\"\nvalue = 1\nscope = \"user\"\n",
                "needs a domain and a key",
            ),
            (
                "[[defaults]]\ndomain = \"../LaunchDaemons/x\"\nkey = \"k\"\nvalue = 1\nscope = \"system\"\n",
                "without `..`",
            ),
        ] {
            let err = expand_rows(&row(text), &ctx(&users)).err().unwrap();
            assert!(err.contains(needle), "{text}: {err}");
            assert!(err.starts_with("[[defaults]] row 1"), "{err}");
        }
    }

    #[test]
    fn an_unknown_field_in_a_row_is_refused_rather_than_ignored() {
        #[derive(Deserialize, Debug)]
        struct Doc {
            #[expect(dead_code, reason = "only the parse result is checked")]
            defaults: Vec<Row>,
        }
        let err = aterm_toml::from_str::<Doc>(
            "[[defaults]]\ndomain = \"x\"\nkey = \"k\"\nvalue = 1\nscope = \"user\"\nrestrat = \"Dock\"\n",
        )
        .unwrap_err();
        assert!(err.to_string().contains("restrat"), "{err}");
    }

    #[test]
    fn an_unmet_item_is_blocked_and_never_applies() {
        let item = Unmet {
            family: "setup",
            id: "setup.x@a".into(),
            title: "x".into(),
            why: "the host's macOS version was not read".into(),
        };
        let items: Vec<Box<dyn Item>> = vec![Box::new(item)];
        let sys = Fake::root();
        let rows = run(&items, &sys, &ctx(&[]), Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Blocked);
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn the_live_tool_reads_a_missing_system_domain_as_absent_and_a_real_one_as_known() {
        // Read only: `defaults export` of system domains, as whoever runs the tests.
        if !Path::new(DEFAULTS).exists() {
            aterm_uds::host::could_not_run(format!("no {DEFAULTS} here (macOS ships it)"));
        }
        let live = crate::port::Live::new();
        let none = Target::system("com.aterm.machines.no-such-domain", "Key");
        assert_eq!(none.read(&live), Read::Absent);
        let languages = Target::system(".GlobalPreferences", "AppleLanguages");
        let read = languages.read(&live);
        assert!(!matches!(read, Read::Unusable(_)), "{read:?}");
    }

    #[test]
    fn the_live_tool_reads_a_system_domain_it_may_not_read_as_unusable_not_absent() {
        // Read only: `defaults export` and `plutil` of a root-only plist (root:wheel 0600
        // on macOS 27), run by a test that is not root. Its export prints `<dict/>`.
        let path = Path::new("/Library/Preferences/com.apple.apsd.plist");
        let live = crate::port::Live::new();
        let denied = matches!(
            std::fs::File::open(path),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied
        );
        if !Path::new(DEFAULTS).exists() {
            aterm_uds::host::could_not_run(format!("no {DEFAULTS} here (macOS ships it)"));
        }
        aterm_uds::host::could_not_run_as_root("a root-only (0600) system plist");
        if !denied {
            aterm_uds::host::could_not_run(format!(
                "{} is not refused to this user here, so no domain stands in as unreadable",
                path.display()
            ));
        }
        let read = Target::system("com.apple.apsd", "Anything").read(&live);
        assert!(matches!(read, Read::Unusable(_)), "{read:?}");
    }

    /// Only a grant known to be held opens another app's container: refused and not
    /// measured both leave it alone, and a domain outside a container is never withheld.
    /// Negative control: treating an unmeasured grant as held hands the row a read that
    /// macOS answers with its warning and an empty domain.
    #[test]
    fn a_container_domain_is_withheld_unless_full_disk_access_is_known_held() {
        for unknown in [None, Some(false)] {
            let why = withheld("com.apple.notificationcenterui", unknown).unwrap();
            assert!(
                why.contains("Notification Center") && why.contains("Full Disk Access"),
                "{why}"
            );
        }
        assert_eq!(withheld("com.apple.notificationcenterui", Some(true)), None);
        for open in ["com.apple.WindowManager", "com.apple.dock", "-g"] {
            assert_eq!(withheld(open, None), None, "{open}");
            assert!(!contained(open), "{open}");
        }
        assert!(contained("com.apple.notificationcenterui"));
        // A group container, as macOS spells it and in another case.
        for path in [
            "/Users//alice/Library/Group Containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example",
            "/Users//alice/Library/group containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example",
        ] {
            assert!(contained(path), "{path}");
            assert!(withheld(path, None).is_some(), "{path}");
        }
    }

    /// A raw `[[defaults]]` row on another app's container (the agent's baseline, `aterm
    /// machines here`) is gated as the Widgets row is, whether it names the domain or spells
    /// a path into the container (Notification Center's, or any app's, in any case) or into
    /// a group container (in any case), or names Notification Center's domain in another
    /// case: without a grant known held (not measured, or refused) its item is
    /// a blocked row under its usual id that runs nothing, apply included; with it, the row
    /// reads its domain. A row on an open domain, by name or path, is never gated. Negative
    /// controls: the granted arm's probe exports each container; matching the container's
    /// name alone (as the gate did) exports the path rows without the grant, matching
    /// `Library/Containers` alone, or `Group Containers` in its own case only, the group
    /// container's, and matching the name in its own case only the other-case row.
    #[test]
    fn a_raw_row_on_another_apps_container_is_read_and_written_only_with_the_grant() {
        const NC_PATH: &str = "/Users//alice/Library/Containers/com.apple.notificationcenterui/Data/Library/Preferences/com.apple.notificationcenterui";
        const OTHER_PATH: &str = "/Users//alice/library/containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed";
        const GROUP_PATH: &str = "/Users//alice/Library/group containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example";
        let rows = row(r#"
[[defaults]]
domain = "com.apple.notificationcenterui"
key = "widgets"
value = { instances = [] }
scope = "user"

[[defaults]]
domain = "/Users/{user}/Library/Containers/com.apple.notificationcenterui/Data/Library/Preferences/com.apple.notificationcenterui"
key = "widgets"
value = { instances = [] }
scope = "user"

[[defaults]]
domain = "/Users/{user}/library/containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed"
key = "Shown"
value = false
scope = "user"

[[defaults]]
domain = "/Users/{user}/Library/group containers/ABCDE12345.group.example/Library/Preferences/ABCDE12345.group.example"
key = "Shown"
value = false
scope = "user"

[[defaults]]
domain = "com.apple.NotificationCenterUI"
key = "widgets"
value = { instances = [] }
scope = "user"

[[defaults]]
domain = "com.apple.dock"
key = "autohide"
value = true
scope = "user"

[[defaults]]
domain = "/Users/{user}/Library/Preferences/com.apple.dock"
key = "autohide"
value = true
scope = "user"
"#);
        let alice = user("alice", 501);
        // As the fake shows a command: an argument with a space in single quotes.
        let export = |domain: &str| {
            let shown = if domain.contains(' ') {
                format!("'{domain}'")
            } else {
                domain.to_owned()
            };
            format!("alice$ /usr/bin/defaults export {shown} -")
        };
        let contained = [
            "com.apple.notificationcenterui",
            NC_PATH,
            OTHER_PATH,
            GROUP_PATH,
            "com.apple.NotificationCenterUI",
        ];
        let open = [
            "com.apple.dock",
            "/Users//alice/Library/Preferences/com.apple.dock",
        ];
        for full_disk_access in [None, Some(false), Some(true)] {
            let mut context = ctx(std::slice::from_ref(&alice));
            context.facts.full_disk_access = full_disk_access;
            let items = expand_rows(&rows, &context).unwrap();
            assert_eq!(items.len(), 7);
            assert_eq!(
                items[0].id(),
                "defaults.com.apple.notificationcenterui.widgets@alice"
            );
            for (item, domain) in items.iter().zip(contained.into_iter().chain(open)) {
                let sys = Fake::root();
                sys.on(&export(domain), missing());
                let probe = item.probe(&sys, &context);
                let exported = sys.calls().contains(&export(domain));
                if full_disk_access == Some(true) || open.contains(&domain) {
                    assert!(matches!(probe, Probe::Change { .. }), "{domain}: {probe:?}");
                    assert!(exported, "{domain}: {:?}", sys.calls());
                } else {
                    let Probe::Blocked { why } = probe else {
                        panic!("{full_disk_access:?} {domain}: {probe:?}");
                    };
                    assert!(why.contains("Full Disk Access"), "{why}");
                    assert!(item.apply(&sys, &context).is_err());
                    assert!(sys.calls().is_empty(), "{domain}: {:?}", sys.calls());
                }
            }
        }
    }

    /// A user row whose domain reaches a path through `..` is refused, whatever the grant,
    /// as a system row's is: the gate reads a path as written, and a `..` through
    /// another directory reaches another app's container by a spelling it does not see as
    /// one (the kernel resolves the `..`, through a symbolic link as it is). Negative control:
    /// without the refusal each row expands, the first into a read of Notification Center's
    /// container without the grant, since the gate does not see that path as one.
    #[test]
    fn a_user_domain_that_climbs_with_dotdot_is_refused_whatever_the_grant() {
        let alice = user("alice", 501);
        for domain in [
            "/Users/{user}/Library/Preferences/../Containers/com.apple.notificationcenterui/Data/Library/Preferences/com.apple.notificationcenterui",
            "/Users/{user}/Library/Application Support/../Containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed",
            "/Users/{user}/Library/Preferences/../../{user}/Library/Preferences/com.apple.dock",
        ] {
            let rows = row(&format!(
                "[[defaults]]\ndomain = \"{domain}\"\nkey = \"widgets\"\nvalue = 1\nscope = \"user\"\n"
            ));
            for full_disk_access in [None, Some(false), Some(true)] {
                let mut context = ctx(std::slice::from_ref(&alice));
                context.facts.full_disk_access = full_disk_access;
                let err = expand_rows(&rows, &context)
                    .err()
                    .unwrap_or_else(|| panic!("{full_disk_access:?}: {domain} expanded"));
                assert!(err.contains("without `..`"), "{err}");
            }
        }
    }

    /// A system row is gated on the file it resolves to: one whose path is in an app's
    /// container is, without a grant known held, a blocked row that runs nothing (no stat of
    /// its directories, no export), and with the grant it reads its file; one that names
    /// Notification Center's domain resolves under /Library/Preferences, no container, and
    /// is never gated. Negative control: without the system arm's gate, the container row
    /// probes its path without the grant.
    #[test]
    fn a_system_row_is_gated_on_the_file_it_resolves_to() {
        let rows = row(r#"
[[defaults]]
domain = "/private/var/root/Library/Containers/com.example.Sandboxed/Data/Library/Preferences/com.example.Sandboxed"
key = "Shown"
value = false
scope = "system"

[[defaults]]
domain = "com.apple.notificationcenterui"
key = "widgets"
value = false
scope = "system"
"#);
        for full_disk_access in [None, Some(false), Some(true)] {
            let mut context = ctx(&[]);
            context.facts.full_disk_access = full_disk_access;
            let items = expand_rows(&rows, &context).unwrap();
            assert_eq!(items.len(), 2);
            for (n, item) in items.iter().enumerate() {
                let sys = Fake::root();
                let probe = item.probe(&sys, &context);
                let gated =
                    matches!(&probe, Probe::Blocked { why } if why.contains("Full Disk Access"));
                if n == 0 && full_disk_access != Some(true) {
                    assert!(gated, "{full_disk_access:?}: {probe:?}");
                    assert!(item.apply(&sys, &context).is_err());
                    assert!(sys.calls().is_empty(), "{:?}", sys.calls());
                } else {
                    assert!(!gated, "{full_disk_access:?} row {n}: {probe:?}");
                    assert!(!sys.calls().is_empty(), "{full_disk_access:?} row {n}");
                }
            }
        }
    }
}
