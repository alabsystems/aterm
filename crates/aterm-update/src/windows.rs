// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows: the build a local build loop staged, read from its pointer file.
//!
//! Windows has no network updater ([`crate::enabled`] is false there). What it has is a build
//! loop on the same machine (`aterm-winship update`) that leaves a self-installing dist folder
//! and one pointer to the newest of them, `%LOCALAPPDATA%\aterm\dist\latest.json`:
//!
//! ```json
//! {"schema":1,"version":"0.199.0","commit":"<12 hex>","commit_full":"<40 hex>",
//!  "build_number":1762819200,"dir":"<absolute dist dir>","staged_at":"<RFC3339>",
//!  "smoke":"pass"|"fail"|"not-run"}
//! ```
//!
//! This module reads that pointer and the record of what is installed beside the running
//! executable (`BUILD.json`, which every dist carries and its installer lays), says where the
//! copy on disk stands against the staged build, and applies it by running the dist's own
//! `install.ps1 -Staged`, which replaces the installed images without stopping a running
//! window. A build is offered only while it is not what is installed: a window that keeps
//! running an old image after the files under it were replaced is told to restart, not to
//! install again. Nothing here reads the network.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The only pointer schema this build reads.
pub const POINTER_SCHEMA: u64 = 1;

/// The command that builds and stages a newer build.
pub const STAGE_COMMAND: &str = "aterm-winship update";

/// How often a running window reads the pointer again.
pub const WATCH_EVERY: Duration = Duration::from_secs(15 * 60);

/// The staged build's smoke-test verdict, as the build loop recorded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Smoke {
    /// The staged build started and answered.
    Pass,
    /// The staged build failed its smoke test: it is named, never offered.
    Fail,
    /// No smoke test ran.
    NotRun,
}

impl Smoke {
    /// The pointer's word for this verdict.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotRun => "not-run",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "not-run" => Some(Self::NotRun),
            _ => None,
        }
    }
}

/// One staged build, as a valid pointer names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedBuild {
    /// `MAJOR.MINOR.0`.
    pub version: String,
    /// The short commit (12 hex).
    pub commit: String,
    /// The monotonic build number `--version` prints as `build:`; never 0.
    pub build_number: u64,
    /// The absolute dist folder holding the images and `install.ps1`.
    pub dir: PathBuf,
    /// When it was staged (RFC3339), as written; empty when the pointer said nothing.
    pub staged_at: String,
    /// Its smoke-test verdict.
    pub smoke: Smoke,
    /// It was built from a changed tree: the pointer's `dirty`, else its own `BUILD.json`'s
    /// ([`read_pointer`]). The build loop stages clean commits only.
    pub dirty: bool,
}

impl StagedBuild {
    /// `aterm 0.199.0 (commit 1a37de31a34a, build 1762819200)`.
    #[must_use]
    pub fn name(&self) -> String {
        build_name(&self.version, &self.commit, self.build_number)
    }

    /// The dist's installer.
    #[must_use]
    pub fn installer(&self) -> PathBuf {
        self.dir.join("install.ps1")
    }
}

fn build_name(version: &str, commit: &str, build_number: u64) -> String {
    format!("aterm {version} (commit {commit}, build {build_number})")
}

#[derive(serde::Deserialize)]
struct RawPointer {
    schema: Option<u64>,
    version: Option<String>,
    commit: Option<String>,
    build_number: Option<u64>,
    dir: Option<String>,
    staged_at: Option<String>,
    smoke: Option<String>,
    dirty: Option<bool>,
}

/// Whether `dir` is absolute as Windows writes one (`C:\…` or `\\server\share\…`), judged on
/// the text so every platform's tests agree.
fn is_windows_absolute(dir: &str) -> bool {
    let bytes = dir.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || dir.starts_with("\\\\")
}

/// Read one pointer's text. Every refusal says which field and why, in one line.
pub fn parse_pointer(text: &str) -> Result<StagedBuild, String> {
    // Windows PowerShell 5.1 writes UTF-8 with a byte-order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let raw: RawPointer = aterm_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    match raw.schema {
        Some(POINTER_SCHEMA) => {}
        Some(other) => {
            return Err(format!(
                "schema {other} is not schema {POINTER_SCHEMA}, the one this aterm reads"
            ));
        }
        None => return Err("no schema".to_string()),
    }
    let version = raw.version.unwrap_or_default();
    if version.is_empty()
        || !version
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c.is_ascii_alphabetic())
    {
        return Err(format!("version {version:?} is not a version"));
    }
    let commit = raw.commit.unwrap_or_default();
    if commit.len() != 12 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("commit {commit:?} is not 12 hex characters"));
    }
    let build_number = match raw.build_number {
        Some(0) => return Err("build_number is 0, which no stamped build carries".to_string()),
        Some(n) => n,
        None => return Err("no build_number".to_string()),
    };
    let dir = raw.dir.unwrap_or_default();
    if !is_windows_absolute(&dir) {
        return Err(format!("dir {dir:?} is not an absolute path"));
    }
    let smoke = match raw.smoke.as_deref() {
        None => Smoke::NotRun,
        Some(word) => Smoke::parse(word)
            .ok_or_else(|| format!("smoke {word:?} is not pass, fail or not-run"))?,
    };
    Ok(StagedBuild {
        version,
        commit: commit.to_ascii_lowercase(),
        build_number,
        dir: PathBuf::from(dir),
        staged_at: raw.staged_at.unwrap_or_default(),
        smoke,
        dirty: raw.dirty.unwrap_or(false),
    })
}

/// The build a folder holds, as its `BUILD.json` records it (`build.ps1` writes one into every
/// dist and `install.ps1` lays it beside the images).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledBuild {
    /// `MAJOR.MINOR.0`; empty when the record said nothing.
    pub version: String,
    /// The short commit (12 hex, lower case).
    pub commit: String,
    /// The build number; never 0.
    pub build_number: u64,
    /// It was built from a changed tree.
    pub dirty: bool,
}

impl InstalledBuild {
    /// `aterm 0.199.0 (commit 1a37de31a34a, build 1762819200)`, with `-dirty` on the commit of
    /// a build from a changed tree, as a stamp writes it.
    #[must_use]
    pub fn name(&self) -> String {
        let dirty = if self.dirty { "-dirty" } else { "" };
        build_name(
            &self.version,
            &format!("{}{dirty}", self.commit),
            self.build_number,
        )
    }
}

#[derive(serde::Deserialize)]
struct RawInstalled {
    version: Option<String>,
    commit: Option<String>,
    commit_full: Option<String>,
    build_number: Option<u64>,
    dirty: Option<bool>,
}

/// Read one `BUILD.json`'s text: `None` for a record that names no build (no build number, or
/// no commit), which is then no evidence of what is installed.
#[must_use]
pub fn parse_installed(text: &str) -> Option<InstalledBuild> {
    // Windows PowerShell 5.1 writes UTF-8 with a byte-order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let raw: RawInstalled = aterm_json::from_str(text).ok()?;
    let build_number = raw.build_number.filter(|n| *n != 0)?;
    let commit: String = [raw.commit, raw.commit_full]
        .into_iter()
        .flatten()
        .find(|c| c.len() >= 12 && c.bytes().all(|b| b.is_ascii_hexdigit()))?
        .chars()
        .take(12)
        .collect();
    Some(InstalledBuild {
        version: raw.version.unwrap_or_default(),
        commit: commit.to_ascii_lowercase(),
        build_number,
        dirty: raw.dirty.unwrap_or(false),
    })
}

/// The build `dir` holds, from its `BUILD.json`: `None` when there is no record to read (a
/// build tree, or an install older than the record).
#[must_use]
pub fn read_installed(dir: &Path) -> Option<InstalledBuild> {
    let text = std::fs::read_to_string(dir.join("BUILD.json")).ok()?;
    parse_installed(&text)
}

/// Where the running copy stands against the staged build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// No pointer file: nothing has been staged on this machine.
    Absent,
    /// A pointer file this build cannot use, and why.
    Unreadable(String),
    /// The running copy carries no build number (an unstamped build), so nothing compares.
    Unstamped(StagedBuild),
    /// The staged build is newer than what is installed.
    Newer(StagedBuild),
    /// The staged build carries the installed copy's build number and is not that build: the
    /// installed copy was built from a changed tree, or from another commit of the same second.
    Differs(StagedBuild),
    /// The staged build is what is installed on disk, and the running image is another build:
    /// the files under this window were replaced, and a restart moves it onto them.
    Installed(StagedBuild),
    /// The running copy is the staged build.
    Same(StagedBuild),
    /// What is installed is newer than the staged build.
    Older(StagedBuild),
}

impl Standing {
    /// The standing's one word (`staged=` on the control wire).
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            Self::Absent => "none",
            Self::Unreadable(_) => "unreadable",
            Self::Unstamped(_) => "unstamped",
            Self::Newer(_) => "newer",
            Self::Differs(_) => "differs",
            Self::Installed(_) => "installed",
            Self::Same(_) => "same",
            Self::Older(_) => "older",
        }
    }

    /// The staged build `apply` installs: not what is installed (strictly newer, or another
    /// build of the same number) and not failed by its smoke test.
    #[must_use]
    pub fn offered(&self) -> Option<&StagedBuild> {
        match self {
            Self::Newer(staged) | Self::Differs(staged) if staged.smoke != Smoke::Fail => {
                Some(staged)
            }
            _ => None,
        }
    }

    /// The staged build the pointer names, whatever its standing.
    #[must_use]
    pub const fn staged(&self) -> Option<&StagedBuild> {
        match self {
            Self::Newer(s)
            | Self::Differs(s)
            | Self::Installed(s)
            | Self::Same(s)
            | Self::Older(s)
            | Self::Unstamped(s) => Some(s),
            Self::Absent | Self::Unreadable(_) => None,
        }
    }
}

/// Classify a read pointer against the copy on disk. `installed` is the record beside the
/// running executable; without one (a build tree, an install older than the record) the
/// running image speaks for what is installed.
///
/// The build number orders builds. On equal numbers the commit and the changed-tree mark say
/// whether the two are one build: the number is HEAD's commit time, which a build from a
/// changed tree shares with its clean commit. Two builds from changed trees of one commit
/// cannot be told apart and count as one.
#[must_use]
pub fn classify(
    running: Running<'_>,
    installed: Option<&InstalledBuild>,
    pointer: Result<Option<StagedBuild>, String>,
) -> Standing {
    let staged = match pointer {
        Ok(Some(staged)) => staged,
        Ok(None) => return Standing::Absent,
        Err(why) => return Standing::Unreadable(why),
    };
    if running.build_number == 0 {
        return Standing::Unstamped(staged);
    }
    // The pointer's commit is twelve hex digits; a stamp may carry more of the same commit.
    let is_staged = |build: u64, commit: &str, dirty: bool| {
        build == staged.build_number
            && commit
                .get(..staged.commit.len())
                .is_some_and(|short| short.eq_ignore_ascii_case(&staged.commit))
            && dirty == staged.dirty
    };
    let (commit, dirty) = running
        .commit
        .strip_suffix("-dirty")
        .map_or((running.commit, false), |clean| (clean, true));
    let running_is_staged = is_staged(running.build_number, commit, dirty);
    let (on_disk, on_disk_is_staged) =
        installed.map_or((running.build_number, running_is_staged), |installed| {
            (
                installed.build_number,
                is_staged(installed.build_number, &installed.commit, installed.dirty),
            )
        });
    match staged.build_number.cmp(&on_disk) {
        std::cmp::Ordering::Greater => Standing::Newer(staged),
        std::cmp::Ordering::Less => Standing::Older(staged),
        std::cmp::Ordering::Equal if !on_disk_is_staged => Standing::Differs(staged),
        std::cmp::Ordering::Equal if running_is_staged => Standing::Same(staged),
        std::cmp::Ordering::Equal => Standing::Installed(staged),
    }
}

/// Read the pointer at `path`: `Ok(None)` when there is no file. A pointer that says nothing
/// of `dirty` takes it from the staged dist's own `BUILD.json`.
pub fn read_pointer(path: &Path) -> Result<Option<StagedBuild>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut staged = parse_pointer(&text)?;
    if !staged.dirty {
        staged.dirty = read_installed(&staged.dir)
            .is_some_and(|dist| dist.dirty && dist.build_number == staged.build_number);
    }
    Ok(Some(staged))
}

/// What this copy knows of the staged build: its standing, and what was read to judge it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Here {
    /// Where the copy on disk stands against the staged build.
    pub standing: Standing,
    /// The pointer file read.
    pub pointer: PathBuf,
    /// The record beside the running executable, when there is one.
    pub installed: Option<InstalledBuild>,
    /// Why the offered build cannot be installed over this copy ([`install_dest`]'s refusal,
    /// or a dist whose installer is gone); `None` when it can, or nothing is offered.
    pub blocked: Option<String>,
}

/// What a window says about the staged build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Nothing to say: no offered build, or one this copy cannot install.
    Nothing,
    /// A build `apply` installs over this copy.
    Offered(StagedBuild),
    /// The staged build is installed; the window still runs another one.
    Installed(StagedBuild),
}

impl Notice {
    /// The build number the notice is about.
    #[must_use]
    pub const fn build_number(&self) -> Option<u64> {
        match self {
            Self::Nothing => None,
            Self::Offered(staged) | Self::Installed(staged) => Some(staged.build_number),
        }
    }

    const fn kind(&self) -> u8 {
        match self {
            Self::Nothing => 0,
            Self::Offered(_) => 1,
            Self::Installed(_) => 2,
        }
    }
}

impl Here {
    /// What a window says: the install row for a build `apply` would install here, the
    /// restart note for one already installed, else nothing.
    #[must_use]
    pub fn notice(&self) -> Notice {
        match &self.standing {
            Standing::Installed(staged) => Notice::Installed(staged.clone()),
            standing => match standing.offered() {
                Some(staged) if self.blocked.is_none() => Notice::Offered(staged.clone()),
                _ => Notice::Nothing,
            },
        }
    }

    /// The build `apply` installs over this copy and its installer, or the one line that says
    /// why there is none.
    pub fn installable(&self) -> Result<&StagedBuild, String> {
        let staged = apply_refusal(&self.standing)?;
        self.blocked.clone().map_or(Ok(staged), Err)
    }
}

/// Look at the pointer at `pointer` from the copy `running_exe` belongs to (`None`: this
/// executable could not be located, so the running image speaks for the install).
#[must_use]
pub fn look(running: Running<'_>, running_exe: Option<&Path>, pointer: &Path) -> Here {
    let installed = running_exe.and_then(Path::parent).and_then(read_installed);
    let standing = classify(running, installed.as_ref(), read_pointer(pointer));
    let blocked = standing.offered().and_then(|staged| {
        running_exe
            .and_then(|exe| install_dest(exe, staged).err())
            .or_else(|| missing_installer(staged))
    });
    Here {
        standing,
        pointer: pointer.to_path_buf(),
        installed,
        blocked,
    }
}

/// The pointer's place under a `%LOCALAPPDATA%` folder.
#[must_use]
pub fn pointer_path_under(local_app_data: &Path) -> PathBuf {
    local_app_data
        .join("aterm")
        .join("dist")
        .join("latest.json")
}

/// The running copy, as the status lines name it.
#[derive(Clone, Copy, Debug)]
pub struct Running<'a> {
    /// `MAJOR.MINOR.0`.
    pub version: &'a str,
    /// The short commit, `unknown` for an unstamped build.
    pub commit: &'a str,
    /// The build number, 0 for an unstamped build.
    pub build_number: u64,
}

impl Running<'_> {
    /// `aterm 0.199.0 (commit 1a37de31a34a, build 1762819200)`.
    #[must_use]
    pub fn name(&self) -> String {
        build_name(self.version, self.commit, self.build_number)
    }
}

fn staged_when(staged: &StagedBuild) -> String {
    if staged.staged_at.is_empty() {
        String::new()
    } else {
        format!(", staged {}", staged.staged_at)
    }
}

/// The line that says how to install an offered build, or why this copy cannot.
fn apply_line(blocked: Option<&str>) -> String {
    blocked.map_or_else(
        || {
            "`aterm update apply` installs it and stops nothing: open windows keep running this \
             build, new ones use the new one (`aterm update apply --restart` installs it and \
             restarts aterm)"
                .to_string()
        },
        |why| format!("`aterm update apply` cannot install it: {why}"),
    )
}

/// What `aterm update status` and `aterm update check` print on Windows: the running build,
/// then where the copy on disk stands against the staged one and what to do about it.
#[must_use]
pub fn status_lines(running: Running<'_>, here: &Here) -> Vec<String> {
    let mut lines = vec![format!("{} is running", running.name())];
    let blocked = here.blocked.as_deref();
    match &here.standing {
        Standing::Newer(staged) | Standing::Differs(staged) if staged.smoke == Smoke::Fail => {
            lines.push(format!(
                "{} is staged at {}{} \u{2014} its smoke test failed, so it is not offered",
                staged.name(),
                staged.dir.display(),
                staged_when(staged),
            ));
            lines.push(format!("`{STAGE_COMMAND}` builds and stages another"));
        }
        Standing::Newer(staged) => {
            lines.push(format!(
                "{} is staged at {}{}, smoke test {}",
                staged.name(),
                staged.dir.display(),
                staged_when(staged),
                staged.smoke.as_str(),
            ));
            lines.push(apply_line(blocked));
        }
        Standing::Differs(staged) => {
            lines.push(format!(
                "{} is staged at {}{}, smoke test {}",
                staged.name(),
                staged.dir.display(),
                staged_when(staged),
                staged.smoke.as_str(),
            ));
            lines.push(format!(
                "{} carries the same build number and is not that build (a changed tree, or \
                 another commit)",
                here.installed.as_ref().map_or_else(
                    || "this copy".to_string(),
                    |installed| format!("the installed copy, {},", installed.name()),
                ),
            ));
            lines.push(apply_line(blocked));
        }
        Standing::Installed(staged) => lines.push(format!(
            "{} is installed{} \u{2014} this copy still runs the build above: restart aterm to \
             use the installed one",
            staged.name(),
            staged_when(staged),
        )),
        Standing::Same(staged) => lines.push(format!(
            "aterm is current: this is the newest staged build{} \u{2014} `{STAGE_COMMAND}` \
             builds a newer one",
            staged_when(staged),
        )),
        Standing::Older(staged) => lines.push(format!(
            "aterm is current: the staged build, {}, is older than what is installed \u{2014} \
             `{STAGE_COMMAND}` builds a newer one",
            staged.name(),
        )),
        Standing::Unstamped(staged) => lines.push(format!(
            "this copy carries no build number (an unstamped build), so it cannot be compared \
             with the staged build, {}",
            staged.name(),
        )),
        Standing::Absent => lines.push(format!(
            "no staged build: {} does not exist \u{2014} `{STAGE_COMMAND}` builds one and \
             stages it",
            here.pointer.display(),
        )),
        Standing::Unreadable(why) => lines.push(format!(
            "can\u{2019}t read the staged build pointer {}: {why} \u{2014} `{STAGE_COMMAND}` \
             writes a new one",
            here.pointer.display(),
        )),
    }
    lines
}

/// A quoted field as the control wire writes one everywhere (`outcome={:?}`): a backslash, a
/// quote and a control character are escaped, so a Windows path survives a reader that
/// unescapes and the closing quote is never ambiguous.
fn wire_quoted(value: &str) -> String {
    format!("{value:?}")
}

/// The `aterm ctl update status` answer on Windows, one `OK key=value …` line without its
/// newline. `enabled=false` is the network checker, which Windows does not run.
#[must_use]
pub fn control_line(running: Running<'_>, here: &Here) -> String {
    let standing = &here.standing;
    let mut line = format!(
        "OK enabled=false build={} version={} commit={} staged={}",
        running.build_number,
        running.version,
        running.commit,
        standing.word(),
    );
    if let Some(staged) = standing.staged() {
        line.push_str(&format!(
            " staged_build={} staged_version={} staged_commit={} smoke={}",
            staged.build_number,
            staged.version,
            staged.commit,
            staged.smoke.as_str(),
        ));
        if !staged.staged_at.is_empty() {
            line.push_str(&format!(" staged_at={}", wire_quoted(&staged.staged_at)));
        }
        line.push_str(&format!(
            " staged_dir={}",
            wire_quoted(&staged.dir.display().to_string())
        ));
    }
    if let Some(installed) = &here.installed {
        line.push_str(&format!(" installed_build={}", installed.build_number));
    }
    let outcome = match standing {
        Standing::Newer(staged) | Standing::Differs(staged) if staged.smoke == Smoke::Fail => {
            "a build is staged but failed its smoke test".to_string()
        }
        Standing::Newer(_) | Standing::Differs(_) => {
            let what = if matches!(standing, Standing::Newer(_)) {
                "a newer build is staged"
            } else {
                "the staged build has this build number and is another build"
            };
            here.blocked.as_ref().map_or_else(
                || format!("{what}: update apply installs it"),
                |why| format!("{what}; update apply cannot install it: {why}"),
            )
        }
        Standing::Installed(_) => {
            "the staged build is installed: restart aterm to use it".to_string()
        }
        Standing::Same(_) => "this is the newest staged build".to_string(),
        Standing::Older(_) => "what is installed is newer than the staged build".to_string(),
        Standing::Unstamped(_) => "this copy carries no build number to compare".to_string(),
        Standing::Absent => "no staged build".to_string(),
        Standing::Unreadable(why) => format!("can\u{2019}t read the staged build pointer: {why}"),
    };
    line.push_str(&format!(" outcome={}", wire_quoted(&outcome)));
    line
}

/// Whether `name` is a dist folder's name as the build loop writes one: twelve hex digits.
fn is_dist_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.len() == 12 && name.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Where `apply` installs: the folder the running executable is in, unless that folder is not
/// a per-user install the dist's installer can replace (a build tree, an MSIX package, the
/// staged dist itself, or another dist folder the build loop keeps beside it).
pub fn install_dest(running_exe: &Path, staged: &StagedBuild) -> Result<PathBuf, String> {
    let dest = running_exe
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| format!("{} has no folder", running_exe.display()))?;
    // A profile folder holds `deps` and `.fingerprint`; a test or an example runs from a
    // folder inside it (`deps`, `examples`).
    let cargo_profile = |dir: &Path| dir.join("deps").is_dir() || dir.join(".fingerprint").is_dir();
    if cargo_profile(dest)
        || dest
            .parent()
            .is_some_and(|up| up.join(".fingerprint").is_dir())
    {
        return Err(format!(
            "this copy runs from a build tree ({}), not an install \u{2014} run {} yourself to \
             install the staged build",
            dest.display(),
            staged.installer().display(),
        ));
    }
    if dest
        .components()
        .any(|part| part.as_os_str().eq_ignore_ascii_case("WindowsApps"))
    {
        return Err(format!(
            "this copy is an MSIX package ({}), which Windows updates \u{2014} install a newer \
             package, or run {} for a per-user install",
            dest.display(),
            staged.installer().display(),
        ));
    }
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(dest, &staged.dir) {
        return Err(format!(
            "this copy runs from the staged dist folder itself ({}) \u{2014} run its install.ps1 \
             to install it",
            dest.display(),
        ));
    }
    // A dist the build loop keeps (the one staged before, the installed one's source) sits
    // beside the staged one under its commit's name. Installing over it would leave a folder
    // whose name is no longer its build, which the loop's retention reads.
    let beside_staged = dest
        .parent()
        .zip(staged.dir.parent())
        .is_some_and(|(a, b)| same(a, b));
    if beside_staged && dest.file_name().is_some_and(is_dist_name) {
        return Err(format!(
            "this copy runs from a staged dist folder ({}), not an install \u{2014} run {} to \
             install the staged build",
            dest.display(),
            staged.installer().display(),
        ));
    }
    Ok(dest.to_path_buf())
}

/// How the installer is asked to install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyMode {
    /// `-Staged`: replace the images on disk and stop nothing.
    Staged,
    /// `-Restart`: save the open windows, stop the installed copy, install, start it again.
    Restart,
}

/// The installer's arguments after the PowerShell executable (`install.ps1`'s own contract:
/// every lay is staged, `-Restart` also saves the open windows, stops the installed copy and
/// starts the new build once the lay went in, and it exits 0 or 1 with one line that says why).
#[must_use]
pub fn installer_args(
    staged: &StagedBuild,
    dest: &Path,
    mode: ApplyMode,
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
        .into_iter()
        .map(Into::into)
        .collect();
    args.push(staged.installer().into_os_string());
    args.push(
        match mode {
            ApplyMode::Staged => "-Staged",
            ApplyMode::Restart => "-Restart",
        }
        .into(),
    );
    args.push("-Dest".into());
    args.push(dest.as_os_str().to_os_string());
    // An update replaces the build. The shortcuts and the PATH entry are the first install's
    // decisions (they name the folder, which does not move) and are not made again.
    args.push("-NoShortcuts".into());
    args.push("-NoPath".into());
    args
}

/// The one line of a failed installer's output that says why: its own `install.ps1: <reason>`
/// line when it wrote one, else the last line it wrote.
#[must_use]
pub fn failure_reason(output: &str) -> Option<&str> {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let mut last = None;
    for line in &mut lines {
        if let Some(reason) = line.strip_prefix("install.ps1: ") {
            return Some(reason);
        }
        last = Some(line);
    }
    last
}

/// What must hold before the installer runs: the build is offered and its installer is there.
pub fn apply_refusal(standing: &Standing) -> Result<&StagedBuild, String> {
    let staged = match standing {
        Standing::Newer(staged) | Standing::Differs(staged) if staged.smoke == Smoke::Fail => {
            return Err(format!(
                "{} failed its smoke test, so it is not installed \u{2014} `{STAGE_COMMAND}` \
                 builds another",
                staged.name()
            ));
        }
        Standing::Newer(staged) | Standing::Differs(staged) => staged,
        Standing::Installed(staged) => {
            return Err(format!(
                "{} is already installed \u{2014} restart aterm to use it",
                staged.name()
            ));
        }
        Standing::Same(_) | Standing::Older(_) => {
            return Err(format!(
                "nothing to install: this copy is current \u{2014} `{STAGE_COMMAND}` builds a \
                 newer one"
            ));
        }
        Standing::Unstamped(_) => {
            return Err(
                "this copy carries no build number (an unstamped build), so it cannot be \
                 compared with the staged build"
                    .to_string(),
            );
        }
        Standing::Absent => {
            return Err(format!(
                "no staged build \u{2014} `{STAGE_COMMAND}` builds one and stages it"
            ));
        }
        Standing::Unreadable(why) => {
            return Err(format!("can\u{2019}t read the staged build pointer: {why}"));
        }
    };
    missing_installer(staged).map_or(Ok(staged), Err)
}

/// Why `staged` cannot be installed for want of its installer (its dist was removed, or
/// staged without one); `None` when `install.ps1` is there.
fn missing_installer(staged: &StagedBuild) -> Option<String> {
    (!staged.installer().is_file()).then(|| {
        format!(
            "the staged build has no installer: {} is missing \u{2014} `{STAGE_COMMAND}` stages \
             a whole one",
            staged.installer().display()
        )
    })
}

/// The window's watch: look now and then every [`WATCH_EVERY`], and tell `notify` each time
/// what the window should say changes: a build that becomes offered, one that becomes
/// installed under the window, and the withdrawal of either (a pointer that now names a build
/// that failed its smoke test, or no build). The first look says nothing when there is
/// nothing to say.
pub fn watch_with(
    look: &mut dyn FnMut() -> Notice,
    notify: &mut dyn FnMut(Notice),
    wait: &mut dyn FnMut(Duration) -> bool,
) {
    let mut told: (u8, Option<u64>) = (Notice::Nothing.kind(), None);
    loop {
        let notice = look();
        let now = (notice.kind(), notice.build_number());
        if now != told {
            if let Notice::Offered(staged) | Notice::Installed(staged) = &notice {
                aterm_log::info!(
                    "aterm-update: {} is {} ({})",
                    staged.name(),
                    if matches!(notice, Notice::Offered(_)) {
                        "staged"
                    } else {
                        "installed under this window"
                    },
                    staged.dir.display()
                );
            } else {
                aterm_log::info!("aterm-update: the staged build is no longer offered");
            }
            told = now;
            notify(notice);
        }
        if !wait(WATCH_EVERY) {
            return;
        }
    }
}

/// The pointer of this user: `%LOCALAPPDATA%\aterm\dist\latest.json`.
#[cfg(windows)]
#[must_use]
pub fn pointer_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|dir| !dir.is_empty())
        .map(|dir| pointer_path_under(Path::new(&dir)))
}

/// [`look`] from this executable at this user's pointer. Every call reads the pointer and the
/// record beside the executable again: both change under a running window.
#[cfg(windows)]
#[must_use]
pub fn here(running: Running<'_>) -> Here {
    let Some(pointer) = pointer_path() else {
        return Here {
            standing: Standing::Unreadable("%LOCALAPPDATA% is not set".to_string()),
            pointer: PathBuf::from("%LOCALAPPDATA%\\aterm\\dist\\latest.json"),
            installed: None,
            blocked: None,
        };
    };
    look(running, std::env::current_exe().ok().as_deref(), &pointer)
}

/// Start the window's watch on its own thread. It wakes once per [`WATCH_EVERY`], reads two
/// small files, and calls `notify` only when what the window should say changes.
#[cfg(windows)]
pub fn spawn_watch(running: Running<'static>, notify: Box<dyn Fn(Notice) + Send>) -> bool {
    if pointer_path().is_none() {
        return false;
    }
    std::thread::Builder::new()
        .name("aterm-staged-build".into())
        .spawn(move || {
            watch_with(
                &mut || here(running).notice(),
                &mut |notice| notify(notice),
                &mut |period| {
                    std::thread::sleep(period);
                    true
                },
            );
        })
        .is_ok()
}

/// How the installer ended.
#[cfg(windows)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// The build installed.
    pub staged: StagedBuild,
    /// Where it was installed.
    pub dest: PathBuf,
}

/// Whose terminal the installer writes to.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallerOutput {
    /// The caller's own stdout and stderr (a person at a prompt reads it as it runs).
    Inherit,
    /// Captured: the failure's reason is the installer's last line.
    Capture,
}

#[cfg(windows)]
fn powershell() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(|root| {
            Path::new(&root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        })
        .filter(|exe| exe.is_file())
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

/// Install the offered staged build over the copy `running_exe` belongs to, by running the
/// dist's own `install.ps1`, and report how it ended. [`ApplyMode::Restart`] always runs the
/// installer with no terminal: it stops the copy that is waiting on it.
#[cfg(windows)]
pub fn apply(
    standing: &Standing,
    running_exe: &Path,
    mode: ApplyMode,
    output: InstallerOutput,
) -> Result<Applied, String> {
    use std::process::Stdio;

    let staged = apply_refusal(standing)?;
    let dest = install_dest(running_exe, staged)?;
    let mut command = aterm_uds::spawnfd::command(powershell());
    command
        .args(installer_args(staged, &dest, mode))
        .stdin(Stdio::null());
    let capture = output == InstallerOutput::Capture && mode == ApplyMode::Staged;
    if mode == ApplyMode::Restart {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    } else if capture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    aterm_log::info!(
        "aterm-update: installing {} into {} ({mode:?})",
        staged.name(),
        dest.display()
    );
    let (status, said) = if capture {
        let out = command
            .output()
            .map_err(|e| format!("could not start PowerShell for the installer: {e}"))?;
        let mut said = String::from_utf8_lossy(&out.stdout).into_owned();
        said.push('\n');
        said.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status, said)
    } else {
        let status = command
            .status()
            .map_err(|e| format!("could not start PowerShell for the installer: {e}"))?;
        (status, String::new())
    };
    if status.success() {
        aterm_log::info!("aterm-update: installed {}", staged.name());
        return Ok(Applied {
            staged: staged.clone(),
            dest,
        });
    }
    let code = status
        .code()
        .map_or_else(|| "no exit code".to_string(), |code| format!("exit {code}"));
    let reason = failure_reason(&said).map_or_else(String::new, |line| format!(": {line}"));
    let failure = format!(
        "{} did not install {} ({code}){reason}",
        staged.installer().display(),
        staged.name()
    );
    aterm_log::warn!("aterm-update: {failure}");
    Err(failure)
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
