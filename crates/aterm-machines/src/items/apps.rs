// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Installs pinned `.dmg`, `.zip`, and `.pkg` application artifacts verified by SHA-256, Team ID, and Gatekeeper.

use super::{Ctx, Item};
use crate::model::{Effect, Probe, Read, User};
use crate::port::{Cmd, Process, Ran, System};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Target directory where application bundles are installed.
pub const APPLICATIONS: &str = "/Applications";

/// Staging root directory (`/Library/Management/aterm/stage`).
pub const STAGE_ROOT: &str = "/Library/Management/aterm/stage";

/// The bound on one `codesign --verify --deep` run: a hang detector, not a latency budget
/// (Emacs.app, 180 MB of nested code, took 5.6 s in the VM).
const VERIFY_BUDGET: Duration = Duration::from_secs(180);

/// The bound on one `spctl -a`. Also a hang detector, and longer: an app with no stapled
/// ticket is looked up with Apple's notary service, and in the VM `spctl -a -t exec` on
/// Emacs.app took 93 s, and over 180 s on its first assessment there.
const GATEKEEPER_BUDGET: Duration = Duration::from_secs(900);

/// The bound on `ditto`, `hdiutil` and `pkgutil --expand-full` over one artifact.
const STAGE_BUDGET: Duration = Duration::from_secs(900);

/// The bound on `installer -pkg`. Homebrew.pkg took 3 s in the VM; a large vendor pkg can
/// take minutes.
const INSTALLER_BUDGET: Duration = Duration::from_secs(1800);

/// Artifact packaging kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Disk image containing one `.app` at its root.
    DmgApp,
    /// Zip archive containing one `.app` at its root.
    ZipApp,
    /// Flat installer package (`.pkg`).
    Pkg,
}

/// Pinned downloadable artifact.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Artifact `https://` download URL.
    pub url: String,
    /// Expected hex SHA-256 digest of the artifact.
    pub sha256: String,
    /// Expected Developer ID Team ID for code or installer signing.
    #[serde(default)]
    pub team_id: Option<String>,
    /// Whether the `.pkg` has no installer signature and is verified by hash and post-install binary Team ID.
    #[serde(default)]
    pub unsigned: bool,
}

/// Manifest `[[app]]` row.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// Short item name (`app.<name>`).
    pub name: String,
    /// Artifact packaging kind.
    pub kind: Kind,
    /// Artifact `https://` download URL.
    pub url: String,
    /// Expected hex SHA-256 digest.
    pub sha256: String,
    /// Expected Developer ID Team ID.
    #[serde(default)]
    pub team_id: Option<String>,
    /// Whether the `.pkg` is unsigned at the installer envelope level.
    #[serde(default)]
    pub unsigned: bool,
    /// Bundle name in `/Applications` for `dmg-app` and `zip-app` rows.
    #[serde(default)]
    pub bundle: Option<String>,
    /// Package receipt identifier for `pkgutil --pkg-info`.
    #[serde(default)]
    pub pkg_id: Option<String>,
    /// Installed binary path whose signature must match `team_id`.
    #[serde(default)]
    pub binary: Option<String>,
    /// Expected installed version (`CFBundleShortVersionString` or package receipt version).
    pub version: String,
}

impl Row {
    /// Extracts the downloadable [`Artifact`] descriptor for this row.
    #[must_use]
    pub fn artifact(&self) -> Artifact {
        Artifact {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            team_id: self.team_id.clone(),
            unsigned: self.unsigned,
        }
    }
}

/// Expands `[[app]]` manifest `rows` into executable [`Item`]s.
/// # Errors
/// Returns an error naming the invalid row if required fields or artifact constraints fail.
pub fn expand(rows: &[Row], ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let _ = ctx;
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    let mut owners: Vec<(String, &str)> = Vec::new();
    for row in rows {
        items.push(Box::new(App::new(row)?));
        let targets = [
            row.bundle.clone(),
            row.pkg_id.as_ref().map(|p| format!("receipt {p}")),
        ];
        for target in targets.into_iter().flatten() {
            // /Applications is case-insensitive: Emacs.app and emacs.app are one bundle.
            if let Some((_, other)) = owners.iter().find(|(t, _)| t.eq_ignore_ascii_case(&target)) {
                return Err(format!(
                    "[[app]] {}: {target} is already installed by [[app]] {other}; one row per bundle",
                    row.name
                ));
            }
            owners.push((target, &row.name));
        }
    }
    Ok(items)
}

/// One `[[app]]` row as an item.
struct App {
    row: Row,
    team: String,
    notes: Notes,
}

impl App {
    fn new(row: &Row) -> Result<Self, String> {
        let at = format!("[[app]] {}", row.name);
        if row.name.is_empty()
            || !row
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(format!(
                "{at}: name must be letters, digits, '-', '_' or '.'"
            ));
        }
        check_artifact(&at, &row.artifact())?;
        let team = row.team_id.clone().ok_or_else(|| {
            format!("{at}: team_id is required: the Team ID check is what an install trusts besides the hash")
        })?;
        if row.version.trim().is_empty() {
            return Err(format!("{at}: version is empty"));
        }
        match row.kind {
            Kind::DmgApp | Kind::ZipApp => {
                if row.unsigned {
                    return Err(format!("{at}: unsigned applies to a pkg only"));
                }
                let bundle = row
                    .bundle
                    .as_deref()
                    .ok_or_else(|| format!("{at}: a dmg-app or zip-app row names its bundle"))?;
                check_bundle_name(&at, bundle)?;
            }
            Kind::Pkg => {
                let id = row
                    .pkg_id
                    .as_deref()
                    .ok_or_else(|| format!("{at}: a pkg row names its pkg_id"))?;
                if id.is_empty() || id.starts_with('-') || id.contains(char::is_whitespace) {
                    return Err(format!("{at}: pkg_id {id:?} is not a receipt id"));
                }
                if let Some(bundle) = &row.bundle {
                    check_bundle_name(&at, bundle)?;
                }
                if let Some(binary) = &row.binary
                    && !binary.starts_with('/')
                {
                    return Err(format!("{at}: binary {binary:?} is not an absolute path"));
                }
                if row.bundle.is_none() && row.binary.is_none() {
                    return Err(format!(
                        "{at}: a pkg row names the bundle or the binary it installs, so the Team ID can be checked after the install"
                    ));
                }
            }
        }
        Ok(Self {
            row: row.clone(),
            team,
            notes: Notes::default(),
        })
    }

    /// `/Applications/<bundle>`, when the row names one.
    fn target(&self) -> Option<PathBuf> {
        self.row
            .bundle
            .as_ref()
            .map(|b| Path::new(APPLICATIONS).join(b))
    }

    fn wanted(&self) -> String {
        match &self.row.bundle {
            Some(b) => format!("{b} {} (team {})", self.row.version, self.team),
            None => format!(
                "{} {} (team {})",
                self.row.pkg_id.as_deref().unwrap_or(&self.row.name),
                self.row.version,
                self.team
            ),
        }
    }

    /// What is installed now, as one line, when it is not what the row wants; `Ok(None)`
    /// when it is.
    fn differs(&self, sys: &dyn System) -> Result<Option<String>, String> {
        match self.row.kind {
            Kind::DmgApp | Kind::ZipApp => {
                let target = self.target().unwrap_or_default();
                installed_bundle(sys, &target, &self.row.version, &self.team)
            }
            Kind::Pkg => self.pkg_differs(sys),
        }
    }

    fn pkg_differs(&self, sys: &dyn System) -> Result<Option<String>, String> {
        let id = self.row.pkg_id.as_deref().unwrap_or_default();
        let version = match receipt_version(sys, id) {
            Read::Value(v) => v,
            Read::Absent => return Ok(Some("absent (no receipt)".into())),
            Read::Unusable(why) => return Err(why),
        };
        if version != self.row.version {
            return Ok(Some(format!("{id} {version}")));
        }
        if let Some(target) = self.target()
            && let Some(now) = installed_bundle(sys, &target, &self.row.version, &self.team)?
        {
            return Ok(Some(format!("{id} {version}, but {now}")));
        }
        if let Some(binary) = &self.row.binary {
            match sys.stat(Path::new(binary)) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Some(format!("{id} {version}, but {binary} is missing")));
                }
                Err(e) => return Err(format!("{binary}: {e}")),
            }
            match verify_team(sys, Path::new(binary), &self.team) {
                Signed::Team => {}
                Signed::Other(why) => {
                    return Ok(Some(format!("{id} {version}, but {binary}: {why}")));
                }
                Signed::Unusable(why) => return Err(why),
            }
        }
        Ok(None)
    }

    fn apply_bundle(
        &self,
        sys: &dyn System,
        cached: &Path,
        stage: &Stage<'_>,
    ) -> Result<(), String> {
        let bundle = self.row.bundle.as_deref().unwrap_or_default();
        let new_dir = stage.join("new");
        sys.create_dir_all(&new_dir, 0o700)
            .map_err(|e| format!("{}: {e}", new_dir.display()))?;
        let staged = new_dir.join(bundle);
        match self.row.kind {
            Kind::DmgApp => with_image(sys, cached, stage, |mount| {
                let found = one_app_in(sys, mount)?;
                if found != bundle {
                    return Err(format!("the image holds {found}, the row names {bundle}"));
                }
                ditto_copy(sys, &mount.join(&found), &staged)
            })?,
            Kind::ZipApp => {
                ditto_unzip(sys, cached, &new_dir)?;
                let found = one_app_in(sys, &new_dir)?;
                if found != bundle {
                    return Err(format!("the zip holds {found}, the row names {bundle}"));
                }
            }
            Kind::Pkg => return Err("a pkg is not staged as a bundle".into()),
        }
        give_to(sys, &staged, 0, 0)?;
        vet_bundle(
            sys,
            &staged,
            &self.team,
            Some(&self.row.version),
            &self.notes,
        )?;
        swap_into(sys, &staged, &Path::new(APPLICATIONS).join(bundle), stage)
    }

    fn apply_pkg(&self, sys: &dyn System, cached: &Path, stage: &Path) -> Result<(), String> {
        let pkg = stage_pkg(sys, cached, stage, &self.row.artifact())?;
        vet_pkg(sys, &pkg, &self.row.artifact(), stage, &self.notes)?;
        run_installer(sys, &pkg)
    }
}

impl Item for App {
    fn id(&self) -> String {
        format!("app.{}", self.row.name)
    }

    fn family(&self) -> &'static str {
        "app"
    }

    fn title(&self) -> String {
        format!("{} {}", self.row.name, self.row.version)
    }

    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        let from = match self.differs(sys) {
            Ok(None) => return Probe::Ok { now: self.wanted() },
            Ok(Some(from)) => from,
            Err(why) => return Probe::Unusable { why },
        };
        if ctx.cached(&self.row.sha256).is_none() {
            return Probe::Blocked {
                why: not_cached(&self.row.sha256),
            };
        }
        if let Some(target) = self.target()
            && !ctx.disrupt
        {
            match running(sys, &target) {
                Read::Value(true) => {
                    return Probe::Deferred {
                        why: format!(
                            "{} is running; it is replaced when the Mac is idle, or with --now",
                            target.display()
                        ),
                    };
                }
                Read::Unusable(why) => return Probe::Unusable { why },
                Read::Value(false) | Read::Absent => {}
            }
        }
        Probe::Change {
            from,
            to: self.wanted(),
        }
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.notes.clear();
        let cached = cached_verified(sys, ctx, &self.row.sha256)?;
        let stage = fresh_stage(sys)?;
        let done = match self.row.kind {
            Kind::DmgApp | Kind::ZipApp => self.apply_bundle(sys, &cached, &stage),
            Kind::Pkg => self.apply_pkg(sys, &cached, &stage),
        };
        clear_stage(stage, done, &self.notes)
    }

    fn revert(&self) -> Option<String> {
        Some(match self.row.kind {
            Kind::DmgApp | Kind::ZipApp => format!(
                "none kept: the replaced bundle is removed after the swap; reinstall the earlier \
                 version from an [[app]] row that pins its artifact, or remove {} to uninstall",
                self.target().unwrap_or_default().display()
            ),
            Kind::Pkg => {
                let id = self.row.pkg_id.as_deref().unwrap_or_default();
                format!(
                    "none kept: a pkg has no uninstaller; remove the files `pkgutil --files {id}` \
                     lists, then `pkgutil --forget {id}`, and pin the earlier version's pkg to go back"
                )
            }
        })
    }

    fn effect(&self) -> Effect {
        Effect::Now
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.notes.joined()
    }
}

/// Collects non-fatal verification notes for the apply report.
#[derive(Debug, Default)]
pub(crate) struct Notes(Mutex<Vec<String>>);

impl Notes {
    /// Clears recorded notes.
    pub(crate) fn clear(&self) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// Appends a note.
    pub(crate) fn push(&self, note: String) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(note);
    }

    /// Joins recorded notes with `; `, or returns `None` if empty.
    pub(crate) fn joined(&self) -> Option<String> {
        let notes = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (!notes.is_empty()).then(|| notes.join("; "))
    }
}

/// Validates that `art` has an `https://` URL, hex SHA-256, and valid Team ID.
/// # Errors
/// Returns an error naming `at` if any field is malformed.
pub(crate) fn check_artifact(at: &str, art: &Artifact) -> Result<(), String> {
    if !art.url.starts_with("https://") {
        return Err(format!("{at}: url {:?} is not https://", art.url));
    }
    if !is_sha256(&art.sha256) {
        return Err(format!(
            "{at}: sha256 {:?} is not 64 hexadecimal digits",
            art.sha256
        ));
    }
    if let Some(team) = &art.team_id
        && !is_team_id(team)
    {
        return Err(format!(
            "{at}: team_id {team:?} is not a Team ID (ten capital letters and digits)"
        ));
    }
    Ok(())
}

fn check_bundle_name(at: &str, bundle: &str) -> Result<(), String> {
    let ok = bundle.len() > 4
        && bundle.ends_with(".app")
        && !bundle.contains('/')
        && !bundle.starts_with('.')
        && !bundle.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{at}: bundle {bundle:?} is not a bundle name like Emacs.app"
        ))
    }
}

/// Returns `true` if `s` is a 64-character hex SHA-256 digest.
#[must_use]
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Returns `true` if `s` is a 10-character alphanumeric Apple Team ID.
#[must_use]
pub fn is_team_id(s: &str) -> bool {
    s.len() == 10
        && s.bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

/// Formats the [`Probe::Blocked`] reason for a missing cached artifact.
pub(crate) fn not_cached(sha256: &str) -> String {
    format!(
        "artifact {} is not in the cache; the controller uploads it",
        &sha256[..sha256.len().min(12)]
    )
}

/// Resolves local user account `name` from baseline users or system facts.
pub(crate) fn account(ctx: &Ctx, name: &str) -> Option<User> {
    ctx.users
        .iter()
        .chain(&ctx.facts.users)
        .find(|u| u.name == name)
        .cloned()
}

/// Builds the Apple-anchored Developer ID code requirement string for `team`.
#[must_use]
pub fn developer_id_requirement(team: &str) -> String {
    format!(
        "anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and \
         certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate \
         leaf[subject.OU] = \"{team}\""
    )
}

/// `codesign --verify --deep --strict -R=<requirement> <path>`.
fn codesign_cmd(path: &Path, team: &str) -> Cmd {
    Cmd::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(format!("-R={}", developer_id_requirement(team)))
        .arg(path)
        .budget(VERIFY_BUDGET)
}

/// Result of verifying a bundle or binary with `codesign` against a Team ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signed {
    /// Verified as sealed and signed by the expected team.
    Team,
    /// Signed by another identity, unsigned, or modified.
    Other(String),
    /// `codesign` failed to produce a verdict.
    Unusable(String),
}

/// Verifies `path` with `codesign` against Developer ID `team`.
#[must_use]
pub fn verify_team(sys: &dyn System, path: &Path, team: &str) -> Signed {
    if !is_team_id(team) {
        return Signed::Other(format!("{team:?} is not a Team ID"));
    }
    match sys.run(&codesign_cmd(path, team)) {
        Ok(ran) => match ran.status {
            Some(0) => Signed::Team,
            Some(3) => Signed::Other(format!("signed, but not by Developer ID team {team}")),
            Some(1) if unreadable(path, &ran.err()) => {
                Signed::Unusable(format!("codesign {}: {}", path.display(), ran.describe()))
            }
            Some(1) => Signed::Other(format!("codesign: {}", first_line(&ran.err()))),
            _ => Signed::Unusable(format!("codesign {}: {}", path.display(), ran.describe())),
        },
        Err(e) => Signed::Unusable(format!("codesign {}: {e}", path.display())),
    }
}

/// Whether codesign's `stderr` says it could not open `path` itself: its first line is
/// `<path>: <errno text>` for a permission refusal or a missing path.
fn unreadable(path: &Path, stderr: &str) -> bool {
    let first = first_line(stderr);
    [
        "Permission denied",
        "Operation not permitted",
        "No such file or directory",
    ]
    .iter()
    .any(|why| first == format!("{}: {why}", path.display()))
}

/// Gatekeeper assessment mode (`spctl -a`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assess {
    /// Assesses an executable or bundle (`spctl -a -t exec`).
    Exec,
    /// Assesses an installer package (`spctl -a -t install`).
    Install,
}

/// Verdict returned by `spctl -a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gatekeeper {
    /// Assessed and accepted with the reported `source=`.
    Accepted(String),
    /// Exited 0 under an assessment override (such as Gatekeeper disabled).
    Overridden {
        /// Reported `override=` value.
        how: String,
        /// Reported `source=` value, if any.
        source: String,
    },
    /// Assessed and rejected (exit 3).
    Rejected(String),
    /// `spctl` did not produce a usable verdict.
    Unusable(String),
}

/// Runs Gatekeeper assessment (`spctl -a`) on `path`.
#[must_use]
pub fn assess(sys: &dyn System, path: &Path, kind: Assess) -> Gatekeeper {
    let t = match kind {
        Assess::Exec => "exec",
        Assess::Install => "install",
    };
    let cmd = Cmd::new("/usr/sbin/spctl")
        .args(["-a", "-vv", "-t", t])
        .arg(path)
        .budget(GATEKEEPER_BUDGET);
    let ran = match sys.run(&cmd) {
        Ok(ran) => ran,
        Err(e) => return Gatekeeper::Unusable(format!("spctl {}: {e}", path.display())),
    };
    let text = format!("{}\n{}", ran.err(), ran.out());
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).map(str::to_owned))
    };
    let source = field("source=").unwrap_or_default();
    match ran.status {
        Some(0) => match field("override=") {
            Some(how) => Gatekeeper::Overridden { how, source },
            None => Gatekeeper::Accepted(source),
        },
        Some(3) => Gatekeeper::Rejected(if source.is_empty() {
            first_line(&ran.err()).to_owned()
        } else {
            format!("source={source}")
        }),
        _ => Gatekeeper::Unusable(format!("spctl {}: {}", path.display(), ran.describe())),
    }
}

/// Require Gatekeeper's acceptance of `path`; an override is noted and passes, because
/// the Team ID check the caller also requires still vouches for the bytes.
fn require_gatekeeper(
    sys: &dyn System,
    path: &Path,
    kind: Assess,
    notes: &Notes,
) -> Result<(), String> {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    match assess(sys, path, kind) {
        Gatekeeper::Accepted(_) => Ok(()),
        Gatekeeper::Overridden { how, source } => {
            let source = if source.is_empty() {
                String::new()
            } else {
                format!(", source={source}")
            };
            notes.push(format!(
                "gatekeeper: not assessed for {name} (spctl: override={how}{source}); the Team ID check alone vouched for it"
            ));
            Ok(())
        }
        Gatekeeper::Rejected(why) => Err(format!(
            "Gatekeeper rejected {name} ({why}){}",
            app_store_only(sys)
        )),
        Gatekeeper::Unusable(why) => Err(why),
    }
}

/// Why a Mac rejects a notarized Developer ID app, when it is the Mac's own setting:
/// `spctl --status -v` prints `developer id disabled` where System Settings allows apps
/// from the App Store only (VM, macOS 27.0, after `spctl --global-enable`: every Developer
/// ID rule then reads disabled and syspolicyd logs `rejecting due to lack of matching
/// active rule`).
fn app_store_only(sys: &dyn System) -> &'static str {
    match sys.run(&Cmd::new("/usr/sbin/spctl").args(["--status", "-v"])) {
        Ok(ran) if format!("{}\n{}", ran.out(), ran.err()).contains("developer id disabled") => {
            "; this Mac allows apps from the App Store only (System Settings ▸ Privacy & \
             Security ▸ Allow applications from), so it refuses every Developer ID app"
        }
        _ => "",
    }
}

/// Reads `key` from `<bundle>/Contents/Info.plist` via `PlistBuddy`.
#[must_use]
pub fn bundle_info(sys: &dyn System, bundle: &Path, key: &str) -> Read<String> {
    let plist = bundle.join("Contents/Info.plist");
    match sys.stat(&plist) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read::Absent,
        Err(e) => return Read::Unusable(format!("{}: {e}", plist.display())),
    }
    let cmd = Cmd::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Print :{key}"))
        .arg(&plist);
    match sys.run(&cmd) {
        Ok(ran) if ran.success() => Read::Value(ran.out().trim().to_owned()),
        Ok(ran) if format!("{}{}", ran.out(), ran.err()).contains("Does Not Exist") => Read::Absent,
        Ok(ran) => Read::Unusable(format!(
            "PlistBuddy {}: {}",
            plist.display(),
            ran.describe()
        )),
        Err(e) => Read::Unusable(format!("PlistBuddy {}: {e}", plist.display())),
    }
}

/// What the bundle at `target` is, when it is not `version` signed by `team`; `Ok(None)`
/// when it is.
fn installed_bundle(
    sys: &dyn System,
    target: &Path,
    version: &str,
    team: &str,
) -> Result<Option<String>, String> {
    match sys.stat(target) {
        Ok(st) if st.dir && !st.symlink => {}
        Ok(_) => return Ok(Some(format!("{} is not a bundle", target.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Some("absent".into())),
        Err(e) => return Err(format!("{}: {e}", target.display())),
    }
    let have = match bundle_info(sys, target, "CFBundleShortVersionString") {
        Read::Value(v) => v,
        Read::Absent => "no version".into(),
        Read::Unusable(why) => return Err(why),
    };
    match verify_team(sys, target, team) {
        Signed::Team if have == version => Ok(None),
        Signed::Team => Ok(Some(have)),
        Signed::Other(why) => Ok(Some(format!("{have} ({why})"))),
        Signed::Unusable(why) => Err(why),
    }
}

/// Whether a process other than this one runs from `bundle` ([`running_in`]), read in this
/// process with no child ([`System::processes`]): no `pgrep -f`, which is a new program start that
/// can hang at macOS's program check and reads every process's arguments, held ones included
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` M29). `Read::Unusable` where the process table
/// cannot be read, so nothing is replaced under a process nobody could see.
#[must_use]
pub fn running(sys: &dyn System, bundle: &Path) -> Read<bool> {
    let Some(rows) = sys.processes() else {
        return Read::Unusable("the process table could not be read".into());
    };
    Read::Value(running_in(
        &rows,
        std::process::id(),
        &Programs::of(sys, bundle),
        |row| sys.process_argv(row),
    ))
}

/// What [`running_in`] judges a process table against: one bundle, as [`Programs::of`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Programs {
    /// The bundle's path as given and its canonical form where that differs, since the kernel
    /// names an executable in its on-disk spelling (`/private/var` for `/var`).
    roots: Vec<PathBuf>,
    /// The short names ([`Process::short_name`]) of the files in its `Contents/MacOS`: none where
    /// that folder could not be listed.
    names: Vec<String>,
    /// The short names a process interpreting one of those files that is a script runs under
    /// ([`interpreters`]): none where no file there is one.
    interpreters: Vec<String>,
}

impl Programs {
    /// `bundle`'s spellings, its programs' short names and those of their scripts' interpreters,
    /// its `Contents/MacOS` listed at its canonical spelling. Each program's first line is read at
    /// its canonical path, and only where that lies inside the bundle's canonical spelling,
    /// through no link ([`System::read_head`]): a program linked to a file elsewhere in the bundle
    /// (`Contents/MacOS/X -> ../Resources/run.sh`, which the kernel still runs as
    /// `<bundle>/Contents/MacOS/X`) is read at that file, and nothing outside the bundle is read,
    /// whether a program links out of it or a link replaces a file after its path was resolved.
    fn of(sys: &dyn System, bundle: &Path) -> Self {
        let real = sys.canonicalize(bundle).ok();
        let mut roots = vec![bundle.to_path_buf()];
        roots.extend(real.clone().filter(|real| real != bundle));
        let folder = real.as_deref().unwrap_or(bundle).join("Contents/MacOS");
        let files = sys.list_dir(&folder).unwrap_or_default();
        let mut interpreters: Vec<String> = files
            .iter()
            .filter_map(|name| sys.canonicalize(&folder.join(name)).ok())
            .filter(|program| real.as_ref().is_some_and(|real| program.starts_with(real)))
            .flat_map(|program| interpreters(sys, &program))
            .collect();
        interpreters.sort();
        interpreters.dedup();
        Self {
            roots,
            names: files.iter().map(|name| Process::short_name(name)).collect(),
            interpreters,
        }
    }
}

/// The longest first line [`interpreters`] reads of a program: the kernel's own bound on a
/// script's `#!` line (`IMG_SHSIZE`).
const SHEBANG_MAX: usize = 512;

/// The short names ([`Process::short_name`]) a process interpreting `program` runs under, where
/// its first line is `#!<interpreter> [<word>...]`; none for another file, or one that cannot be
/// read ([`System::read_head`]: a link, or no regular file). They are the interpreter's own name,
/// the program an `env` interpreter is asked for, and the name of the file an absolute
/// interpreter's path resolves to, since the kernel names a process after the file it execs, a
/// link's target (`<prefix>/bin/python3 -> python3.12` runs as `python3.12`, a link `qsh ->
/// /bin/bash` as `bash`): that resolution is the one the kernel makes at each start of the
/// program, and a name it adds can widen only which processes launchd started, with no terminal,
/// have their words read ([`running_in`]). Of each, for `sh` also the shell macOS's `/bin/sh` runs in its place under
/// that shell's own name (the one `/private/var/select/sh` links to), and for a name that starts
/// `python` also `Python`, the name a framework Python (Apple's, python.org's, Homebrew's) runs
/// under once it re-execs into its `Python.app`. Measured on macOS 27: a script whose line names
/// `/bin/sh` runs as `bash`, `/bin/zsh` as `zsh`, `/usr/bin/env perl` as `perl`, and
/// `/usr/bin/python3`, `/usr/bin/env python3` and Homebrew's `python3.13` as `Python`, each with
/// the script's path among its argument words.
fn interpreters(sys: &dyn System, program: &Path) -> Vec<String> {
    let Ok(head) = sys.read_head(program, SHEBANG_MAX) else {
        return Vec::new();
    };
    let Some(line) = head.strip_prefix(b"#!") else {
        return Vec::new();
    };
    let line = String::from_utf8_lossy(line.split(|b| *b == b'\n').next().unwrap_or_default());
    let mut words = line.split_ascii_whitespace();
    let mut runs: Vec<&str> = words.next().into_iter().collect();
    if runs.first().and_then(|first| Path::new(first).file_name()) == Some("env".as_ref()) {
        runs.extend(words.find(|word| !word.starts_with('-') && !word.contains('=')));
    }
    let mut names = Vec::new();
    let spelled = runs.into_iter().map(Path::new).flat_map(|run| {
        let real = run
            .is_absolute()
            .then(|| sys.canonicalize(run).ok())
            .flatten();
        [Some(run.to_path_buf()), real].into_iter().flatten()
    });
    for run in spelled {
        let Some(name) = run.file_name() else {
            continue;
        };
        let name = name.to_string_lossy();
        if name == "sh" {
            let shell = sys.canonicalize(Path::new("/private/var/select/sh")).ok();
            names.extend(
                shell
                    .as_deref()
                    .and_then(Path::file_name)
                    .map(|shell| Process::short_name(&shell.to_string_lossy())),
            );
        }
        if name.starts_with("python") {
            names.push("Python".to_owned());
        }
        names.push(Process::short_name(&name));
    }
    names
}

/// Whether a process in `rows` other than `me` runs from `bundle`. No ancestor of `me` is left
/// out: the aterm window whose tab runs this process still counts. A row counts when
/// - the kernel names its executable at or below a root: an app, a helper, a login item or an XPC
///   service in the bundle, a background job of it, and a command-line tool started by a link
///   to one (`~/.local/bin/aterm`), whether macOS's program check holds it or not;
/// - the kernel names no executable for it (its file was deleted) and its short name is one of
///   the bundle's `names`;
/// - its executable lies elsewhere, its short name is one of `names`, and `argv` reads its first
///   word as a path at or below a root: a program started from the bundle that an earlier
///   replacement moved aside, which still finds the bundle's files by that path;
/// - launchd is its parent ([`Process::LAUNCHD`]) and it has no controlling terminal
///   ([`Process::terminal`]), as an app LaunchServices opened, a launch agent or a login item
///   runs, its short name is one of the bundle's `interpreters`, and `argv` reads any of its words
///   as a path at or below a root: a script of the bundle started as an app, whose executable is
///   the interpreter's, outside the bundle.
///
/// `argv` is asked only of a row of one of `names`, or of one of `interpreters` (none for a
/// bundle with no script) that launchd is the parent of and that has no controlling terminal,
/// and only once no row counts by the first two rules, so a table with no such row reads no
/// process's arguments (M29). Its parent alone would not bound it: a terminal's shell that
/// outlived its terminal's restart (each aterm tab's that outlives an aterm) has launchd for its
/// parent too, so for a bundle with a `#!/bin/sh` program every such shell would be read, as many
/// as the Mac has tabs. No terminal's shell, whether its terminal or launchd is its parent, and no
/// shell's child forked and not yet exec'd (the exec a read races, §6.4) is read: a terminal's
/// shells and their forks keep its terminal, and any other shell's fork has that shell for its
/// parent. A script of the bundle that a shell runs is not counted by its words, though it counts
/// by its image once it execs a program of the bundle. A row `argv` reads nothing of (a held
/// program, another user's) does not count by its words.
fn running_in(
    rows: &[Process],
    me: u32,
    bundle: &Programs,
    mut argv: impl FnMut(&Process) -> Option<Vec<std::ffi::OsString>>,
) -> bool {
    let inside = |path: &Path| bundle.roots.iter().any(|root| path.starts_with(root));
    let named = |row: &Process| bundle.names.contains(&row.comm);
    let others = || rows.iter().filter(move |row| row.pid != me);
    others().any(|row| row.exe.as_deref().map_or_else(|| named(row), inside))
        || others().any(|row| {
            let reach = if row.ppid == Process::LAUNCHD
                && !row.terminal
                && bundle.interpreters.contains(&row.comm)
            {
                usize::MAX
            } else if named(row) {
                1
            } else {
                return false;
            };
            argv(row)
                .is_some_and(|words| words.iter().take(reach).any(|word| inside(Path::new(word))))
        })
}

/// Locates cached artifact `sha256` and re-verifies its SHA-256 digest.
/// # Errors
/// Returns an error if the artifact is missing, unreadable, or fails hash verification.
pub(crate) fn cached_verified(
    sys: &dyn System,
    ctx: &Ctx,
    sha256: &str,
) -> Result<PathBuf, String> {
    let path = ctx.cached(sha256).ok_or_else(|| not_cached(sha256))?;
    let got = sys
        .sha256(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if got.eq_ignore_ascii_case(sha256) {
        Ok(path)
    } else {
        Err(format!(
            "{} hashes to {got}, not its name; the controller uploads it again",
            path.display()
        ))
    }
}

/// Cleanup custody handle for an exclusively created staging directory.
pub(crate) struct Stage<'s> {
    path: PathBuf,
    scratch: Option<crate::port::Scratch<'s>>,
    retained: std::cell::Cell<bool>,
}

impl<'s> Stage<'s> {
    fn new(scratch: crate::port::Scratch<'s>) -> Self {
        Self {
            path: scratch.path().to_path_buf(),
            scratch: Some(scratch),
            retained: std::cell::Cell::new(false),
        }
    }
}

impl std::fmt::Debug for Stage<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stage")
            .field("path", &self.path)
            .field("retained", &self.retained.get())
            .finish_non_exhaustive()
    }
}

impl std::ops::Deref for Stage<'_> {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl Drop for Stage<'_> {
    fn drop(&mut self) {
        if self.retained.get()
            && let Some(scratch) = self.scratch.take()
        {
            let _ = scratch.keep();
        }
    }
}

/// Creates a fresh root-only staging directory under [`STAGE_ROOT`].
/// # Errors
/// Returns an error if directory creation or ownership/mode verification fails.
pub(crate) fn fresh_stage<'s>(sys: &'s dyn System) -> Result<Stage<'s>, String> {
    match sys.stat(Path::new(crate::AGENT_ROOT)) {
        Ok(st) if st.dir && !st.symlink => {}
        Ok(_) => return Err(format!("{} is not a directory", crate::AGENT_ROOT)),
        Err(e) => {
            return Err(format!(
                "{}: {e}; the agent is not installed here",
                crate::AGENT_ROOT
            ));
        }
    }
    let root = Path::new(STAGE_ROOT);
    match sys.stat(root) {
        Ok(st) if st.dir && !st.symlink && st.uid == 0 && st.mode & 0o022 == 0 => {}
        Ok(_) => {
            return Err(format!(
                "{} is not a root-owned, non-writable staging parent",
                root.display()
            ));
        }
        Err(e) => return Err(format!("{}: {e}", root.display())),
    }
    let scratch = crate::port::Scratch::new_in(sys, root, "install")
        .map_err(|e| format!("staging {}: {e}", root.display()))?;
    Ok(Stage::new(scratch))
}

/// Cleans up `stage`, or retains it if a displaced original bundle is still held.
/// # Errors
/// Returns an error if retaining a displaced bundle or removing scratch fails.
pub(crate) fn clear_stage(
    mut stage: Stage<'_>,
    done: Result<(), String>,
    notes: &Notes,
) -> Result<(), String> {
    let Some(mut scratch) = stage.scratch.take() else {
        return Err("the staging allocation is no longer held".into());
    };
    if stage.retained.get() {
        let path = scratch.keep();
        notes.push(format!("stage retained for recovery: {}", path.display()));
        return done.and_then(|()| Err(format!("a previous bundle remains at {}", path.display())));
    }
    let cleared = scratch.close();
    match (done, cleared) {
        (Err(why), _) => Err(why),
        (Ok(()), Err(why)) => {
            notes.push(format!("stage left behind ({why})"));
            Ok(())
        }
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// `ditto` of one bundle, with no ACL and no quarantine.
fn ditto_copy(sys: &dyn System, from: &Path, to: &Path) -> Result<(), String> {
    let ran = sys
        .run(
            &Cmd::new("/usr/bin/ditto")
                .args(["--noacl", "--noqtn"])
                .arg(from)
                .arg(to)
                .budget(STAGE_BUDGET),
        )
        .map_err(|e| format!("ditto: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("ditto {}: {}", from.display(), ran.describe()))
    }
}

/// Extracts `zip` into `into` using `ditto -x -k`.
/// # Errors
/// Returns an error if `ditto` fails.
pub(crate) fn ditto_unzip(sys: &dyn System, zip: &Path, into: &Path) -> Result<(), String> {
    let ran = sys
        .run(
            &Cmd::new("/usr/bin/ditto")
                .args(["-x", "-k", "--noacl", "--noqtn"])
                .arg(zip)
                .arg(into)
                .budget(STAGE_BUDGET),
        )
        .map_err(|e| format!("ditto -x: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("ditto -x {}: {}", zip.display(), ran.describe()))
    }
}

/// Returns the single `.app` directory name at the root of `dir`.
/// # Errors
/// Returns an error if `dir` does not contain exactly one safe `.app` bundle.
pub(crate) fn one_app_in(sys: &dyn System, dir: &Path) -> Result<String, String> {
    let names = sys
        .list_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut apps = Vec::new();
    for name in names {
        let is_app = Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("app"));
        if is_app
            && sys
                .stat(&dir.join(&name))
                .is_ok_and(|st| st.dir && !st.symlink)
        {
            apps.push(name);
        }
    }
    match apps.len() {
        1 => Ok(apps.remove(0)),
        n => Err(format!(
            "{} holds {n} .app bundles at its root, not one",
            dir.display()
        )),
    }
}

/// Every `mount-point` string in `hdiutil attach -plist`'s output.
fn mount_points(plist: &str) -> Vec<PathBuf> {
    const KEY: &str = "<key>mount-point</key>";
    let mut out = Vec::new();
    let mut rest = plist;
    while let Some(at) = rest.find(KEY) {
        rest = rest[at + KEY.len()..].trim_start();
        if let Some(body) = rest.strip_prefix("<string>")
            && let Some(end) = body.find("</string>")
        {
            out.push(PathBuf::from(xml_text(&body[..end])));
            rest = &body[end..];
        }
    }
    out
}

/// The text of an XML string element.
fn xml_text(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Attaches `image` read-only under `under`, runs `work` on the mount point, and detaches.
/// # Errors
/// Returns an error if attach, `work`, or detach fails.
pub(crate) fn with_image<T>(
    sys: &dyn System,
    image: &Path,
    under: &Path,
    work: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let attach = Cmd::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-readonly",
            "-nobrowse",
            "-noautoopen",
            "-mountrandom",
        ])
        .arg(under)
        .arg("-plist")
        .arg(image)
        .budget(STAGE_BUDGET);
    let ran = sys
        .run(&attach)
        .map_err(|e| format!("hdiutil attach: {e}"))?;
    let points = mount_points(&ran.out());
    if !ran.success() || points.is_empty() {
        for point in &points {
            let _ = detach(sys, point);
        }
        return Err(format!(
            "hdiutil attach {}: {}",
            image.display(),
            if ran.success() {
                "no mount point in its answer".to_owned()
            } else {
                ran.describe()
            }
        ));
    }
    if points.len() > 1 {
        for point in &points {
            let _ = detach(sys, point);
        }
        return Err(format!(
            "{} mounts {} volumes; an app image has one",
            image.display(),
            points.len()
        ));
    }
    let point = &points[0];
    let done = work(point);
    let detached = detach(sys, point);
    let value = done?;
    detached?;
    Ok(value)
}

/// `hdiutil detach <point>`, then `-force` if that is refused.
fn detach(sys: &dyn System, point: &Path) -> Result<(), String> {
    let plain = Cmd::new("/usr/bin/hdiutil").arg("detach").arg(point);
    if sys.run(&plain).is_ok_and(|r| r.success()) {
        return Ok(());
    }
    let forced = Cmd::new("/usr/bin/hdiutil")
        .args(["detach", "-force"])
        .arg(point);
    match sys.run(&forced) {
        Ok(ran) if ran.success() => Ok(()),
        Ok(ran) => Err(format!(
            "hdiutil detach {}: {}",
            point.display(),
            ran.describe()
        )),
        Err(e) => Err(format!("hdiutil detach {}: {e}", point.display())),
    }
}

/// Sets ownership of `path` to `uid:gid`, strips group/other write, and clears setuid/setgid bits.
/// # Errors
/// Returns an error if `chown` or `chmod` fails.
pub(crate) fn give_to(sys: &dyn System, path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    for cmd in [
        Cmd::new("/usr/sbin/chown")
            .arg("-R")
            .arg(format!("{uid}:{gid}"))
            .arg(path),
        Cmd::new("/bin/chmod").args(["-R", "go-w,ug-s"]).arg(path),
    ] {
        let ran = sys
            .run(&cmd.clone().budget(STAGE_BUDGET))
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
    }
    Ok(())
}

/// Verifies that `staged` matches `team`, passes Gatekeeper, and reports `version`.
/// # Errors
/// Returns an error if signature, Gatekeeper, or version verification fails.
pub(crate) fn vet_bundle(
    sys: &dyn System,
    staged: &Path,
    team: &str,
    version: Option<&str>,
    notes: &Notes,
) -> Result<(), String> {
    let name = staged
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match verify_team(sys, staged, team) {
        Signed::Team => {}
        Signed::Other(why) => return Err(format!("{name} from the artifact: {why}")),
        Signed::Unusable(why) => return Err(why),
    }
    require_gatekeeper(sys, staged, Assess::Exec, notes)?;
    if let Some(want) = version {
        match bundle_info(sys, staged, "CFBundleShortVersionString") {
            Read::Value(have) if have == want => {}
            Read::Value(have) => {
                return Err(format!(
                    "the artifact holds {name} {have}, the baseline pins {want}"
                ));
            }
            Read::Absent => return Err(format!("{name} from the artifact names no version")),
            Read::Unusable(why) => return Err(why),
        }
    }
    Ok(())
}

/// Swaps `staged` into `target`, displacing any existing bundle into `stage/old/`.
/// # Errors
/// Returns an error if moving the existing bundle or renaming `staged` into place fails.
pub(crate) fn swap_into(
    sys: &dyn System,
    staged: &Path,
    target: &Path,
    stage: &Stage<'_>,
) -> Result<(), String> {
    let name = target
        .file_name()
        .ok_or_else(|| format!("{} has no name", target.display()))?;
    let aside = stage.join("old").join(name);
    let moved = match sys.stat(target) {
        Ok(_) => {
            let old_dir = stage.join("old");
            sys.create_dir_all(&old_dir, 0o700)
                .map_err(|e| format!("{}: {e}", old_dir.display()))?;
            stage.retained.set(true);
            if let Err(e) = sys.rename(target, &aside) {
                stage.retained.set(false);
                return Err(format!("moving {} aside: {e}", target.display()));
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(format!("{}: {e}", target.display())),
    };
    if let Err(e) = sys.rename(staged, target) {
        let back = if moved {
            match sys.rename(&aside, target) {
                Ok(()) => {
                    stage.retained.set(false);
                    "; the previous bundle is back in place".to_owned()
                }
                Err(b) => format!("; the previous bundle is at {} ({b})", aside.display()),
            }
        } else {
            String::new()
        };
        return Err(format!(
            "moving the new bundle to {}: {e}{back}",
            target.display()
        ));
    }
    stage.retained.set(false);
    Ok(())
}

/// Reads the installed version from `pkg_id`'s receipt via `pkgutil --pkg-info`.
#[must_use]
pub fn receipt_version(sys: &dyn System, pkg_id: &str) -> Read<String> {
    let cmd = Cmd::new("/usr/sbin/pkgutil").arg("--pkg-info").arg(pkg_id);
    match sys.run(&cmd) {
        Ok(ran) if ran.success() => ran
            .out()
            .lines()
            .find_map(|l| l.strip_prefix("version: ").map(|v| v.trim().to_owned()))
            .map_or_else(
                || Read::Unusable(format!("pkgutil --pkg-info {pkg_id}: no version line")),
                Read::Value,
            ),
        Ok(ran) if ran.status == Some(1) && ran.err().starts_with("No receipt for") => Read::Absent,
        Ok(ran) => Read::Unusable(format!("pkgutil --pkg-info {pkg_id}: {}", ran.describe())),
        Err(e) => Read::Unusable(format!("pkgutil --pkg-info {pkg_id}: {e}")),
    }
}

/// The start of the `Status:` line `pkgutil --check-signature` prints for a pkg signed
/// through a Developer ID chain it trusts (Homebrew.pkg, VM).
const DEVELOPER_ID_STATUS: &str =
    "signed by a developer certificate issued by Apple for distribution";

/// Parsed `pkgutil --check-signature` verdict for a `.pkg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PkgSignature {
    /// Signed package with trust status, leaf certificate, Team ID, and notarization state.
    Signed {
        /// Reported `Status:` line.
        status: String,
        /// Leaf certificate common name.
        leaf: String,
        /// Extracted Team ID.
        team: String,
        /// Whether notarization is trusted.
        notarized: bool,
    },
    /// Package has no signature (`Status: no signature`).
    Unsigned,
    /// `pkgutil` output could not be parsed.
    Unusable(String),
}

/// Parses `pkgutil --check-signature` exit status and stdout.
#[must_use]
pub fn parse_pkg_signature(status: Option<i32>, text: &str) -> PkgSignature {
    let line = |prefix: &str| {
        text.lines()
            .map(str::trim)
            .find_map(|l| l.strip_prefix(prefix).map(str::trim))
    };
    if line("Status:") == Some("no signature") {
        return PkgSignature::Unsigned;
    }
    if status != Some(0) {
        return PkgSignature::Unusable(format!(
            "pkgutil --check-signature: exit {}: {}",
            status.map_or_else(|| "by a signal".into(), |s| s.to_string()),
            line("Status:").unwrap_or("no status")
        ));
    }
    let Some(leaf) = line("1. ") else {
        return PkgSignature::Unusable("pkgutil --check-signature: no certificate chain".into());
    };
    let team = leaf
        .rsplit_once('(')
        .and_then(|(_, t)| t.strip_suffix(')'))
        .unwrap_or_default()
        .to_owned();
    PkgSignature::Signed {
        status: line("Status:").unwrap_or_default().to_owned(),
        leaf: leaf.to_owned(),
        team,
        notarized: line("Notarization:").is_some_and(|n| n.starts_with("trusted")),
    }
}

/// Returns `true` if `head` starts with a thin or universal Mach-O magic number.
#[must_use]
pub fn is_macho(head: &[u8]) -> bool {
    let Some(magic) = head.get(..4) else {
        return false;
    };
    match magic {
        [0xfe, 0xed, 0xfa, 0xce | 0xcf] | [0xce | 0xcf, 0xfa, 0xed, 0xfe] => true,
        [0xca, 0xfe, 0xba, 0xbe | 0xbf] => head
            .get(4..8)
            .is_some_and(|n| u32::from_be_bytes([n[0], n[1], n[2], n[3]]) < 20),
        _ => false,
    }
}

/// Copies the cached `.pkg` into `stage` under its URL filename.
/// # Errors
/// Returns an error if the URL filename is invalid or copying fails.
pub(crate) fn stage_pkg(
    sys: &dyn System,
    cached: &Path,
    stage: &Path,
    art: &Artifact,
) -> Result<PathBuf, String> {
    let from_url = art
        .url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split(['?', '#'])
        .next()
        .unwrap_or_default();
    let safe = from_url.len() > 4
        && from_url.ends_with(".pkg")
        && !from_url.starts_with('.')
        && from_url
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'));
    let name = if safe { from_url } else { "artifact.pkg" };
    let pkg = stage.join(name);
    sys.copy_file(cached, &pkg, 0o600)
        .map_err(|e| format!("copying {} into the stage: {e}", cached.display()))?;
    let got = sys
        .sha256(&pkg)
        .map_err(|e| format!("{}: {e}", pkg.display()))?;
    if got.eq_ignore_ascii_case(&art.sha256) {
        Ok(pkg)
    } else {
        Err(format!(
            "the staged {} hashes to {got}, not {}",
            pkg.display(),
            art.sha256
        ))
    }
}

/// Vets staged package `pkg` against `art` before running `installer`.
/// # Errors
/// Returns an error if signature, Gatekeeper, or unsigned-package checks fail.
pub(crate) fn vet_pkg(
    sys: &dyn System,
    pkg: &Path,
    art: &Artifact,
    stage: &Path,
    notes: &Notes,
) -> Result<(), String> {
    let team = art
        .team_id
        .as_deref()
        .ok_or("the pkg has no team_id to check")?;
    if art.unsigned {
        return vet_unsigned_pkg(sys, pkg, team, stage, notes);
    }
    let ran = sys
        .run(
            &Cmd::new("/usr/sbin/pkgutil")
                .arg("--check-signature")
                .arg(pkg),
        )
        .map_err(|e| format!("pkgutil --check-signature: {e}"))?;
    match parse_pkg_signature(ran.status, &ran.out()) {
        PkgSignature::Signed {
            status,
            leaf,
            team: signer,
            notarized,
        } => {
            if !leaf.starts_with("Developer ID Installer:") || signer != team {
                return Err(format!(
                    "the pkg is signed by {leaf}, not a Developer ID Installer of team {team}"
                ));
            }
            if !status.starts_with(DEVELOPER_ID_STATUS) {
                return Err(format!(
                    "pkgutil does not trust the pkg's signature as Developer ID (Status: {status})"
                ));
            }
            if !notarized {
                notes.push(format!("pkgutil: {leaf} is not notarized"));
            }
        }
        PkgSignature::Unsigned => {
            return Err(
                "the pkg carries no installer signature (a row that expects that says unsigned = true)"
                    .into(),
            );
        }
        PkgSignature::Unusable(why) => return Err(why),
    }
    require_gatekeeper(sys, pkg, Assess::Install, notes)
}

/// [`vet_pkg`] for a pkg with no installer signature: no scripts, a Distribution whose
/// JavaScript cannot run programs, and at least one Mach-O, every one the team's.
fn vet_unsigned_pkg(
    sys: &dyn System,
    pkg: &Path,
    team: &str,
    stage: &Path,
    notes: &Notes,
) -> Result<(), String> {
    let expanded = stage.join("expanded");
    let ran = sys
        .run(
            &Cmd::new("/usr/sbin/pkgutil")
                .arg("--expand-full")
                .arg(pkg)
                .arg(&expanded)
                .budget(STAGE_BUDGET),
        )
        .map_err(|e| format!("pkgutil --expand-full: {e}"))?;
    if !ran.success() {
        return Err(format!("pkgutil --expand-full: {}", ran.describe()));
    }
    let listed = sys
        .run_complete_stdout(
            &Cmd::new("/usr/bin/find")
                .arg(&expanded)
                .args(["-type", "f", "-print0"])
                .budget(STAGE_BUDGET),
        )
        .map_err(|e| format!("find: {e}"))?;
    if !listed.success() {
        return Err(format!(
            "find {}: {}",
            expanded.display(),
            listed.describe()
        ));
    }
    let mut machos = 0_usize;
    for file in listed.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
        let path = PathBuf::from(std::str::from_utf8(file).map_err(|_| {
            "find printed a non-UTF-8 path; the inventory cannot be admitted".to_owned()
        })?);
        let rel = path.strip_prefix(&expanded).unwrap_or(&path);
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.first().is_some_and(|p| p == "Scripts")
            || parts
                .windows(2)
                .any(|w| w[0].ends_with(".pkg") && w[1] == "Scripts")
        {
            return Err(format!(
                "the unsigned pkg carries installer scripts ({}), which would run as root with no signature",
                rel.display()
            ));
        }
        let bytes = sys
            .read_file(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if parts.len() == 1 && parts[0] == "Distribution" {
            vet_distribution(&bytes)?;
        }
        if !is_macho(&bytes) {
            continue;
        }
        machos += 1;
        match verify_team(sys, &path, team) {
            Signed::Team => {}
            Signed::Other(why) => return Err(format!("{} in the pkg: {why}", rel.display())),
            Signed::Unusable(why) => return Err(why),
        }
    }
    if machos == 0 {
        return Err(format!(
            "the unsigned pkg holds no Mach-O, so nothing in it carries team {team}"
        ));
    }
    notes.push(format!(
        "unsigned pkg: no installer signature to assess; {machos} Mach-O file(s) verified as team {team}"
    ));
    Ok(())
}

/// Refuse an unsigned pkg's `Distribution` that allows external scripts
/// ([`allows_external_scripts`]), or that this scan cannot read as Installer's XML parser reads
/// it: bytes that are not UTF-8 (a UTF-16 document, whose attribute names are not the scanned
/// bytes), a NUL, a DTD (an entity or an `ATTLIST` default supplies an attribute the text never
/// spells), or an XML declaration naming another encoding.
fn vet_distribution(bytes: &[u8]) -> Result<(), String> {
    let unread = |what: &str| {
        format!("the unsigned pkg's Distribution {what}, so its installer options cannot be vetted")
    };
    let Ok(xml) = std::str::from_utf8(bytes) else {
        return Err(unread("is not UTF-8"));
    };
    if xml.contains('\0') {
        return Err(unread("holds a NUL byte"));
    }
    let lower = xml.to_ascii_lowercase();
    if ["<!doctype", "<!entity", "<!attlist"]
        .iter()
        .any(|dtd| lower.contains(dtd))
    {
        return Err(unread("declares a DTD"));
    }
    if let Some(why) = foreign_encoding(&lower) {
        return Err(unread(&why));
    }
    if allows_external_scripts(xml) {
        return Err(
            "the unsigned pkg's Distribution allows external scripts, so its JavaScript could run programs as root with no signature"
                .into(),
        );
    }
    Ok(())
}

/// What makes an XML declaration in `lower` (the document, ASCII-lowercased) name anything but
/// UTF-8: an unterminated `<?xml`, an `encoding` without a quoted value, or another encoding.
fn foreign_encoding(lower: &str) -> Option<String> {
    let mut rest = lower;
    while let Some(at) = rest.find("<?xml") {
        rest = &rest[at + "<?xml".len()..];
        let Some(end) = rest.find("?>") else {
            return Some("has an unterminated XML declaration".into());
        };
        let declaration = &rest[..end];
        rest = &rest[end..];
        let mut from = 0;
        while let Some(found) = declaration[from..].find("encoding") {
            let at = from + found;
            from = at + "encoding".len();
            let spaced = at
                .checked_sub(1)
                .and_then(|before| declaration.as_bytes().get(before))
                .is_some_and(u8::is_ascii_whitespace);
            if !spaced {
                continue;
            }
            let value = declaration[from..]
                .trim_start()
                .strip_prefix('=')
                .map(str::trim_start)
                .and_then(|v| {
                    let quote = v.chars().next().filter(|c| matches!(c, '"' | '\''))?;
                    let body = &v[1..];
                    body.find(quote).map(|close| &body[..close])
                });
            match value {
                Some(name) if name.trim() == "utf-8" => {}
                Some(name) => return Some(format!("declares the encoding {:?}", name.trim())),
                None => return Some("has an XML declaration whose encoding is unread".into()),
            }
        }
    }
    None
}

/// Returns `true` if a product archive `Distribution` XML permits external scripts (`system.run`).
#[must_use]
pub fn allows_external_scripts(distribution: &str) -> bool {
    const KEY: &str = "allow-external-scripts";
    let mut rest = distribution;
    while let Some(at) = rest.find(KEY) {
        rest = &rest[at + KEY.len()..];
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            return true;
        };
        let value = value.trim_start();
        let Some(quote) = value.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            return true;
        };
        let body = &value[1..];
        let Some(end) = body.find(quote) else {
            return true;
        };
        if !matches!(
            body[..end].trim().to_ascii_lowercase().as_str(),
            "false" | "no"
        ) {
            return true;
        }
        rest = &body[end..];
    }
    false
}

/// Runs `installer -pkg <pkg> -target /`.
/// # Errors
/// Returns an error if `installer` fails.
pub(crate) fn run_installer(sys: &dyn System, pkg: &Path) -> Result<(), String> {
    let cmd = Cmd::new("/usr/sbin/installer")
        .arg("-pkg")
        .arg(pkg)
        .args(["-target", "/"])
        .budget(INSTALLER_BUDGET);
    let ran = sys.run(&cmd).map_err(|e| format!("installer: {e}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("installer: {}{}", ran.describe(), last_words(&ran)))
    }
}

/// Formats the last non-empty stdout line of `ran` as a `: <line>` suffix.
pub(crate) fn last_words(ran: &Ran) -> String {
    ran.out()
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| format!(" ({l})"))
        .unwrap_or_default()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
#[path = "apps_tests.rs"]
pub(crate) mod tests;
