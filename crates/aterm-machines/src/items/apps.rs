// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Installs pinned `.dmg`, `.zip`, and `.pkg` application artifacts verified by SHA-256, Team ID, and Gatekeeper.

use super::{Ctx, Item};
use crate::model::{Effect, Probe, Read, User};
use crate::port::{Cmd, Ran, System};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Target directory where application bundles are installed.
pub const APPLICATIONS: &str = "/Applications";

/// Staging root directory (`/Library/Management/aterm/stage`).
pub const STAGE_ROOT: &str = "/Library/Management/aterm/stage";

/// The bound on one `codesign --verify --deep` run: a hang detector, not a latency budget
const VERIFY_BUDGET: Duration = Duration::from_secs(180);

/// The bound on one `spctl -a`. Also a hang detector, and longer: an app with no stapled
const GATEKEEPER_BUDGET: Duration = Duration::from_secs(900);

/// The bound on `ditto`, `hdiutil` and `pkgutil --expand-full` over one artifact.
const STAGE_BUDGET: Duration = Duration::from_secs(900);

/// The bound on `installer -pkg`. Homebrew.pkg took 3 s in the VM; a large vendor pkg can
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
    //") {
    if !art.url.starts_with("https://") {
        //", art.url));
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

/// `s` with every extended-regex metacharacter escaped, for `pgrep -f`.
fn regex_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.[]()*+?{}|^$".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Returns whether any process is running an executable inside `bundle`.
#[must_use]
pub fn running(sys: &dyn System, bundle: &Path) -> Read<bool> {
    let pattern = regex_literal(&format!("{}/Contents/MacOS/", bundle.display()));
    match sys.run(&Cmd::new("/usr/bin/pgrep").arg("-f").arg(pattern)) {
        Ok(ran) if ran.status == Some(0) => Read::Value(true),
        Ok(ran) if ran.status == Some(1) => Read::Value(false),
        Ok(ran) => Read::Unusable(format!("pgrep: {}", ran.describe())),
        Err(e) => Read::Unusable(format!("pgrep: {e}")),
    }
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
        if parts.len() == 1
            && parts[0] == "Distribution"
            && allows_external_scripts(&String::from_utf8_lossy(&bytes))
        {
            return Err(
                "the unsigned pkg's Distribution allows external scripts, so its JavaScript could run programs as root with no signature"
                    .into(),
            );
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
pub(crate) mod tests {
    use super::*;
    use crate::model::{HostFacts, Outcome};
    use crate::port::{Fake, Ran, Stat};

    /// Sample `hdiutil attach -plist` output for `Caffeine.dmg`.
    pub(crate) const ATTACH_CAFFEINE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>system-entities</key>
	<array>
		<dict>
			<key>content-hint</key>
			<string>Apple_HFS</string>
			<key>dev-entry</key>
			<string>/dev/disk6s1</string>
			<key>mount-point</key>
			<string>/private/tmp/swteam/stage/dmg.9CxWxv</string>
			<key>potentially-mountable</key>
			<true/>
			<key>unmapped-content-hint</key>
			<string>48465300-0000-11AA-AA11-00306543ECAC</string>
			<key>volume-kind</key>
			<string>hfs</string>
		</dict>
		<dict>
			<key>content-hint</key>
			<string>GUID_partition_scheme</string>
			<key>dev-entry</key>
			<string>/dev/disk6</string>
			<key>potentially-mountable</key>
			<false/>
			<key>unmapped-content-hint</key>
			<string>GUID_partition_scheme</string>
		</dict>
	</array>
</dict>
</plist>
"#;

    /// `spctl -a -vv -t exec` on the mounted Caffeine.app, VM (Gatekeeper off), stderr.
    const SPCTL_CAFFEINE_OFF: &str = "/private/tmp/swteam/stage/dmg.9CxWxv/Caffeine.app: accepted\nsource=Notarized Developer ID\noverride=security disabled\norigin=Developer ID Application: Michael Jones (YD6LEYT6WZ)\n";

    /// The same check on an ad hoc signed app, VM (Gatekeeper off): accepted all the same.
    const SPCTL_ADHOC_OFF: &str =
        "/private/tmp/swteam/adhoc/X.app: accepted\noverride=security disabled\n";

    /// Sample `spctl -a -vv -t exec` output for `KeepingYouAwake.app` with Gatekeeper enabled.
    pub(crate) const SPCTL_KYA_ON: &str = "kya/KeepingYouAwake.app: accepted\nsource=Notarized Developer ID\norigin=Developer ID Application: Marcel Dierkes (5KESHV9W85)\n";

    /// `spctl -a -vv -t exec` on an ad hoc signed app, this Mac (Gatekeeper on): exit 3.
    const SPCTL_ADHOC_ON: &str = "/private/tmp/sw-adhoc/X.app: rejected\n";

    /// Sample `codesign --verify` stderr on Team ID mismatch.
    pub(crate) const CODESIGN_WRONG_TEAM: &str =
        "test-requirement: code failed to satisfy specified code requirement(s)\n";

    /// `codesign --verify` on an unsigned binary, VM: exit 1, stderr.
    const CODESIGN_UNSIGNED: &str =
        "./unsigned-ls: code object is not signed at all\nIn architecture: arm64e.x1\n";

    /// Sample `pkgutil --check-signature` stdout for `Homebrew-7.0.9.pkg`.
    pub(crate) const PKGUTIL_HOMEBREW: &str = r#"Package "Homebrew-7.0.9.pkg":
   Status: signed by a developer certificate issued by Apple for distribution
   Notarization: trusted by the Apple notary service
   Signed with a trusted timestamp on: 2026-10-08 10:47:18 +0000
   Certificate Chain:
    1. Developer ID Installer: Patrick Linnane (927JGANW46)
       Expires: 2029-07-02 15:50:33 +0000
       SHA256 Fingerprint:
           3C 5D AA 66 55 A8 89 1F C1 E0 C0 60 8A EB 6B 3D 06 54 9E 05 A6 7C
           1D C9 03 CE A8 D5 21 20 AA C0
       ------------------------------------------------------------------------
    2. Developer ID Certification Authority
       Expires: 2031-09-17 00:00:00 +0000
       SHA256 Fingerprint:
           F1 6C D3 C5 4C 7F 83 CE A4 BF 1A 3E 6A 08 19 C8 AA A8 E4 A1 52 8F
           D1 44 71 5F 35 06 43 D2 DF 3A
       ------------------------------------------------------------------------
    3. Apple Root CA
       Expires: 2035-02-09 21:40:36 +0000
       SHA256 Fingerprint:
           B0 B1 73 0E CB C7 FF 45 05 14 2C 49 F1 29 5E 6E DA 6B CA ED 7E 2C
           68 C5 BE 91 B5 A1 10 01 F0 24

"#;

    /// `pkgutil --check-signature gh_2.102.0_macOS_universal.pkg`, VM: exit 1, stdout.
    const PKGUTIL_GH: &str =
        "Package \"gh_2.102.0_macOS_universal.pkg\":\n   Status: no signature\n";

    /// Sample `spctl -a -vv -t install` stderr for `Homebrew-7.0.9.pkg` with Gatekeeper disabled.
    pub(crate) const SPCTL_HOMEBREW_OFF: &str = "Homebrew-7.0.9.pkg: accepted\nsource=Notarized Developer ID\noverride=security disabled\norigin=Developer ID Installer: Patrick Linnane (927JGANW46)\n";

    /// `installer -pkg gh_2.102.0_macOS_universal.pkg -target /`, VM: exit 0, stdout.
    const INSTALLER_GH: &str = "installer: Package name is GitHub CLI\ninstaller: Installing at base path /\ninstaller: The install was successful.\n";

    /// `pkgutil --pkg-info com.github.cli` after that install, VM.
    const PKGINFO_GH: &str = "package-id: com.github.cli\nversion: 2.102.0\nvolume: /\nlocation: \ninstall-time: 1792192481\n";

    /// `pkgutil --pkg-info com.github.cli` before it, VM: exit 1, stderr.
    const PKGINFO_ABSENT: &str = "No receipt for 'com.github.cli' found at '/'.\n";

    /// `PlistBuddy -c 'Print :NoSuchKey' …/Info.plist`, VM: exit 1.
    const PLISTBUDDY_NO_KEY: &str = "Print: Entry, \":NoSuchKey\", Does Not Exist\n";

    pub(crate) const CAFFEINE_SHA: &str =
        "1ad34c3299a0c866873c4feb432c3e11186375adb6f43c5f481ae9d46d06d723";
    const KYA_SHA: &str = "8001a149b4490c008fdac19898bce9902d516c4aa6412a7eb0f9a37443b15c6b";
    const GH_SHA: &str = "1787e65f36626245a6b3cee353a458f6ea4372008548ceb8fb5d5dfb55b006d0";

    pub(crate) fn ok(out: &str) -> Ran {
        Ran::ok(out)
    }

    pub(crate) fn err(status: i32, stderr: &str) -> Ran {
        Ran::exit(status, stderr)
    }

    pub(crate) fn out_status(status: i32, stdout: &str) -> Ran {
        Ran {
            status: Some(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    pub(crate) fn dir() -> Stat {
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        }
    }

    pub(crate) fn file() -> Stat {
        Stat {
            dir: false,
            mode: 0o644,
            ..dir()
        }
    }

    pub(crate) fn admin() -> User {
        User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }
    }

    /// Returns the staging directory path for `sequence` in the fake backend.
    pub(crate) fn stage_path(sequence: u64) -> String {
        Fake::scratch_path(Some(Path::new(STAGE_ROOT)), "install", sequence)
            .to_string_lossy()
            .into_owned()
    }

    /// Builds a test [`Ctx`] with `cached` artifacts seeded in `sys`.
    pub(crate) fn ctx_with(sys: &Fake, cached: &[(&str, &[u8])], disrupt: bool) -> Ctx {
        let mut facts = HostFacts {
            arch: "arm64".into(),
            ..HostFacts::default()
        };
        for (sha, bytes) in cached {
            sys.file(&format!("/c/{sha}"), bytes);
            facts.cached.push((*sha).to_owned());
        }
        sys.stat_of(crate::AGENT_ROOT, dir()).stat_of(
            STAGE_ROOT,
            Stat {
                mode: 0o700,
                ..dir()
            },
        );
        Ctx {
            facts,
            users: vec![admin()],
            console: Some(admin()),
            host_name: "lab-07".into(),
            cache_dir: "/c".into(),
            disrupt,
        }
    }

    /// Returns the hex SHA-256 digest of `bytes`.
    pub(crate) fn sha_of(bytes: &[u8]) -> String {
        let mut h = aterm_digest::Sha256::new();
        h.update(bytes);
        aterm_codec::hex::encode_lower(&h.finalize())
    }

    pub(crate) fn codesign_key(path: &str, team: &str) -> String {
        codesign_cmd(Path::new(path), team).display()
    }

    fn caffeine_row(sha: &str) -> Row {
        Row {
            name: "Caffeine".into(),
            kind: Kind::DmgApp,
            url: "https://github.com/IntelliScape/caffeine/releases/download/1.1.4/Caffeine.dmg"
                .into(),
            sha256: sha.into(),
            team_id: Some("YD6LEYT6WZ".into()),
            unsigned: false,
            bundle: Some("Caffeine.app".into()),
            pkg_id: None,
            binary: None,
            version: "1.1.4".into(),
        }
    }

    fn kya_row(sha: &str) -> Row {
        Row {
            name: "KeepingYouAwake".into(),
            kind: Kind::ZipApp,
            url: "https://github.com/newmarcel/KeepingYouAwake/releases/download/1.6.8/KeepingYouAwake-1.6.8.zip".into(),
            sha256: sha.into(),
            team_id: Some("5KESHV9W85".into()),
            unsigned: false,
            bundle: Some("KeepingYouAwake.app".into()),
            pkg_id: None,
            binary: None,
            version: "1.6.8".into(),
        }
    }

    fn gh_row(sha: &str) -> Row {
        Row {
            name: "gh".into(),
            kind: Kind::Pkg,
            url: "https://github.com/cli/cli/releases/download/v2.102.0/gh_2.102.0_macOS_universal.pkg".into(),
            sha256: sha.into(),
            team_id: Some("VEKTX9H2N7".into()),
            unsigned: true,
            bundle: None,
            pkg_id: Some("com.github.cli".into()),
            binary: Some("/usr/local/bin/gh".into()),
            version: "2.102.0".into(),
        }
    }

    fn plistbuddy(bundle: &str, key: &str) -> String {
        format!("/usr/libexec/PlistBuddy -c 'Print :{key}' {bundle}/Contents/Info.plist")
    }

    fn one(row: &Row, sys: &Fake, ctx: &Ctx, mode: crate::model::Mode) -> crate::model::ItemReport {
        let items = expand(std::slice::from_ref(row), ctx).unwrap();
        crate::engine::run(&items, mode, sys, ctx, &mut crate::engine::NoJournal)
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn the_team_requirement_is_install_sh_s_developer_id_chain() {
        assert_eq!(
            codesign_key("/Applications/Caffeine.app", "YD6LEYT6WZ"),
            "/usr/bin/codesign --verify --deep --strict '-R=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"YD6LEYT6WZ\"' /Applications/Caffeine.app"
        );
    }

    #[test]
    fn codesign_exit_3_is_another_team_and_exit_1_is_unsigned() {
        let sys = Fake::root();
        let key = codesign_key("/x", "VEKTX9H2N7");
        sys.on(&key, ok(""));
        assert_eq!(
            verify_team(&sys, Path::new("/x"), "VEKTX9H2N7"),
            Signed::Team
        );
        let sys = Fake::root();
        sys.on(&key, err(3, CODESIGN_WRONG_TEAM));
        assert!(
            matches!(verify_team(&sys, Path::new("/x"), "VEKTX9H2N7"), Signed::Other(w) if w.contains("not by Developer ID team VEKTX9H2N7"))
        );
        let sys = Fake::root();
        sys.on(&key, err(1, CODESIGN_UNSIGNED));
        assert!(
            matches!(verify_team(&sys, Path::new("/x"), "VEKTX9H2N7"), Signed::Other(w) if w.contains("not signed at all"))
        );
        let sys = Fake::root();
        assert!(matches!(
            verify_team(&sys, Path::new("/x"), "VEKTX9H2N7"),
            Signed::Unusable(_)
        ));
    }

    #[test]
    fn a_malformed_team_never_reaches_a_code_requirement() {
        let sys = Fake::root();
        assert!(matches!(
            verify_team(&sys, Path::new("/x"), "X\" or true"),
            Signed::Other(_)
        ));
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn gatekeeper_off_is_an_override_not_a_pass_even_for_an_ad_hoc_app() {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/spctl -a -vv -t exec /a",
            err(0, SPCTL_CAFFEINE_OFF),
        );
        sys.on("/usr/sbin/spctl -a -vv -t exec /b", err(0, SPCTL_ADHOC_OFF));
        sys.on("/usr/sbin/spctl -a -vv -t exec /c", err(0, SPCTL_KYA_ON));
        sys.on("/usr/sbin/spctl -a -vv -t exec /d", err(3, SPCTL_ADHOC_ON));
        assert_eq!(
            assess(&sys, Path::new("/a"), Assess::Exec),
            Gatekeeper::Overridden {
                how: "security disabled".into(),
                source: "Notarized Developer ID".into()
            }
        );
        assert_eq!(
            assess(&sys, Path::new("/b"), Assess::Exec),
            Gatekeeper::Overridden {
                how: "security disabled".into(),
                source: String::new()
            }
        );
        assert_eq!(
            assess(&sys, Path::new("/c"), Assess::Exec),
            Gatekeeper::Accepted("Notarized Developer ID".into())
        );
        assert!(matches!(
            assess(&sys, Path::new("/d"), Assess::Exec),
            Gatekeeper::Rejected(_)
        ));
    }

    #[test]
    fn a_rejection_on_an_app_store_only_mac_says_it_is_the_mac_s_setting() {
        let rejected = "/s/Caffeine.app: rejected\nsource=Notarized Developer ID\norigin=Developer ID Application: Michael Jones (YD6LEYT6WZ)\n";
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/spctl -a -vv -t exec /s/Caffeine.app",
            err(3, rejected),
        );
        sys.on(
            "/usr/sbin/spctl --status -v",
            out_status(0, "assessments enabled\ndeveloper id disabled\n"),
        );
        let why = require_gatekeeper(
            &sys,
            Path::new("/s/Caffeine.app"),
            Assess::Exec,
            &Notes::default(),
        )
        .unwrap_err();
        assert!(why.contains("App Store only"), "{why}");
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/spctl -a -vv -t exec /s/Caffeine.app",
            err(3, rejected),
        );
        sys.on(
            "/usr/sbin/spctl --status -v",
            out_status(0, "assessments enabled\ndeveloper id enabled\n"),
        );
        let why = require_gatekeeper(
            &sys,
            Path::new("/s/Caffeine.app"),
            Assess::Exec,
            &Notes::default(),
        )
        .unwrap_err();
        assert!(!why.contains("App Store only"), "{why}");
    }

    #[test]
    fn the_mount_point_comes_from_hdiutil_s_plist() {
        assert_eq!(
            mount_points(ATTACH_CAFFEINE),
            [PathBuf::from("/private/tmp/swteam/stage/dmg.9CxWxv")]
        );
        assert_eq!(
            mount_points("<key>mount-point</key>\n<string>/a &amp; b</string>"),
            [PathBuf::from("/a & b")]
        );
        assert!(mount_points("<plist/>").is_empty());
    }

    #[test]
    fn an_image_is_detached_even_when_the_work_on_it_fails() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/hdiutil attach -readonly -nobrowse -noautoopen -mountrandom /s -plist /i.dmg",
            ok(ATTACH_CAFFEINE),
        );
        sys.on(
            "/usr/bin/hdiutil detach /private/tmp/swteam/stage/dmg.9CxWxv",
            ok("\"disk6\" ejected.\n"),
        );
        let r: Result<(), String> = with_image(&sys, Path::new("/i.dmg"), Path::new("/s"), |_| {
            Err("copy failed".into())
        });
        assert_eq!(r, Err("copy failed".into()));
        assert_eq!(
            sys.calls().last().map(String::as_str),
            Some("/usr/bin/hdiutil detach /private/tmp/swteam/stage/dmg.9CxWxv")
        );
    }

    #[test]
    fn a_refused_detach_is_forced() {
        let sys = Fake::root();
        sys.on(
            "/usr/bin/hdiutil detach /m",
            err(16, "hdiutil: couldn't unmount \"disk6\" - Resource busy"),
        );
        sys.on("/usr/bin/hdiutil detach -force /m", ok(""));
        assert_eq!(detach(&sys, Path::new("/m")), Ok(()));
    }

    #[test]
    fn exactly_one_real_app_at_the_root_is_required() {
        let sys = Fake::root();
        sys.file("/m/Caffeine.app", b"")
            .stat_of("/m/Caffeine.app", dir());
        sys.file("/m/.DS_Store", b"");
        sys.file("/m/Applications", b"").stat_of(
            "/m/Applications",
            Stat {
                symlink: true,
                ..file()
            },
        );
        assert_eq!(one_app_in(&sys, Path::new("/m")), Ok("Caffeine.app".into()));
        sys.file("/m/Other.app", b"").stat_of("/m/Other.app", dir());
        assert!(
            one_app_in(&sys, Path::new("/m"))
                .unwrap_err()
                .contains("2 .app")
        );
        let sys = Fake::root();
        sys.file("/z/Link.app", b"").stat_of(
            "/z/Link.app",
            Stat {
                symlink: true,
                ..dir()
            },
        );
        assert!(
            one_app_in(&sys, Path::new("/z"))
                .unwrap_err()
                .contains("0 .app")
        );
    }

    #[test]
    fn the_mach_o_magic_tells_a_universal_binary_from_a_java_class() {
        assert!(is_macho(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 0x01]));
        assert!(is_macho(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]));
        assert!(!is_macho(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 52]));
        assert!(!is_macho(b"#!/bin/sh\n"));
        assert!(!is_macho(&[0xcf, 0xfa]));
    }

    #[test]
    fn pkgutil_names_the_installer_leaf_and_its_team() {
        assert_eq!(
            parse_pkg_signature(Some(0), PKGUTIL_HOMEBREW),
            PkgSignature::Signed {
                status: "signed by a developer certificate issued by Apple for distribution".into(),
                leaf: "Developer ID Installer: Patrick Linnane (927JGANW46)".into(),
                team: "927JGANW46".into(),
                notarized: true
            }
        );
        assert_eq!(
            parse_pkg_signature(Some(1), PKGUTIL_GH),
            PkgSignature::Unsigned
        );
        assert!(matches!(
            parse_pkg_signature(None, ""),
            PkgSignature::Unusable(_)
        ));
    }

    #[test]
    fn a_receipt_reads_its_version_and_pkgutil_s_no_receipt_is_absent() {
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/pkgutil --pkg-info com.github.cli",
            ok(PKGINFO_GH),
        );
        assert_eq!(
            receipt_version(&sys, "com.github.cli"),
            Read::Value("2.102.0".into())
        );
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/pkgutil --pkg-info com.github.cli",
            err(1, PKGINFO_ABSENT),
        );
        assert_eq!(receipt_version(&sys, "com.github.cli"), Read::Absent);
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/pkgutil --pkg-info com.github.cli",
            err(2, "boom"),
        );
        assert!(matches!(
            receipt_version(&sys, "com.github.cli"),
            Read::Unusable(_)
        ));
    }

    #[test]
    fn a_missing_key_is_absent_and_a_failed_read_is_not() {
        let sys = Fake::root();
        sys.file("/A.app/Contents/Info.plist", b"");
        sys.on(
            &plistbuddy("/A.app", "CFBundleShortVersionString"),
            ok("1.1.4\n"),
        );
        sys.on(
            &plistbuddy("/A.app", "NoSuchKey"),
            out_status(1, PLISTBUDDY_NO_KEY),
        );
        assert_eq!(
            bundle_info(&sys, Path::new("/A.app"), "CFBundleShortVersionString"),
            Read::Value("1.1.4".into())
        );
        assert_eq!(
            bundle_info(&sys, Path::new("/A.app"), "NoSuchKey"),
            Read::Absent
        );
        assert!(matches!(
            bundle_info(&sys, Path::new("/A.app"), "Other"),
            Read::Unusable(_)
        ));
        assert_eq!(bundle_info(&sys, Path::new("/B.app"), "X"), Read::Absent);
    }

    #[test]
    fn a_row_without_a_team_or_a_bundle_or_an_https_url_is_refused() {
        let ctx = ctx_with(&Fake::root(), &[], true);
        let mut row = caffeine_row(CAFFEINE_SHA);
        row.team_id = None;
        assert!(
            expand(&[row], &ctx)
                .err()
                .unwrap()
                .contains("team_id is required")
        );
        let mut row = caffeine_row(CAFFEINE_SHA);
        row.bundle = None;
        assert!(
            expand(&[row], &ctx)
                .err()
                .unwrap()
                .contains("names its bundle")
        );
        let mut row = caffeine_row(CAFFEINE_SHA);
        row.url = "http://example.com/x.dmg".into();
        assert!(expand(&[row], &ctx).err().unwrap().contains("not https"));
        let mut row = gh_row(GH_SHA);
        row.pkg_id = None;
        assert!(expand(&[row], &ctx).err().unwrap().contains("pkg_id"));
        let mut row = caffeine_row("abc");
        row.sha256 = "abc".into();
        assert!(
            expand(&[row], &ctx)
                .err()
                .unwrap()
                .contains("64 hexadecimal")
        );
    }

    /// The installed Caffeine.app at 1.1.4, signed by its team.
    fn installed_caffeine(sys: &Fake, version: &str) {
        sys.file("/Applications/Caffeine.app", b"")
            .stat_of("/Applications/Caffeine.app", dir());
        sys.file("/Applications/Caffeine.app/Contents/Info.plist", b"");
        sys.on(
            &plistbuddy("/Applications/Caffeine.app", "CFBundleShortVersionString"),
            ok(&format!("{version}\n")),
        );
        sys.on(
            &codesign_key("/Applications/Caffeine.app", "YD6LEYT6WZ"),
            ok(""),
        );
    }

    #[test]
    fn an_installed_bundle_at_the_pinned_version_and_team_is_ok() {
        let sys = Fake::root();
        installed_caffeine(&sys, "1.1.4");
        let ctx = ctx_with(&sys, &[], false);
        let row = one(
            &caffeine_row(CAFFEINE_SHA),
            &sys,
            &ctx,
            crate::model::Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        assert_eq!(
            row.to.as_deref(),
            Some("Caffeine.app 1.1.4 (team YD6LEYT6WZ)")
        );
        assert_eq!(row.id, "app.Caffeine");
    }

    #[test]
    fn a_missing_artifact_blocks_the_install() {
        let sys = Fake::root();
        let ctx = ctx_with(&sys, &[], true);
        let row = one(
            &caffeine_row(CAFFEINE_SHA),
            &sys,
            &ctx,
            crate::model::Mode::Plan,
        );
        assert_eq!(row.outcome, Outcome::Blocked);
        assert!(
            row.why
                .unwrap()
                .contains("not in the cache; the controller uploads it")
        );
    }

    #[test]
    fn an_older_bundle_that_is_running_waits_for_an_idle_mac() {
        let sys = Fake::root();
        installed_caffeine(&sys, "1.1.3");
        sys.on(
            "/usr/bin/pgrep -f '/Applications/Caffeine\\.app/Contents/MacOS/'",
            ok("1614\n"),
        );
        let bytes = b"dmg";
        let sha = sha_of(bytes);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], false);
        let row = one(&caffeine_row(&sha), &sys, &ctx, crate::model::Mode::Plan);
        assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        let row = one(&caffeine_row(&sha), &sys, &ctx, crate::model::Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned);
        assert_eq!(row.from.as_deref(), Some("1.1.3"));
    }

    #[test]
    fn a_disk_image_install_stages_vets_swaps_and_cleans_up() {
        let sys = Fake::root();
        let bytes = b"caffeine dmg";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        installed_caffeine(&sys, "1.1.3");
        sys.on(
            &plistbuddy("/Applications/Caffeine.app", "CFBundleShortVersionString"),
            ok("1.1.4\n"),
        );
        let mount = "/private/tmp/swteam/stage/dmg.9CxWxv";
        sys.on(
            &format!("/usr/bin/hdiutil attach -readonly -nobrowse -noautoopen -mountrandom {stage} -plist /c/{sha}"),
            ok(ATTACH_CAFFEINE),
        );
        sys.file(&format!("{mount}/Caffeine.app"), b"")
            .stat_of(&format!("{mount}/Caffeine.app"), dir());
        let staged = format!("{stage}/new/Caffeine.app");
        sys.on(
            &format!("/usr/bin/ditto --noacl --noqtn {mount}/Caffeine.app {staged}"),
            ok(""),
        );
        sys.on(
            &format!("/usr/bin/hdiutil detach {mount}"),
            ok("\"disk6\" ejected.\n"),
        );
        sys.on(&format!("/usr/sbin/chown -R 0:0 {staged}"), ok(""));
        sys.on(&format!("/bin/chmod -R go-w,ug-s {staged}"), ok(""));
        sys.on(&codesign_key(&staged, "YD6LEYT6WZ"), ok(""));
        sys.on(
            &format!("/usr/sbin/spctl -a -vv -t exec {staged}"),
            err(0, SPCTL_CAFFEINE_OFF),
        );
        sys.on_write(
            &format!("/usr/bin/ditto --noacl --noqtn {mount}/Caffeine.app {staged}"),
            Path::new(&format!("{staged}/Contents/Info.plist")),
            b"",
        );
        sys.on(
            &plistbuddy(&staged, "CFBundleShortVersionString"),
            ok("1.1.4\n"),
        );
        let row = one(&caffeine_row(&sha), &sys, &ctx, crate::model::Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert!(row.detail.as_deref().unwrap().contains("gatekeeper: not assessed for Caffeine.app (spctl: override=security disabled, source=Notarized Developer ID)"));
        assert!(row.revert.unwrap().contains("none kept"));
        let calls = sys.calls();
        let pos = |needle: &str| {
            calls
                .iter()
                .position(|c| c.starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} not run: {calls:#?}"))
        };
        let last = |needle: &str| calls.iter().rposition(|c| c.starts_with(needle)).unwrap();
        assert!(pos("/usr/bin/ditto") < pos("/usr/bin/hdiutil detach"));
        assert!(pos("/usr/bin/hdiutil detach") < pos("/usr/sbin/chown -R 0:0"));
        assert!(
            pos("/usr/sbin/chown")
                < pos(
                    "/usr/bin/codesign --verify --deep --strict '-R=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"YD6LEYT6WZ\"' /Library"
                )
        );
        let aside = pos(&format!(
            "rename /Applications/Caffeine.app {stage}/old/Caffeine.app"
        ));
        let into = pos(&format!("rename {staged} /Applications/Caffeine.app"));
        assert!(pos("/usr/sbin/spctl") < aside && aside < into);
        assert!(last(&format!("scratch closed {stage}")) > into);
    }

    #[test]
    fn a_bundle_from_another_team_is_never_swapped_in() {
        let sys = Fake::root();
        let bytes = b"kya zip";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        sys.on(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            ok(""),
        );
        let staged = format!("{stage}/new/KeepingYouAwake.app");
        sys.on_write(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&staged),
            b"",
        )
        .on_stat(
            &format!("/usr/bin/ditto -x -k --noacl --noqtn /c/{sha} {stage}/new"),
            Path::new(&staged),
            dir(),
        );
        sys.on(&format!("/usr/sbin/chown -R 0:0 {staged}"), ok(""));
        sys.on(&format!("/bin/chmod -R go-w,ug-s {staged}"), ok(""));
        sys.on(
            &codesign_key(&staged, "5KESHV9W85"),
            err(3, CODESIGN_WRONG_TEAM),
        );
        let row = one(&kya_row(&sha), &sys, &ctx, crate::model::Mode::Apply);
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(
            row.why
                .unwrap()
                .contains("not by Developer ID team 5KESHV9W85")
        );
        assert!(!sys.calls().iter().any(|c| c.starts_with("rename")));
        assert_eq!(
            sys.calls().last().unwrap(),
            &format!("scratch closed {stage}")
        );
    }

    #[test]
    fn a_cached_artifact_that_no_longer_hashes_to_its_name_is_not_used() {
        let sys = Fake::root();
        let ctx = ctx_with(&sys, &[(KYA_SHA, b"not the zip")], true);
        let items = expand(&[kya_row(KYA_SHA)], &ctx).unwrap();
        let why = items[0].apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("hashes to"), "{why}");
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn a_failed_swap_puts_the_previous_bundle_back() {
        struct NoSecondRename(
            Fake,
            std::sync::atomic::AtomicU32,
            std::sync::Mutex<Vec<String>>,
        );
        impl System for NoSecondRename {
            fn scratch_dir(
                &self,
                parent: Option<&Path>,
                label: &str,
            ) -> std::io::Result<crate::port::Scratch<'_>> {
                self.0.scratch_dir(parent, label)
            }
            fn run(&self, c: &Cmd) -> Result<Ran, crate::port::RunFailed> {
                self.0.run(c)
            }
            fn run_tty(&self, c: &Cmd) -> Result<Option<i32>, crate::port::RunFailed> {
                self.0.run_tty(c)
            }
            fn read_file(&self, p: &Path) -> std::io::Result<Vec<u8>> {
                self.0.read_file(p)
            }
            fn write_file(&self, p: &Path, b: &[u8], m: u32) -> std::io::Result<()> {
                self.0.write_file(p, b, m)
            }
            fn write_user_file(&self, u: &User, p: &Path, b: &[u8], m: u32) -> std::io::Result<()> {
                self.0.write_user_file(u, p, b, m)
            }
            fn remove(&self, p: &Path) -> std::io::Result<()> {
                self.0.remove(p)
            }
            fn create_dir_all(&self, p: &Path, m: u32) -> std::io::Result<()> {
                self.0.create_dir_all(p, m)
            }
            fn chown(&self, p: &Path, u: u32, g: u32) -> std::io::Result<()> {
                self.0.chown(p, u, g)
            }
            fn stat(&self, p: &Path) -> std::io::Result<Stat> {
                self.0.stat(p)
            }
            fn list_dir(&self, p: &Path) -> std::io::Result<Vec<String>> {
                self.0.list_dir(p)
            }
            fn sha256(&self, p: &Path) -> std::io::Result<String> {
                self.0.sha256(p)
            }
            fn copy_file(&self, f: &Path, t: &Path, m: u32) -> std::io::Result<()> {
                self.0.copy_file(f, t, m)
            }
            fn rename(&self, f: &Path, t: &Path) -> std::io::Result<()> {
                let n = self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.2
                    .lock()
                    .unwrap()
                    .push(format!("rename {} {}", f.display(), t.display()));
                if n == 1 {
                    return Err(std::io::Error::other("EXDEV"));
                }
                self.0.rename(f, t)
            }
            fn euid(&self) -> u32 {
                0
            }
            fn now(&self) -> u64 {
                0
            }
        }
        let sys = NoSecondRename(
            Fake::root(),
            std::sync::atomic::AtomicU32::new(0),
            std::sync::Mutex::new(Vec::new()),
        );
        sys.0
            .file("/Applications/A.app", b"old")
            .stat_of("/Applications/A.app", dir());
        let stage =
            Stage::new(crate::port::Scratch::new_in(&sys, Path::new("/s"), "swap").unwrap());
        let stage_path = stage.to_path_buf();
        let err = swap_into(
            &sys,
            &stage.join("new/A.app"),
            Path::new("/Applications/A.app"),
            &stage,
        )
        .unwrap_err();
        assert!(
            err.contains("the previous bundle is back in place"),
            "{err}"
        );
        assert_eq!(
            sys.0.read_file(Path::new("/Applications/A.app")).unwrap(),
            b"old"
        );
        assert_eq!(
            sys.2.lock().unwrap().clone(),
            [
                format!(
                    "rename /Applications/A.app {}/old/A.app",
                    stage_path.display()
                ),
                format!(
                    "rename {}/new/A.app /Applications/A.app",
                    stage_path.display()
                ),
                format!(
                    "rename {}/old/A.app /Applications/A.app",
                    stage_path.display()
                )
            ]
        );
    }

    #[test]
    fn an_unsigned_pkg_installs_only_when_every_mach_o_in_it_carries_the_team() {
        let sys = Fake::root();
        let bytes = b"gh pkg";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        sys.on(
            "/usr/sbin/pkgutil --pkg-info com.github.cli",
            err(1, PKGINFO_ABSENT),
        );
        sys.on(
            "/usr/sbin/pkgutil --pkg-info com.github.cli",
            ok(PKGINFO_GH),
        );
        let x = format!("{stage}/expanded");
        sys.on(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            ok(""),
        );
        let gh = format!("{x}/com.github.cli.pkg/Payload/usr/local/bin/gh");
        let man = format!("{x}/com.github.cli.pkg/Payload/usr/local/share/man/man1/gh.1");
        sys.on(
            &format!("/usr/bin/find {x} -type f -print0"),
            ok(&format!("{x}/Distribution\0{gh}\0{man}\0")),
        );
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            Path::new(&format!("{x}/Distribution")),
            b"<?xml version=\"1.0\"?>",
        );
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            Path::new(&gh),
            &[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2, 0, 0],
        );
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            Path::new(&man),
            b".TH GH 1",
        );
        sys.on(&codesign_key(&gh, "VEKTX9H2N7"), ok(""));
        sys.on(
            &format!("/usr/sbin/installer -pkg {stage}/gh_2.102.0_macOS_universal.pkg -target /"),
            ok(INSTALLER_GH),
        );
        sys.file("/usr/local/bin/gh", b"");
        sys.on(&codesign_key("/usr/local/bin/gh", "VEKTX9H2N7"), ok(""));
        let row = one(&gh_row(&sha), &sys, &ctx, crate::model::Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(row.from.as_deref(), Some("absent (no receipt)"));
        assert!(
            row.detail
                .unwrap()
                .contains("1 Mach-O file(s) verified as team VEKTX9H2N7")
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|c| c.starts_with("/usr/bin/codesign"))
                .count(),
            2
        );
    }

    #[test]
    fn an_unsigned_pkg_with_scripts_is_refused() {
        let sys = Fake::root();
        let bytes = b"scripted pkg";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        let x = format!("{stage}/expanded");
        sys.on(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            ok(""),
        );
        sys.on(
            &format!("/usr/bin/find {x} -type f -print0"),
            ok(&format!("{x}/a.pkg/Scripts/postinstall\0")),
        );
        let items = expand(&[gh_row(&sha)], &ctx).unwrap();
        let why = items[0].apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("installer scripts"), "{why}");
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("/usr/sbin/installer"))
        );
    }

    /// An unsigned pkg's apply up to its `find` listing, which answers `listing`.
    fn unsigned_pkg_listing(listing: &dyn Fn(&str) -> String) -> (Fake, Ctx, String) {
        let sys = Fake::root();
        let bytes = b"component pkg";
        let sha = sha_of(bytes);
        let stage = stage_path(0);
        let ctx = ctx_with(&sys, &[(&sha, bytes)], true);
        let x = format!("{stage}/expanded");
        sys.on(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            ok(""),
        );
        sys.on(
            &format!("/usr/bin/find {x} -type f -print0"),
            ok(&listing(&x)),
        );
        (sys, ctx, sha)
    }

    #[test]
    fn a_component_pkg_s_top_level_scripts_are_refused_too() {
        let (sys, ctx, sha) = unsigned_pkg_listing(&|x| {
            format!(
                "{x}/Bom\0{x}/PackageInfo\0{x}/Payload/usr/local/bin/gh\0{x}/Scripts/postinstall\0"
            )
        });
        let stage = stage_path(0);
        let x = format!("{stage}/expanded");
        let gh = format!("{x}/Payload/usr/local/bin/gh");
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            Path::new(&gh),
            &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0],
        );
        sys.on(&codesign_key(&gh, "VEKTX9H2N7"), ok(""));
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            Path::new(&format!("{x}/Bom")),
            b"BOMStore",
        );
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"),
            Path::new(&format!("{x}/PackageInfo")),
            b"<pkg-info/>",
        );
        let items = expand(&[gh_row(&sha)], &ctx).unwrap();
        let why = items[0].apply(&sys, &ctx).unwrap_err();
        assert!(
            why.contains("installer scripts (Scripts/postinstall)"),
            "{why}"
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("/usr/sbin/installer"))
        );
    }

    #[test]
    fn an_unsigned_pkg_whose_distribution_may_run_programs_is_refused() {
        let (sys, ctx, sha) = unsigned_pkg_listing(&|x| format!("{x}/Distribution\0"));
        let stage = stage_path(0);
        let x = format!("{stage}/expanded");
        sys.on_write(
            &format!("/usr/sbin/pkgutil --expand-full {stage}/gh_2.102.0_macOS_universal.pkg {x}"), Path::new(&format!("{x}/Distribution")),
            br#"<installer-gui-script><options allow-external-scripts="true"/><script>system.run('/bin/sh');</script></installer-gui-script>"#,
        );
        let items = expand(&[gh_row(&sha)], &ctx).unwrap();
        let why = items[0].apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("allows external scripts"), "{why}");
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.starts_with("/usr/sbin/installer"))
        );
    }

    #[test]
    fn only_an_explicit_false_keeps_a_distribution_s_javascript_from_running_programs() {
        assert!(!allows_external_scripts(
            r#"<options hostArchitectures="arm64,x86_64" customize="never" require-scripts="true" allow-external-scripts="false"/>"#
        ));
        assert!(!allows_external_scripts("<options/>"));
        assert!(!allows_external_scripts("allow-external-scripts = 'no'"));
        assert!(allows_external_scripts(r#"allow-external-scripts="yes""#));
        assert!(allows_external_scripts(
            r#"allow-external-scripts="false" allow-external-scripts="true""#
        ));
        assert!(allows_external_scripts("allow-external-scripts"));
        assert!(allows_external_scripts(r#"allow-external-scripts="false"#));
    }

    #[test]
    fn fresh_staging_never_adopts_or_removes_an_earlier_attempt() {
        let sys = Fake::root();
        sys.stat_of(crate::AGENT_ROOT, dir()).stat_of(
            STAGE_ROOT,
            Stat {
                mode: 0o700,
                ..dir()
            },
        );
        let old = Path::new(STAGE_ROOT)
            .join("0".repeat(64))
            .join("old/A.app/receipt");
        let occupied = PathBuf::from(stage_path(0)).join("old/A.app/receipt");
        sys.file(old.to_str().unwrap(), b"older retained bundle");
        sys.file(occupied.to_str().unwrap(), b"another retained bundle");
        let first = fresh_stage(&sys).unwrap();
        assert_eq!(first.to_path_buf(), PathBuf::from(stage_path(1)));
        sys.write_file(&first.join("new/file"), b"scratch", 0o600)
            .unwrap();
        let first_path = first.to_path_buf();
        drop(first);
        assert!(sys.stat(&first_path).is_err());
        let next = fresh_stage(&sys).unwrap();
        assert_ne!(next.to_path_buf(), first_path);
        drop(next);
        assert_eq!(sys.read_file(&old).unwrap(), b"older retained bundle");
        assert_eq!(
            sys.read_file(&occupied).unwrap(),
            b"another retained bundle"
        );
        for bad in [
            Stat { uid: 501, ..dir() },
            Stat {
                mode: 0o775,
                ..dir()
            },
            Stat {
                symlink: true,
                dir: false,
                ..dir()
            },
        ] {
            let sys = Fake::root();
            sys.stat_of(crate::AGENT_ROOT, dir())
                .stat_of(STAGE_ROOT, bad)
                .file(old.to_str().unwrap(), b"unproven owner bytes");
            assert!(fresh_stage(&sys).is_err());
            assert_eq!(sys.read_file(&old).unwrap(), b"unproven owner bytes");
            assert!(
                sys.calls().is_empty(),
                "unadmitted parent is never allocated below"
            );
        }
        let sys = Fake::root();
        sys.stat_of(crate::AGENT_ROOT, dir());
        assert!(
            fresh_stage(&sys).is_err(),
            "missing staging parent is not recreated"
        );
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn a_path_codesign_cannot_read_is_no_verdict_on_its_signer() {
        let sys = Fake::root();
        sys.on(
            &codesign_key("/x", "VEKTX9H2N7"),
            err(1, "/x: Permission denied"),
        );
        assert!(matches!(
            verify_team(&sys, Path::new("/x"), "VEKTX9H2N7"),
            Signed::Unusable(_)
        ));
    }

    #[test]
    fn a_pkg_row_names_what_it_installs_and_two_rows_never_share_a_bundle() {
        let ctx = ctx_with(&Fake::root(), &[], true);
        let mut row = gh_row(GH_SHA);
        row.binary = None;
        assert!(
            expand(&[row], &ctx)
                .err()
                .unwrap()
                .contains("names the bundle or the binary")
        );
        let mut again = caffeine_row(CAFFEINE_SHA);
        again.name = "Caffeine-again".into();
        again.bundle = Some("caffeine.app".into());
        assert!(
            expand(&[caffeine_row(CAFFEINE_SHA), again], &ctx)
                .err()
                .unwrap()
                .contains("already installed by [[app]] Caffeine")
        );
        let mut gh2 = gh_row(GH_SHA);
        gh2.name = "gh2".into();
        gh2.binary = Some("/usr/local/bin/gh2".into());
        assert!(
            expand(&[gh_row(GH_SHA), gh2], &ctx)
                .err()
                .unwrap()
                .contains("receipt com.github.cli")
        );
    }

    #[test]
    fn a_signed_pkg_pkgutil_does_not_trust_is_refused() {
        let sys = Fake::root();
        let art = Artifact {
            url: "https://example.com/x.pkg".into(),
            sha256: "0".repeat(64),
            team_id: Some("927JGANW46".into()),
            unsigned: false,
        };
        sys.on(
            "/usr/sbin/pkgutil --check-signature /p.pkg",
            ok(&PKGUTIL_HOMEBREW.replace(
                "signed by a developer certificate issued by Apple for distribution",
                "signed by untrusted certificate",
            )),
        );
        let why = vet_pkg(
            &sys,
            Path::new("/p.pkg"),
            &art,
            Path::new("/s"),
            &Notes::default(),
        )
        .unwrap_err();
        assert!(why.contains("does not trust"), "{why}");
        assert!(!sys.calls().iter().any(|c| c.contains("spctl")));
    }

    #[test]
    fn a_signed_pkg_must_name_the_row_s_team() {
        let sys = Fake::root();
        let notes = Notes::default();
        let art = Artifact {
            url: "https://example.com/x.pkg".into(),
            sha256: "0".repeat(64),
            team_id: Some("AAAAAAAAAA".into()),
            unsigned: false,
        };
        sys.on(
            "/usr/sbin/pkgutil --check-signature /p.pkg",
            ok(PKGUTIL_HOMEBREW),
        );
        let why = vet_pkg(&sys, Path::new("/p.pkg"), &art, Path::new("/s"), &notes).unwrap_err();
        assert!(
            why.contains("not a Developer ID Installer of team AAAAAAAAAA"),
            "{why}"
        );
        let art = Artifact {
            team_id: Some("927JGANW46".into()),
            ..art
        };
        sys.on(
            "/usr/sbin/spctl -a -vv -t install /p.pkg",
            err(0, SPCTL_HOMEBREW_OFF),
        );
        assert_eq!(
            vet_pkg(&sys, Path::new("/p.pkg"), &art, Path::new("/s"), &notes),
            Ok(())
        );
        assert!(notes.joined().unwrap().contains("not assessed for p.pkg"));
    }

    #[test]
    fn a_stage_never_recreates_an_agent_root_an_uninstall_removed() {
        let sys = Fake::root();
        let why = fresh_stage(&sys).unwrap_err();
        assert!(why.contains("the agent is not installed here"), "{why}");
        assert!(sys.calls().is_empty());
    }

    #[test]
    fn the_staged_pkg_takes_its_url_s_name_or_a_safe_one() {
        let sys = Fake::root();
        sys.file("/c/x", b"pkg bytes");
        let mut art = gh_row(&sha_of(b"pkg bytes")).artifact();
        assert_eq!(
            stage_pkg(&sys, Path::new("/c/x"), Path::new("/s"), &art),
            Ok(PathBuf::from("/s/gh_2.102.0_macOS_universal.pkg"))
        );
        art.url = "https://example.com/download?file=x.pkg".into();
        assert_eq!(
            stage_pkg(&sys, Path::new("/c/x"), Path::new("/s"), &art),
            Ok(PathBuf::from("/s/artifact.pkg"))
        );
        art.url = "https://example.com/..pkg".into();
        assert_eq!(
            stage_pkg(&sys, Path::new("/c/x"), Path::new("/s"), &art),
            Ok(PathBuf::from("/s/artifact.pkg"))
        );
        art.sha256 = "0".repeat(64);
        assert!(
            stage_pkg(&sys, Path::new("/c/x"), Path::new("/s"), &art)
                .unwrap_err()
                .contains("hashes to")
        );
    }

    #[test]
    fn pgrep_s_pattern_is_the_literal_bundle_path() {
        assert_eq!(
            regex_literal("/Applications/Foo (1)+.app/Contents/MacOS/"),
            "/Applications/Foo \\(1\\)\\+\\.app/Contents/MacOS/"
        );
    }
    /// The actual swap -> cleanup sequence retains displaced original bytes on
    #[test]
    fn a_failed_restore_keeps_the_old_bundle_out_of_stage_cleanup() {
        use std::sync::atomic::{AtomicU32, Ordering};
        struct SwapFaults {
            inner: Fake,
            renames: AtomicU32,
            fail_new: bool,
            fail_restore: bool,
            panic_publish: bool,
        }
        impl System for SwapFaults {
            fn scratch_dir(
                &self,
                parent: Option<&Path>,
                label: &str,
            ) -> std::io::Result<crate::port::Scratch<'_>> {
                self.inner.scratch_dir(parent, label)
            }
            fn run(&self, c: &Cmd) -> Result<Ran, crate::port::RunFailed> {
                self.inner.run(c)
            }
            fn run_tty(&self, c: &Cmd) -> Result<Option<i32>, crate::port::RunFailed> {
                self.inner.run_tty(c)
            }
            fn read_file(&self, p: &Path) -> std::io::Result<Vec<u8>> {
                self.inner.read_file(p)
            }
            fn write_file(&self, p: &Path, b: &[u8], m: u32) -> std::io::Result<()> {
                self.inner.write_file(p, b, m)
            }
            fn write_user_file(&self, u: &User, p: &Path, b: &[u8], m: u32) -> std::io::Result<()> {
                self.inner.write_user_file(u, p, b, m)
            }
            fn remove(&self, p: &Path) -> std::io::Result<()> {
                self.inner.remove(p)
            }
            fn create_dir_all(&self, p: &Path, m: u32) -> std::io::Result<()> {
                self.inner.create_dir_all(p, m)
            }
            fn chown(&self, p: &Path, u: u32, g: u32) -> std::io::Result<()> {
                self.inner.chown(p, u, g)
            }
            fn stat(&self, p: &Path) -> std::io::Result<Stat> {
                self.inner.stat(p)
            }
            fn list_dir(&self, p: &Path) -> std::io::Result<Vec<String>> {
                self.inner.list_dir(p)
            }
            fn sha256(&self, p: &Path) -> std::io::Result<String> {
                self.inner.sha256(p)
            }
            fn copy_file(&self, f: &Path, t: &Path, m: u32) -> std::io::Result<()> {
                self.inner.copy_file(f, t, m)
            }
            fn rename(&self, f: &Path, t: &Path) -> std::io::Result<()> {
                let n = self
                    .renames
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 1 && self.panic_publish {
                    panic!("injected unwind after original moved");
                }
                if (n == 1 && self.fail_new) || (n == 2 && self.fail_restore) {
                    // A refused rename changes nothing, unlike a post-mutation error.
                    return Err(std::io::Error::other("injected rename refusal"));
                }
                self.inner.rename(f, t)
            }
            fn euid(&self) -> u32 {
                0
            }
            fn now(&self) -> u64 {
                0
            }
        }
        let scenario = |fail_new, fail_restore, panic_publish| {
            let inner = Fake::root();
            inner
                .stat_of(crate::AGENT_ROOT, dir())
                .stat_of(
                    STAGE_ROOT,
                    Stat {
                        mode: 0o700,
                        ..dir()
                    },
                )
                .file("/Applications/A.app/receipt", b"original personal bytes")
                .stat_of("/Applications/A.app", dir());
            SwapFaults {
                inner,
                renames: AtomicU32::new(0),
                fail_new,
                fail_restore,
                panic_publish,
            }
        };
        let target = Path::new("/Applications/A.app");
        let receipt = target.join("receipt");
        for (fail_new, want) in [
            (false, b"new bytes".as_slice()),
            (true, b"original personal bytes".as_slice()),
        ] {
            let sys = scenario(fail_new, false, false);
            let stage = fresh_stage(&sys).unwrap();
            let path = stage.to_path_buf();
            sys.inner.file(
                stage.join("new/A.app/receipt").to_str().unwrap(),
                b"new bytes",
            );
            let done = swap_into(&sys, &stage.join("new/A.app"), target, &stage);
            assert_eq!(done.is_err(), fail_new, "positive control: {done:?}");
            let result = clear_stage(stage, done, &Notes::default());
            assert_eq!(result.is_err(), fail_new, "positive control: {result:?}");
            assert_eq!(sys.inner.read_file(&receipt).unwrap(), want);
            assert!(sys.inner.stat(&path).is_err(), "ordinary scratch is closed");
        }

        let sys = scenario(true, true, false);
        let notes = Notes::default();
        let stage = fresh_stage(&sys).unwrap();
        let path = stage.to_path_buf();
        sys.inner.file(
            stage.join("new/A.app/receipt").to_str().unwrap(),
            b"new bytes",
        );
        let why = swap_into(&sys, &stage.join("new/A.app"), target, &stage)
            .expect_err("both publication and restoration refuse");
        let retained = stage.join("old/A.app/receipt");
        assert!(why.contains("the previous bundle is at"), "{why}");
        assert_eq!(
            sys.renames.load(Ordering::SeqCst),
            3,
            "restore was attempted"
        );
        assert_eq!(
            sys.inner.read_file(&retained).unwrap(),
            b"original personal bytes"
        );
        assert_eq!(clear_stage(stage, Err(why.clone()), &notes), Err(why));
        assert_eq!(
            sys.inner.read_file(&retained).unwrap(),
            b"original personal bytes"
        );
        assert!(notes.joined().unwrap().contains(path.to_str().unwrap()));
        let retry = fresh_stage(&sys).unwrap();
        assert_ne!(retry.to_path_buf(), path);
        drop(retry);
        assert_eq!(
            sys.inner.read_file(&retained).unwrap(),
            b"original personal bytes"
        );

        let sys = scenario(false, false, true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let stage = fresh_stage(&sys).unwrap();
            sys.inner.file(
                stage.join("new/A.app/receipt").to_str().unwrap(),
                b"new bytes",
            );
            let _ = swap_into(&sys, &stage.join("new/A.app"), target, &stage);
        }));
        assert!(result.is_err());
        assert_eq!(
            sys.renames.load(Ordering::SeqCst),
            2,
            "unwind occurs at publication"
        );
        let retained = Path::new(&stage_path(0)).join("old/A.app/receipt");
        assert_eq!(
            sys.inner.read_file(&retained).unwrap(),
            b"original personal bytes"
        );
        let retry = fresh_stage(&sys).unwrap();
        drop(retry);
        assert_eq!(
            sys.inner.read_file(&retained).unwrap(),
            b"original personal bytes"
        );
    }

    #[test]
    fn an_unsigned_pkg_inventory_must_be_a_complete_record_before_code_is_admitted() {
        let stage = Path::new("/scratch");
        let code = "/scratch/expanded/Payload/bin/program";
        let listing = format!("{code}\0");
        let find = "/usr/bin/find /scratch/expanded -type f -print0";
        let fixture = || {
            let sys = Fake::root();
            sys.on(
                "/usr/sbin/pkgutil --expand-full /input.pkg /scratch/expanded",
                ok(""),
            );
            sys.on(find, ok(&listing));
            sys.file(code, &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]);
            sys.on(&codesign_key(code, "VEKTX9H2N7"), ok(""));
            sys
        };
        let sys = fixture();
        let notes = Notes::default();
        vet_unsigned_pkg(&sys, Path::new("/input.pkg"), "VEKTX9H2N7", stage, &notes)
            .expect("ordinary complete inventory admits the signed Mach-O control");
        assert!(notes.joined().unwrap().contains("1 Mach-O file(s)"));
        assert!(
            sys.calls()
                .iter()
                .any(|call| call.starts_with("/usr/bin/codesign"))
        );

        let mut refused = Vec::new();
        for why in [
            "stdout did not reach EOF",
            "stdout read failed after a partial record",
        ] {
            let sys = fixture();
            sys.refuse_complete_stdout(find, why);
            let notes = Notes::default();
            let result =
                vet_unsigned_pkg(&sys, Path::new("/input.pkg"), "VEKTX9H2N7", stage, &notes);
            assert!(
                sys.calls().iter().any(|call| call == find),
                "actual inventory-read route reached"
            );
            refused.push((
                result.is_err(),
                notes.joined().is_none(),
                !sys.calls()
                    .iter()
                    .any(|call| call.starts_with("/usr/bin/codesign")),
            ));
        }
        let sys = Fake::root();
        sys.on(
            "/usr/sbin/pkgutil --expand-full /input.pkg /scratch/expanded",
            ok(""),
        );
        let lossy = "/scratch/expanded/Payload/bin/\u{fffd}";
        sys.on(
            find,
            Ran {
                status: Some(0),
                stdout: b"/scratch/expanded/Payload/bin/\xff\0".to_vec(),
                stderr: Vec::new(),
            },
        );
        sys.file(lossy, &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0]);
        sys.on(&codesign_key(lossy, "VEKTX9H2N7"), ok(""));
        let notes = Notes::default();
        let result = vet_unsigned_pkg(&sys, Path::new("/input.pkg"), "VEKTX9H2N7", stage, &notes);
        refused.push((
            result.is_err(),
            notes.joined().is_none(),
            !sys.calls()
                .iter()
                .any(|call| call.starts_with("/usr/bin/codesign")),
        ));
        assert_eq!(
            refused,
            [(true, true, true); 3],
            "every unproven inventory refuses before code admission"
        );
    }
}
