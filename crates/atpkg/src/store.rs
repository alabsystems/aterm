// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The manager-owned install layout (§10).

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use crate::manifest::DigestAlg;

/// The resolved, validated install layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The manager prefix, e.g. `~/Library/Application Support/aterm/pkg`.
    pub prefix: PathBuf,
}

impl Layout {
    /// Whether this is a system prefix (the root-owned shape) rather than a `$HOME` one, decided by the on-disk chain ([`system_chain_trusted`]), not by location.
    #[must_use]
    pub fn is_system_prefix(&self) -> bool {
        #[cfg(test)]
        if system_shape::forced(&self.prefix) {
            return true;
        }
        system_chain_trusted(&self.prefix)
    }

    /// Create and harden a directory of this layout with the mode the prefix shape calls for.
    pub fn ensure_dir(&self, dir: &Path) -> std::io::Result<()> {
        let created = if self.is_system_prefix() {
            crate::platform::ensure_shared_dir(dir)
        } else {
            crate::platform::ensure_private_dir(dir)
        };
        if dir == self.prefix {
            crate::platform::exclude_from_backup(dir);
        }
        created
    }

    /// Ensure `agents/` ([`Self::agents_dir`]) exists before a session puts it on `PATH` ([`crate::reroute::AGENTS_DIR_ENV`], [`crate::activate::reconcile_agents`]).
    pub fn ensure_agents_dir(&self) -> Result<PathBuf, String> {
        let dir = self.agents_dir();
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(format!("{} is a symlink", dir.display()));
            }
            Ok(md) if !md.is_dir() => {
                return Err(format!("{} exists and is not a directory", dir.display()));
            }
            _ => {}
        }
        self.ensure_dir(&dir).map_err(|error| {
            let text = error.to_string();
            if text.starts_with(&dir.display().to_string()) {
                text
            } else {
                format!("{}: {text}", dir.display())
            }
        })?;
        match std::fs::symlink_metadata(&dir) {
            Ok(md) if md.file_type().is_symlink() => Err(format!("{} is a symlink", dir.display())),
            Ok(md) if md.is_dir() => Ok(dir),
            Ok(_) => Err(format!("{} exists and is not a directory", dir.display())),
            Err(error) => Err(format!("{}: {error}", dir.display())),
        }
    }

    /// `store/<program>/<build>/` — the versioned, immutable extracted tree.
    #[inline]
    #[must_use]
    pub fn build_dir(&self, program: &str, build: u64) -> PathBuf {
        aterm_toolchain::layout::build_dir(&self.prefix, program, build)
    }

    /// `bin/` — the only directory placed on the child `PATH` (shims into `current`).
    #[inline]
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.prefix.join("bin")
    }

    /// `reroute/`: session-scoped directory of upstream-name stubs ([`crate::reroute`], [`crate::hooks`]).
    #[inline]
    #[must_use]
    pub fn reroute_dir(&self) -> PathBuf {
        crate::reroute::dir(self)
    }

    /// `agents/`: the one managed directory that goes first on `PATH` (the shell hook prepends it, [`crate::hooks`]; the GUI's spawn seam front-inserts it beside `reroute/`).
    #[inline]
    #[must_use]
    pub fn agents_dir(&self) -> PathBuf {
        self.prefix.join("agents")
    }

    /// `agents/<tool>` — an agent program's front-of-`PATH` shim ([`Self::agents_dir`]); the same [`ToolName`] gate as [`Self::shim`].
    #[inline]
    #[must_use]
    pub fn agent_shim(&self, tool: &ToolName) -> PathBuf {
        self.agents_dir().join(tool.shim_file())
    }

    /// `landing/`: where older passes wrote landing markers, which twins those clients laid still test for ([`crate::landing`]).
    #[inline]
    #[must_use]
    pub fn landing_dir(&self) -> PathBuf {
        self.prefix.join("landing")
    }

    /// `bin/<tool>`: a single shim, named by [`ToolName::shim_file`] (`bin/ay` on Unix, `bin/ay.cmd` on Windows).
    #[inline]
    #[must_use]
    pub fn shim(&self, tool: &ToolName) -> PathBuf {
        self.bin_dir().join(tool.shim_file())
    }

    /// `store/<program>/current`.
    #[inline]
    #[must_use]
    pub fn program_current(&self, program: &str) -> PathBuf {
        self.prefix.join("store").join(program).join("current")
    }

    /// `staging/<program>/` — the per-program download + stage scratch.
    #[inline]
    #[must_use]
    pub fn staging_dir(&self, program: &str) -> PathBuf {
        self.prefix.join("staging").join(program)
    }

    /// `floor` — the `0600` durable high-water `index_build` file (§8).
    #[inline]
    #[must_use]
    pub fn floor(&self) -> PathBuf {
        self.prefix.join("floor")
    }

    /// `floor.gen`: the `0600` roster generation that recorded the current [`Self::floor`].
    #[inline]
    #[must_use]
    pub fn floor_generation(&self) -> PathBuf {
        self.prefix.join("floor.gen")
    }

    /// `roster.floor`: the `0600` durable high-water `roster_seq`, the replay ratchet for the master-signed machine roster that authorizes this store's index.
    #[inline]
    #[must_use]
    pub fn roster_floor(&self) -> PathBuf {
        self.prefix.join("roster.floor")
    }

    /// `store.lock` — the `0600` store-wide single-writer advisory lock file ([`crate::lock`]).
    #[inline]
    #[must_use]
    pub fn store_lock(&self) -> PathBuf {
        self.prefix.join("store.lock")
    }

    /// `status.toml` — the aggregate observability record.
    #[inline]
    #[must_use]
    pub fn status(&self) -> PathBuf {
        self.prefix.join("status.toml")
    }

    /// `machine-apply.stamp`.
    #[inline]
    #[must_use]
    pub fn machine_apply_stamp(&self) -> PathBuf {
        self.prefix.join("machine-apply.stamp")
    }

    /// `session-pass.stamp`.
    #[inline]
    #[must_use]
    pub fn session_pass_stamp(&self) -> PathBuf {
        self.prefix.join("session-pass.stamp")
    }

    /// `progress.json` — the live install-progress snapshot ([`crate::progress`]), the in-flight complement to [`Self::status`]'s durable per-pass record.
    #[inline]
    #[must_use]
    pub fn progress_file(&self) -> PathBuf {
        self.prefix.join("progress.json")
    }

    /// `bump` — the priority-queue channel ([`crate::progress`]).
    #[inline]
    #[must_use]
    pub fn bump_file(&self) -> PathBuf {
        self.prefix.join("bump")
    }

    /// `pass-owed.json` — the promise that a default-set pass is coming and has not planned yet ([`crate::progress::pass_owed`]).
    #[inline]
    #[must_use]
    pub fn pass_owed_file(&self) -> PathBuf {
        self.prefix.join("pass-owed.json")
    }

    /// `adopted`: the durable marker that this machine runs the ALab toolset as a set, not as a handful of individually chosen programs (§11).
    #[inline]
    #[must_use]
    pub fn adopted(&self) -> PathBuf {
        self.prefix.join("adopted")
    }

    /// `adopted-by-seed`: present only when `atpkg seed`, the first launch's bootstrap, created the [`Self::adopted`] marker itself.
    #[inline]
    #[must_use]
    pub fn adopted_by_seed(&self) -> PathBuf {
        self.prefix.join("adopted-by-seed")
    }

    /// `removed`: programs the user uninstalled individually, one name per line.
    #[inline]
    #[must_use]
    pub fn removed(&self) -> PathBuf {
        self.prefix.join("removed")
    }

    /// The programs this machine removed on purpose.
    #[must_use]
    pub fn removed_programs(&self) -> std::collections::BTreeSet<String> {
        std::fs::read_to_string(self.removed())
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty() && !line.starts_with('#'))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `declined`: the durable "this machine does not want the bundled toolset" marker, written by `uninstall --all`.
    #[inline]
    #[must_use]
    pub fn declined(&self) -> PathBuf {
        self.prefix.join("declined")
    }

    /// `retired/`: per-program retirement markers.
    #[inline]
    #[must_use]
    pub fn retired_dir(&self) -> PathBuf {
        self.prefix.join("retired")
    }

    /// `retired/<program>`: one retirement marker; its callers gate the name by [`ToolName`].
    #[inline]
    #[must_use]
    pub fn retired_marker(&self, program: &str) -> PathBuf {
        self.retired_dir().join(program)
    }

    /// The `YYYY-MM-DD` day a managed copy of `program` was retired for a system install, if recorded.
    #[must_use]
    pub fn retired_date(&self, program: &str) -> Option<String> {
        ToolName::new(program)?;
        let path = self.retired_marker(program);
        if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            return None;
        }
        let text = crate::metadata_io::read_bounded_regular_utf8(&path, 4096).ok()?;
        let first = text.lines().next()?.trim();
        let shaped = first.len() == 10
            && first.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            });
        shaped.then(|| first.to_string())
    }

    /// Record that `program`'s managed copy was retired on `date` (`YYYY-MM-DD`).
    pub fn record_retired(&self, program: &str, date: &str) -> std::io::Result<()> {
        if ToolName::new(program).is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "retirement name is not an installable program name",
            ));
        }
        let dir = self.retired_dir();
        self.ensure_dir(&dir)?;
        let path = self.retired_marker(program);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {
                let _ = std::fs::remove_file(&path);
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "retirement marker path is occupied by something that is not a marker",
                ));
            }
            Err(_) => {}
        }
        let mut f = crate::platform::open_create_write(&path, 0o600)?;
        use std::io::Write as _;
        f.write_all(date.as_bytes())?;
        f.write_all(
            b"
# The day atpkg retired its managed copy of this program because a system install of
# the same name appeared on PATH. Read back by `aterm pkg which` and the status row.
",
        )
    }

    /// Forget the retirement record for `program` (a no-op when none exists).
    pub fn clear_retired(&self, program: &str) {
        if ToolName::new(program).is_none() {
            return;
        }
        let path = self.retired_marker(program);
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_file()) {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// Forget every retirement record (`uninstall --all`).
    pub fn clear_all_retired(&self) {
        let Ok(entries) = std::fs::read_dir(self.retired_dir()) else {
            return;
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                self.clear_retired(name);
            }
        }
    }

    /// `links/` — the per-program dev-link markers directory (§13).
    #[inline]
    #[must_use]
    pub fn links_dir(&self) -> PathBuf {
        self.prefix.join("links")
    }

    /// `links/<program>` — one dev-link marker.
    #[inline]
    #[must_use]
    pub fn link_marker(&self, program: &str) -> PathBuf {
        self.links_dir().join(program)
    }
}

/// The one stderr line for a refused `agents/` ([`Layout::ensure_agents_dir`]), shared by the front door and the window.
#[must_use]
pub fn agents_dir_refusal_line(dir: &Path, error: &str, system: bool, reach: &str) -> String {
    let remedy = match std::fs::symlink_metadata(dir) {
        Ok(md) if md.file_type().is_symlink() => "remove that symlink, then run `aterm pkg repair`",
        Ok(md) if !md.is_dir() => "remove that file, then run `aterm pkg repair`",
        _ if system => "run `aterm pkg repair` as root",
        _ => "run `aterm pkg repair`",
    };
    format!(
        "aterm: the managed `claude`/`codex` are not first on PATH in {reach}: {error} — {remedy}"
    )
}

fn ready_marker_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(format!("{name}.ready")))
}

const READY_PLATFORM_KEY: &str = "platform=";

const READY_CONTENTS_KEY: &str = "contents=";

const READY_CONTENTS_NONE: &str = "none";

const READY_CONTENTS_BIN: &str = "bin:";

const READY_OK_LINE: &str = "ok";

fn running_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn recorded_platform(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(READY_PLATFORM_KEY))
        .map(str::trim)
        .find(|p| !p.is_empty())
}

fn recorded_contents(text: &str) -> Option<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(READY_CONTENTS_KEY))
        .map(str::trim)
        .find(|v| !v.is_empty())
}

fn bin_inventory(build_dir: &Path) -> Option<String> {
    let bin = build_dir.join("bin");
    let entries = match std::fs::read_dir(&bin) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(String::from(READY_CONTENTS_NONE));
        }
        Err(_) => return None,
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name().into_string().ok()?;
        if name.contains(',') || name.contains('\n') || name.contains('\r') || name.is_empty() {
            return None;
        }
        names.push(name);
    }
    if names.is_empty() {
        return Some(String::from(READY_CONTENTS_NONE));
    }
    names.sort();
    names.dedup();
    Some(format!("{READY_CONTENTS_BIN}{}", names.join(",")))
}

fn contents_still_stand(build_dir: &Path, recorded: &str) -> bool {
    if recorded == READY_CONTENTS_NONE {
        return true;
    }
    let Some(list) = recorded.strip_prefix(READY_CONTENTS_BIN) else {
        return true;
    };
    let bin = build_dir.join("bin");
    !list
        .split(',')
        .filter(|n| !n.is_empty())
        .any(|name| is_provably_absent(&bin.join(name)))
}

fn is_provably_absent(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

fn ready_text_accepts(text: &str, running: &str) -> bool {
    if !first_line_is_ok(text) {
        return false;
    }
    match recorded_platform(text) {
        None => true,
        Some(recorded) => recorded == running,
    }
}

fn ready_contents_accept(text: &str, build_dir: &Path) -> bool {
    match recorded_contents(text) {
        None => true,
        Some(recorded) => contents_still_stand(build_dir, recorded),
    }
}

fn first_line_is_ok(text: &str) -> bool {
    matches!(text.lines().next(), Some(first) if first.trim() == READY_OK_LINE)
}

/// What this process could learn about whether `path` is there: three answers.
#[derive(Debug)]
pub enum Presence {
    Present,
    Absent,
    Unknown(std::io::Error),
}

impl Presence {
    /// True only when the path is provably not there.
    #[inline]
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }
}

/// Ask the filesystem about `path`, keeping the three answers apart.
#[must_use]
pub fn presence(path: &Path) -> Presence {
    match std::fs::metadata(path) {
        Ok(_) => Presence::Present,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Presence::Absent,
        Err(e) => Presence::Unknown(e),
    }
}

/// Whether `build_dir` holds a complete install for the running slice.
#[must_use]
pub fn build_is_complete(build_dir: &Path) -> bool {
    let Some(marker) = ready_marker_path(build_dir) else {
        return false;
    };
    if is_provably_absent(build_dir) {
        return false;
    }
    match std::fs::read_to_string(&marker) {
        Ok(text) => {
            ready_text_accepts(&text, &running_platform())
                && ready_contents_accept(&text, build_dir)
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => false,
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Whether `build_dir` carries a well-formed marker recording the other slice of the universal binary.
pub(crate) fn build_marks_another_slice(build_dir: &Path) -> bool {
    let Some(marker) = ready_marker_path(build_dir) else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&marker) else {
        return false;
    };
    first_line_is_ok(&text)
        && recorded_platform(&text).is_some_and(|p| p != running_platform().as_str())
}

/// Atomically and durably mark `build_dir` complete (temp + fsync + rename, so a crash leaves no marker rather than a half-written one).
pub fn mark_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let dest = ready_marker_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let parent = dest.parent().unwrap_or(build_dir);
    let tmp = parent.join(format!(".ready.tmp-{}", std::process::id()));
    let mut body = format!(
        "{READY_OK_LINE}\n{READY_PLATFORM_KEY}{}\n",
        running_platform()
    );
    if let Some(contents) = bin_inventory(build_dir) {
        body.push_str(READY_CONTENTS_KEY);
        body.push_str(&contents);
        body.push('\n');
    }
    {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, 0o644)?;
        f.write_all(body.as_bytes())?;
        sync_contents_or_accept_refusal(&f)?;
    }
    std::fs::rename(&tmp, &dest)
}

/// Remove the completeness marker, making `build_dir` read as not installed.
pub(crate) fn clear_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let Some(marker) = ready_marker_path(build_dir) else {
        return Ok(());
    };
    match std::fs::remove_file(&marker) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Take down the completeness marker of every build under `prog_store` (`<prefix>/store/<program>`), durably, before a caller removes the tree.
pub(crate) fn unmark_program_builds(prog_store: &Path) {
    let Ok(entries) = std::fs::read_dir(prog_store) else {
        return;
    };
    let mut unmarked = false;
    for entry in entries.flatten() {
        // Numeric build dirs only, by the same test every other store scan uses: `current`
        // is a symlink, and `<n>.ready` / `<n>.provenance` / `<n>.shim-env` are files.
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.parse::<u64>().is_err() {
            continue;
        }
        if clear_build_ready(&entry.path()).is_ok() {
            unmarked = true;
        }
    }
    if unmarked {
        // Ordered: every marker is gone before any tree goes; a lost removal leaves a
        // marked tree for the next uninstall to unmark again.
        sync_dir_ordered(prog_store);
    }
}

/// The suffix of the declared-mode record beside a build dir: `store/<program>/<build>.modes`.
pub const DECLARED_MODES_SUFFIX: &str = ".modes";

const DECLARED_MODES_HEADER: &str = "declared-modes v1";

const MAX_DECLARED_MODES_BYTES: usize = 16 * 1024 * 1024;

/// `store/<program>/<build>.modes` for `build_dir`, or `None` for a path with no name.
#[must_use]
pub fn declared_modes_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(DECLARED_MODES_SUFFIX);
    Some(build_dir.with_file_name(sidecar))
}

/// Write the declared-mode record beside `build_dir` durably ([`write_sidecar_durably`]).
pub fn write_declared_modes(
    build_dir: &Path,
    modes: &crate::tree::DeclaredModes,
) -> std::io::Result<()> {
    if crate::platform::HAS_POSIX_MODES {
        return Ok(());
    }
    if let Some(parent) = build_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body: Vec<u8> = Vec::with_capacity(modes.len().saturating_mul(48).saturating_add(32));
    body.extend_from_slice(DECLARED_MODES_HEADER.as_bytes());
    body.push(b'\n');
    for (rel, mode) in modes {
        body.extend_from_slice(rel);
        body.push(0);
        body.extend_from_slice(crate::tree::oct_mode(*mode).as_bytes());
        body.push(b'\n');
    }
    write_sidecar_durably(build_dir, DECLARED_MODES_SUFFIX, &body)
}

/// The declared-mode record beside `build_dir`, for [`crate::tree::tree_root_declared`].
pub fn declared_modes(build_dir: &Path) -> std::io::Result<crate::tree::DeclaredModes> {
    if crate::platform::HAS_POSIX_MODES {
        return Ok(crate::tree::DeclaredModes::new());
    }
    let path = declared_modes_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let bytes =
        crate::metadata_io::read_bounded_regular(&path, MAX_DECLARED_MODES_BYTES).map_err(|e| {
            let mut m = String::from("no declared-mode record beside the build (");
            m.push_str(&e.to_string());
            m.push_str("): staged before this platform kept one; `aterm pkg install` re-stages it");
            std::io::Error::new(e.kind(), m)
        })?;
    parse_declared_modes(&bytes).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the declared-mode record beside the build is malformed",
        )
    })
}

fn parse_declared_modes(bytes: &[u8]) -> Option<crate::tree::DeclaredModes> {
    let mut lines = bytes.split(|&b| b == b'\n');
    if lines.next()? != DECLARED_MODES_HEADER.as_bytes() {
        return None;
    }
    let mut modes = crate::tree::DeclaredModes::new();
    for line in lines {
        if line.is_empty() {
            // The trailing newline's empty tail; an empty line anywhere else is
            // equally content-free.
            continue;
        }
        let nul = line.iter().position(|&b| b == 0)?;
        let (rel, digits) = (line.get(..nul)?, line.get(nul.saturating_add(1)..)?);
        if rel.is_empty() || digits.is_empty() || digits.len() > 4 {
            return None;
        }
        let mut mode: u32 = 0;
        for &d in digits {
            if !(b'0'..=b'7').contains(&d) {
                return None;
            }
            mode = mode.wrapping_shl(3) | u32::from(d.wrapping_sub(b'0'));
        }
        modes.insert(rel.to_vec(), mode);
    }
    Some(modes)
}

/// Remove the declared-mode record beside `build_dir`, if any.
pub fn clear_declared_modes(build_dir: &Path) {
    if let Some(sidecar) = declared_modes_path(build_dir) {
        let _ = std::fs::remove_file(sidecar);
    }
}

/// The suffix of the stage-refusal memo beside a build dir (`<build>.refused`).
pub const STAGE_REFUSAL_SUFFIX: &str = ".refused";

const STAGE_REFUSAL_HEADER: &str = "stage-refusal v1";

const MAX_REFUSAL_BYTES: usize = 4 * 1024;

const REFUSAL_COOLDOWN_BASE_SECS: i64 = 12 * 60 * 60;

const REFUSAL_COOLDOWN_CAP_SECS: i64 = 7 * 24 * 60 * 60;

const MAX_REFUSAL_WHY_CHARS: usize = 300;

/// What a deterministic stage failure recorded beside `store/<program>/<build>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRefusal {
    /// The algorithm of [`Self::digest`].
    pub alg: DigestAlg,
    /// The signed payload digest of the artifact this refusal is about.
    pub digest: String,
    /// The signed `tree_root` of that artifact (empty when the manifest carries none).
    pub tree_root: String,
    /// The stage's own sentence ([`crate::install::StageError`]'s `Display`).
    pub why: String,
    /// When it was recorded (Unix epoch second).
    pub at: i64,
    /// How many consecutive attempts have failed on these digests (>= 1).
    pub attempts: u32,
    /// The platform signer check refused bytes that matched these digests ([`crate::install::StageError::SignerRefused`]).
    pub signer: bool,
    /// Whether the row is held rather than cooled down: a `tree_root` mismatch.
    pub held: bool,
    /// The atpkg version that reached the verdict ([`aterm_types::version::app_version`]).
    pub client: String,
}

impl StageRefusal {
    /// The cooldown this record's attempt count buys.
    #[inline]
    #[must_use]
    pub const fn cooldown_secs(&self) -> i64 {
        let Some(steps) = self.attempts.checked_sub(2) else {
            return 0;
        };
        let shift = if steps < 8 { steps } else { 8 };
        let scaled = REFUSAL_COOLDOWN_BASE_SECS.saturating_mul(1_i64 << shift);
        if scaled < REFUSAL_COOLDOWN_CAP_SECS {
            scaled
        } else {
            REFUSAL_COOLDOWN_CAP_SECS
        }
    }

    /// The epoch second at which a new attempt is due.
    #[inline]
    #[must_use]
    pub const fn retry_after(&self) -> i64 {
        self.at.saturating_add(self.cooldown_secs())
    }

    /// Whether this record still stands in the way of re-staging an artifact with these signed digests at `now_unix`.
    #[inline]
    #[must_use]
    pub fn binds(&self, digest: (DigestAlg, &str), tree_root: &str, now_unix: i64) -> bool {
        self.same_digests(digest, tree_root)
            && self.client == aterm_types::version::app_version()
            && (self.signer || self.held || now_unix < self.retry_after())
    }

    #[inline]
    fn same_digests(&self, (alg, digest): (DigestAlg, &str), tree_root: &str) -> bool {
        !self.digest.is_empty()
            && self.alg == alg
            && self.digest.eq_ignore_ascii_case(digest)
            && self.tree_root.eq_ignore_ascii_case(tree_root)
    }
}

fn stage_refusal_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut marker = String::from(name);
    marker.push_str(STAGE_REFUSAL_SUFFIX);
    Some(build_dir.with_file_name(marker))
}

/// Record that staging `build_dir` failed on the signed digests (temp + rename, so a crash leaves no half-written memo).
pub fn record_stage_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
        RefusalKind::Cooldown,
    )
}

/// [`record_stage_refusal`] for a platform signer refusal ([`StageRefusal::signer`]).
pub(crate) fn record_signer_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
        RefusalKind::Signer,
    )
}

/// [`record_stage_refusal`] for a `tree_root` mismatch.
pub(crate) fn record_held_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
        RefusalKind::Held,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefusalKind {
    Cooldown,
    Signer,
    Held,
}

fn write_stage_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
    kind: RefusalKind,
) -> std::io::Result<()> {
    let dest = stage_refusal_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let attempts = stage_refusal(build_dir)
        .filter(|prior| prior.same_digests(digest, tree_root))
        .map_or(1, |prior| prior.attempts.saturating_add(1));
    let parent = dest.parent().unwrap_or(build_dir);
    make_build_parent(build_dir)?;
    let tmp = parent.join(format!(".refused.tmp-{}", std::process::id()));
    let mut body = String::from(STAGE_REFUSAL_HEADER);
    body.push('\n');
    body.push_str(digest.0.key());
    body.push('=');
    body.push_str(digest.1);
    body.push_str("\ntree_root=");
    body.push_str(tree_root);
    body.push_str("\nattempts=");
    body.push_str(&attempts.to_string());
    body.push_str("\nat=");
    body.push_str(&u64::try_from(now_unix).unwrap_or(0).to_string());
    match kind {
        RefusalKind::Signer => body.push_str("\nsigner=1"),
        RefusalKind::Held => body.push_str("\nhold=1"),
        RefusalKind::Cooldown => {}
    }
    body.push_str("\nclient=");
    body.push_str(aterm_types::version::app_version());
    body.push_str("\nwhy=");
    let one_line: String = why
        .replace(['\n', '\r'], " ")
        .chars()
        .take(MAX_REFUSAL_WHY_CHARS)
        .collect();
    body.push_str(&one_line);
    body.push('\n');
    write_record_temp(&tmp, body.as_bytes())?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(name) = dest.file_name() {
        sweep_other_refusals(parent, name);
    }
    Ok(())
}

fn sweep_other_refusals(dir: &Path, keep: &OsStr) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == keep || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if name
            .to_str()
            .is_some_and(|n| n.ends_with(STAGE_REFUSAL_SUFFIX))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The refusal recorded beside `build_dir`, if one is there and reads.
#[must_use]
pub fn stage_refusal(build_dir: &Path) -> Option<StageRefusal> {
    let path = stage_refusal_path(build_dir)?;
    let text = crate::metadata_io::read_bounded_regular_utf8(&path, MAX_REFUSAL_BYTES).ok()?;
    parse_stage_refusal(&text)
}

fn parse_stage_refusal(text: &str) -> Option<StageRefusal> {
    let mut lines = text.lines();
    if lines.next()?.trim() != STAGE_REFUSAL_HEADER {
        return None;
    }
    let (mut sha256, mut sha512, mut tree_root, mut why) = (None, None, None, None);
    let (mut at, mut attempts, mut signer) = (None, None, false);
    let (mut held, mut client) = (false, String::new());
    for line in lines {
        if let Some(v) = line.strip_prefix("sha256=") {
            sha256 = Some(v);
        } else if let Some(v) = line.strip_prefix("sha512=") {
            sha512 = Some(v);
        } else if let Some(v) = line.strip_prefix("tree_root=") {
            tree_root = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("why=") {
            why = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("at=") {
            at = v.trim().parse::<i64>().ok();
        } else if let Some(v) = line.strip_prefix("attempts=") {
            attempts = v.trim().parse::<u32>().ok().filter(|n| *n >= 1);
        } else if let Some(v) = line.strip_prefix("signer=") {
            signer = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("hold=") {
            held = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("client=") {
            client = v.trim().to_string();
        }
    }
    let (alg, digest) = match (sha256, sha512) {
        (Some(v), None) => (DigestAlg::Sha256, v),
        (None, Some(v)) => (DigestAlg::Sha512, v),
        _ => return None,
    };
    Some(StageRefusal {
        alg,
        digest: digest.to_string(),
        tree_root: tree_root?,
        why: why.unwrap_or_default(),
        at: at?,
        attempts: attempts?,
        signer,
        held,
        client,
    })
}

/// Remove the refusal beside `build_dir`, if any.
pub fn clear_stage_refusal(build_dir: &Path) {
    if let Some(marker) = stage_refusal_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
}

/// The suffix of a vendor-direct build's record (`store/<program>/<build>.vendor`).
pub(crate) const VENDOR_SIDECAR_SUFFIX: &str = ".vendor";

/// The sidecars a stage may write through [`crate::install::StageHooks::sidecars`].
pub(crate) const STAGE_SIDECAR_SUFFIXES: &[&str] = &[
    VENDOR_SIDECAR_SUFFIX,
    crate::shim_env::SIDECAR_SUFFIX,
    DECLARED_MODES_SUFFIX,
];

/// `<build><suffix>` beside `build_dir`, or `None` for a path with no UTF-8 file name.
pub(crate) fn sidecar_path(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(suffix);
    Some(build_dir.with_file_name(sidecar))
}

/// Write `<build><suffix>` durably: temp + fsync + rename, mode `0644` like `.ready` so a system prefix's readers can read it.
pub(crate) fn write_sidecar_durably(
    build_dir: &Path,
    suffix: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let dest = sidecar_path(build_dir, suffix).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let parent = dest.parent().unwrap_or(build_dir);
    let mut tmp_name = String::from(suffix);
    tmp_name.push_str(".tmp-");
    tmp_name.push_str(&std::process::id().to_string());
    let tmp = parent.join(tmp_name);
    let written = (|| {
        use std::io::Write as _;
        let mut f = crate::platform::open_create_write(&tmp, 0o644)?;
        f.write_all(bytes)?;
        sync_contents_or_accept_refusal(&f)?;
        drop(f);
        std::fs::rename(&tmp, &dest)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Remove the vendor record beside `build_dir`, if any.
pub(crate) fn clear_vendor_sidecar(build_dir: &Path) {
    if let Some(sidecar) = sidecar_path(build_dir, VENDOR_SIDECAR_SUFFIX) {
        let _ = std::fs::remove_file(sidecar);
    }
}

/// Forget every refusal recorded for `program`.
pub fn clear_stage_refusals(layout: &Layout, program: &str) -> usize {
    if crate::ops::uninstall_name_shape(program).is_err() {
        return 0;
    }
    let dir = layout.prefix.join("store").join(program);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut forgotten = 0usize;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(STAGE_REFUSAL_SUFFIX))
            && std::fs::remove_file(entry.path()).is_ok()
        {
            forgotten = forgotten.saturating_add(1);
        }
    }
    forgotten
}

/// [`clear_stage_refusals`] for every program the store has a directory for.
pub fn clear_all_stage_refusals(layout: &Layout) -> usize {
    let Ok(entries) = std::fs::read_dir(layout.prefix.join("store")) else {
        return 0;
    };
    let mut forgotten = 0usize;
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if let Some(program) = entry.file_name().to_str() {
            forgotten = forgotten.saturating_add(clear_stage_refusals(layout, program));
        }
    }
    forgotten
}

/// The sibling scratch directory a stage extracts into before it swaps: `<build>.incoming-<pid>`.
pub(crate) fn incoming_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".incoming-")
}

/// Write the temp of a small record renamed into place beside a build (the shim-environment sidecar, the stage-refusal memo), created `0644`, which the umask can only narrow.
pub(crate) fn write_record_temp(tmp: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(tmp);
    crate::platform::open_create_write(tmp, 0o644)?.write_all(bytes)
}

/// Create `store/<program>/` (and `store/`) if missing at mode `0755` ([`crate::extract::safe_mode`], [`crate::platform::make_dirs_born`]).
pub(crate) fn make_build_parent(build_dir: &Path) -> std::io::Result<()> {
    match build_dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            crate::platform::make_dirs_born(parent, crate::extract::safe_mode(0, true))
        }
        _ => Ok(()),
    }
}

/// The sibling the outgoing tree is moved to during a swap: `<build>.superseded-<pid>`.
pub(crate) fn superseded_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".superseded-")
}

fn scratch_sibling(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(format!("{name}{suffix}{}", std::process::id())))
}

/// Which of the two scratch shapes a stage produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scratch {
    Incoming,
    Superseded,
}

/// A store build directory's name, parsed the way this manager writes one.
pub(crate) fn parse_build_name(name: &str) -> Option<u64> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if name.len() > 1 && name.starts_with('0') {
        return None;
    }
    name.parse::<u64>().ok()
}

/// Recognise stage scratch by the producer's exact shape.
pub(crate) fn stage_scratch_of(name: &str) -> Option<(u64, Scratch)> {
    let (build, rest) = name.split_once('.')?;
    let build = parse_build_name(build)?;
    let (kind, pid) = match rest.strip_prefix("incoming-") {
        Some(pid) => (Scratch::Incoming, pid),
        None => (Scratch::Superseded, rest.strip_prefix("superseded-")?),
    };
    if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((build, kind))
}

fn scratch_siblings(build_dir: &Path) -> Vec<(Scratch, PathBuf)> {
    let (Some(parent), Some(name)) = (
        build_dir.parent(),
        build_dir.file_name().and_then(|n| n.to_str()),
    ) else {
        return Vec::new();
    };
    let Some(build) = parse_build_name(name) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let entry_name = entry.file_name();
            let (found, kind) = stage_scratch_of(entry_name.to_str()?)?;
            (found == build).then(|| (kind, entry.path()))
        })
        .collect()
}

/// Recover the one crash window a swap cannot make atomic; `true` when a tree was moved back.
pub(crate) fn recover_interrupted_swap(build_dir: &Path) -> bool {
    // `symlink_metadata`, not `exists()`: a dangling symlink at the build path is still
    // something there, and the window means nothing is.
    if std::fs::symlink_metadata(build_dir).is_ok() {
        return false;
    }
    let mut superseded = scratch_siblings(build_dir).into_iter().filter(|(kind, p)| {
        // A real directory, not a symlink to one: the outgoing tree the swap parked here is
        // a directory, and anything else is not the state this recovers.
        *kind == Scratch::Superseded && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
    });
    let Some((_, old)) = superseded.next() else {
        return false;
    };
    if superseded.next().is_some() {
        // Distinct branch.
        return false;
    }
    std::fs::rename(&old, build_dir).is_ok()
}

/// Delete every stage-scratch sibling of `build_dir` an earlier run left, after recovering the swap window ([`recover_interrupted_swap`]).
pub(crate) fn sweep_stage_scratch(build_dir: &Path) {
    let recovered = recover_interrupted_swap(build_dir);
    let parked_only_copy = !recovered && std::fs::symlink_metadata(build_dir).is_err();
    for (kind, path) in scratch_siblings(build_dir) {
        // Spare only a directory (what a swap parks); a file or symlink at that name is
        // nobody's only copy, and nothing else reclaims it.
        if parked_only_copy
            && kind == Scratch::Superseded
            && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => {
                let _ = remove_tree(&path);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&path);
            }
            Err(_) => {}
        }
    }
}

/// Attempt a full flush of this directory's metadata.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = crate::platform::sync_durable(&handle);
    }
}

/// Attempt an ORDERED flush of `dir` ([`aterm_durable::ordered`]).
pub(crate) fn sync_dir_ordered(dir: &Path) {
    if let Ok(handle) = std::fs::File::open(dir) {
        let _ = aterm_durable::ordered(&handle);
    }
}

/// [`crate::platform::sync_file_contents`], with a refusal degraded to success.
pub(crate) fn sync_contents_or_accept_refusal(f: &std::fs::File) -> std::io::Result<()> {
    match crate::platform::sync_file_contents(f) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// What [`sync_tree`] flushed.
#[derive(Debug, Default)]
pub(crate) struct Synced {
    /// Regular files whose contents were pushed to the filesystem.
    pub files: u64,
    /// Directories whose entries were pushed to the filesystem.
    pub dirs: u64,
}

/// Flush a staged tree (every regular file's contents, then every directory's entries) so nothing that publishes the tree can become durable ahead of the bytes it publishes.
pub(crate) fn sync_tree(root: &Path) -> std::io::Result<Synced> {
    let mut counts = Synced::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for child in std::fs::read_dir(&dir)? {
            let path = child?.path();
            let ft = std::fs::symlink_metadata(&path)?.file_type();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                match crate::platform::open_for_sync(&path) {
                    Ok(f) => {
                        sync_contents_or_accept_refusal(&f)?;
                        counts.files = counts.files.saturating_add(1);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {}
                    Err(e) => return Err(e),
                }
            }
        }
        if let Ok(handle) = std::fs::File::open(&dir) {
            sync_contents_or_accept_refusal(&handle)?;
            counts.dirs = counts.dirs.saturating_add(1);
        }
    }
    Ok(counts)
}

/// The mode of a single-binary build's `bin/` once its file is laid ([`seal_bin`]).
pub(crate) const SEALED_BIN_MODE: u32 = 0o555;

/// Seal a staged single-binary tree's `bin/` at [`SEALED_BIN_MODE`] ([`crate::install::verify_and_stage`], once every member has its final mode).
pub(crate) fn seal_bin(tree: &Path) -> std::io::Result<()> {
    crate::platform::set_mode(&tree.join("bin"), SEALED_BIN_MODE)
}

/// Give a sealed `<tree>/bin` ([`seal_bin`]) its owner's write bit back so the tree's removal can unlink what it holds.
pub(crate) fn unseal_bin(tree: &Path) {
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).ok().filter(|m| m.is_dir());
    let bin = tree.join("bin");
    if real_dir(tree).is_some()
        && real_dir(&bin)
            .is_some_and(|m| crate::platform::permission_mode(&m) & 0o7777 == SEALED_BIN_MODE)
    {
        let _ = crate::platform::set_mode(&bin, crate::extract::safe_mode(0, true));
    }
}

/// Remove the store tree at `dir` (a build, or a stage's `.incoming`/`.superseded` scratch), unsealing its `bin/` first ([`unseal_bin`]). std's `remove_dir_all` never follows a link.
pub(crate) fn remove_tree(dir: &Path) -> std::io::Result<()> {
    unseal_bin(dir);
    std::fs::remove_dir_all(dir)
}

/// Discard a build entirely.
pub(crate) fn discard_build(build_dir: &Path) {
    let _ = clear_build_ready(build_dir);
    if let Some(parent) = build_dir.parent() {
        sync_dir(parent);
    }
    crate::compat::discard_root_of(build_dir);
    let _ = remove_tree(build_dir);
    if let Some(marker) = ready_marker_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
    if let Some(name) = build_dir.file_name().and_then(|n| n.to_str()) {
        let mut prov = String::from(name);
        prov.push_str(".provenance");
        let _ = std::fs::remove_file(build_dir.with_file_name(prov));
    }
    crate::shim_env::remove_sidecar(build_dir);
    clear_stage_refusal(build_dir);
    clear_vendor_sidecar(build_dir);
    clear_declared_modes(build_dir);
}

/// The default prefix under `home`.
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    crate::platform::default_prefix(home)
}

/// Resolve the install layout.
#[must_use]
pub fn resolve(configured: Option<&Path>) -> Option<Layout> {
    let home = aterm_types::dirs::home_dir()?;
    let prefix = vet_prefix(configured, &home);
    #[cfg(test)]
    if let Some(asked) = configured {
        assert_eq!(
            prefix,
            asked.to_path_buf(),
            "vet_prefix rejected the prefix this test asked for and fell back to the \
             DEFAULT — which under test is the developer's real store. Give the test a \
             prefix the vet admits: strictly under $HOME, absolute, no `..`, and a real \
             0700 directory"
        );
    }
    Some(Layout { prefix })
}

/// Resolve the layout the user configured: `[packages].prefix` when set, the default otherwise.
#[must_use]
pub fn resolve_configured() -> Option<Layout> {
    resolve_from(&crate::config::load())
}

/// [`resolve_configured`] over a `[packages]` table already read.
#[must_use]
pub fn resolve_from(cfg: &crate::config::PackagesConfig) -> Option<Layout> {
    resolve(
        cfg.prefix_path(aterm_types::dirs::home_dir().as_deref())
            .as_deref(),
    )
}

/// Validate a configured prefix against `home`, returning it if safe or the trusted [`default_prefix`] otherwise.
#[must_use]
pub fn vet_prefix(configured: Option<&Path>, home: &Path) -> PathBuf {
    let default = default_prefix(home);
    let Some(p) = configured else {
        return default;
    };
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return default;
    }
    if !under_home(p, home) {
        if !system_chain_trusted(p) {
            return default;
        }
        if !caller_can_use_prefix(p) {
            crate::notice::say_once(&format!(
                "configured prefix {} is not writable by this user — using the default {} \
                 instead (set [packages].prefix only for a shared multi-user store this user \
                 can write)",
                p.display(),
                default.display()
            ));
            return default;
        }
        return p.to_path_buf();
    }
    if p == home {
        return default;
    }
    for anc in p.ancestors().filter(|a| under_home(a, home)) {
        // A missing ancestor is the tail atpkg makes: the prefix `0700`, the directories on
        // the way `0755` whatever the umask (`create_dir_all_unshared`), so the next resolve
        // admits what this one created. `is_reparse` also catches a junction, which needs no
        // admin and reports `is_symlink() == false`.
        if let Ok(m) = std::fs::symlink_metadata(anc)
            && (crate::platform::is_reparse(&m) || !crate::platform::dir_meta_is_private(&m))
        {
            return default;
        }
    }
    p.to_path_buf()
}

#[must_use]
fn caller_can_use_prefix(p: &Path) -> bool {
    // Writable: we can install here.
    if p.ancestors()
        .find(|a| a.exists())
        .is_some_and(crate::platform::dir_writable_by_caller)
    {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(p.join("bin")) else {
        return false;
    };
    entries.flatten().any(|e| {
        crate::platform::resolve_shim(&e.path()).is_some_and(|t| !presence(&t).is_absent())
    })
}

/// A test's own system-shaped prefix.
#[cfg(test)]
pub(crate) mod system_shape {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    thread_local! {
        static FORCED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    /// Answer "system-shaped" for `prefix` on this thread from now on (the mode-bit tests that ask are unix).
    #[cfg(unix)]
    pub(crate) fn force(prefix: &Path) {
        FORCED.with(|f| f.borrow_mut().push(prefix.to_path_buf()));
    }

    pub(super) fn forced(prefix: &Path) -> bool {
        FORCED.with(|f| f.borrow().iter().any(|p| p == prefix))
    }
}

fn system_chain_trusted(p: &Path) -> bool {
    p.ancestors().all(|anc| {
        std::fs::symlink_metadata(anc).is_ok_and(|m| {
            !crate::platform::is_reparse(&m) && crate::platform::dir_meta_is_system(&m)
        }) || std::fs::symlink_metadata(anc).is_err()
    })
}

#[must_use]
fn under_home(p: &Path, home: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut hc = home.components();
        let mut pc = p.components();
        loop {
            match hc.next() {
                None => return true,
                Some(h) => match pc.next() {
                    Some(q) if h.as_os_str().eq_ignore_ascii_case(q.as_os_str()) => continue,
                    _ => return false,
                },
            }
        }
    }
    #[cfg(not(windows))]
    {
        p.starts_with(home)
    }
}

const SENSITIVE_SHIMS: &[&str] = &[
    "sudo",
    "ssh",
    "scp",
    "sshd",
    "git",
    "env",
    "sudo_askpass",
    "doas",
    "su",
    "login",
    "passwd",
    "gpg",
    "gpg2",
    "curl",
    "wget",
    "rm",
    "mv",
    "cp",
    "ln",
    "chmod",
    "chown",
    "kill",
    "launchctl",
    "osascript",
    "security",
    "codesign",
    "spctl",
    "cargo",
    "rustc",
    "rustup",
    "python",
    "python3",
    "node",
    "ls",
    "cat",
];

const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "rbash",
    "zsh",
    "fish",
    "pwsh",
    "powershell",
    "nu",
    "dash",
    "ash",
    "ksh",
    "mksh",
    "oksh",
    "pdksh",
    "loksh",
    "yash",
    "tcsh",
    "csh",
    "elvish",
    "xonsh",
    "osh",
    "ysh",
    "ion",
    "rc",
    "es",
    "busybox",
];

fn is_shell(lower: &str) -> bool {
    SHELLS.iter().any(|shell| {
        lower.strip_prefix(shell).is_some_and(|rest| {
            let version = rest.strip_prefix('-').unwrap_or(rest);
            rest.is_empty()
                || rest == "-preview"
                || (version.starts_with(|c: char| c.is_ascii_digit())
                    && version.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
    })
}

/// The prefix of an alias shim.
pub const ALIAS_PREFIX: &str = "alab-";

/// Whether `name` may be installed as a `bin/` shim.
#[must_use]
pub fn shim_allowed(name: &str) -> bool {
    if name.is_empty()
        || !name.is_ascii()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    if SENSITIVE_SHIMS.contains(&lower.as_str()) || is_shell(&lower) {
        return false;
    }
    match lower.strip_prefix(ALIAS_PREFIX) {
        Some(base) => !base.is_empty() && !base.starts_with(ALIAS_PREFIX) && shim_allowed(base),
        None => true,
    }
}

/// A logical tool name: one entry of a manifest's `exposes` list, never a file name.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ToolName(String);

fn with_suffix(name: &str, suffix: &str) -> String {
    format!("{name}{suffix}")
}

#[inline]
fn without_suffix<'a>(name: &'a str, suffix: &str) -> &'a str {
    name.strip_suffix(suffix).unwrap_or(name)
}

impl ToolName {
    /// Admit `raw` as a tool name, or `None` when it fails [`shim_allowed`] (a sensitive command or a malformed name).
    #[inline]
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        shim_allowed(raw).then(|| Self(raw.to_string()))
    }

    /// The `bin/` **shim** file name: `<tool><SHIM_SUFFIX>` — `ay` on Unix (a bare symlink), `ay.cmd` on Windows (a batch wrapper).
    #[inline]
    #[must_use]
    pub fn shim_file(&self) -> String {
        with_suffix(&self.0, crate::platform::SHIM_SUFFIX)
    }

    /// The **executable** file name inside a build's `bin/`: `<tool><EXE_SUFFIX>` — `ay` on Unix, `ay.exe` on Windows.
    #[inline]
    #[must_use]
    pub fn exe_file(&self) -> String {
        with_suffix(&self.0, crate::platform::EXE_SUFFIX)
    }

    /// Recover the logical name from a `bin/` entry, stripping the platform [`crate::platform::SHIM_SUFFIX`] (`ay.cmd` reads back as `ay` on Windows).
    #[inline]
    #[must_use]
    pub fn from_shim_file(name: &str) -> Option<Self> {
        Self::new(without_suffix(name, crate::platform::SHIM_SUFFIX))
    }

    /// The logical name, for reporting (`status.toml`, refusal lists, log lines), not for building a path.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is an [`ALIAS_PREFIX`]ed name (`alab-trust`), i.e. a shim that forwards to another tool's executable rather than to one of its own name.
    #[inline]
    #[must_use]
    pub fn is_alias(&self) -> bool {
        self.0.starts_with(ALIAS_PREFIX)
    }

    /// The `alab-<tool>` alias of this tool.
    #[inline]
    #[must_use]
    pub fn alias(&self) -> Option<Self> {
        if self.is_alias() {
            return None;
        }
        let mut s = String::from(ALIAS_PREFIX);
        s.push_str(&self.0);
        Self::new(&s)
    }

    /// The tool an alias names: `alab-trust` → `trust`; `None` for a plain name.
    #[inline]
    #[must_use]
    pub fn alias_base(&self) -> Option<Self> {
        self.0.strip_prefix(ALIAS_PREFIX).and_then(Self::new)
    }
}

/// Split a manifest's raw `exposes` list into the names that may be shimmed and the raw names refused.
#[must_use]
pub fn split_exposed(exposes: &[String]) -> (Vec<ToolName>, Vec<String>) {
    let mut tools = Vec::new();
    let mut refused = Vec::new();
    for raw in exposes {
        match ToolName::new(raw) {
            Some(t) => tools.push(t),
            None => refused.push(raw.clone()),
        }
    }
    (tools, refused)
}

/// Compose the child `PATH` for running a managed tool.
#[must_use]
pub fn append_bin_to_path(inherited: Option<&OsStr>, bin_dir: &Path) -> OsString {
    let mut dirs: Vec<PathBuf> = match inherited {
        Some(p) if !p.is_empty() => std::env::split_paths(p).collect(),
        _ => Vec::new(),
    };
    if !dirs.iter().any(|d| d == bin_dir) {
        dirs.push(bin_dir.to_path_buf());
    }
    std::env::join_paths(&dirs)
        .unwrap_or_else(|_| inherited.map(OsStr::to_os_string).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_home(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-store-{label}-"))
    }

    #[test]
    fn ensure_agents_dir_creates_a_real_directory_once_and_refuses_a_link_or_a_file() {
        let h = temp_home("ensure-agents");
        let l = Layout {
            prefix: h.join("pkg"),
        };
        assert!(!l.agents_dir().exists(), "a fresh prefix has no agents/");
        let dir = l.ensure_agents_dir().expect("created");
        assert_eq!(dir, l.agents_dir());
        assert!(dir.is_absolute());
        assert!(
            std::fs::symlink_metadata(&dir).unwrap().is_dir(),
            "a real directory"
        );
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "a $HOME prefix's directory is private"
        );
        assert_eq!(l.ensure_agents_dir().as_deref(), Ok(dir.as_path()));
        let filed = Layout {
            prefix: h.join("filed"),
        };
        std::fs::create_dir_all(&filed.prefix).unwrap();
        std::fs::write(filed.agents_dir(), b"not a dir").unwrap();
        filed.ensure_agents_dir().expect_err("a file is refused");
        assert!(filed.agents_dir().is_file(), "left alone");
        #[cfg(unix)]
        {
            let linked = Layout {
                prefix: h.join("linked"),
            };
            std::fs::create_dir_all(&linked.prefix).unwrap();
            std::os::unix::fs::symlink(&dir, linked.agents_dir()).unwrap();
            assert!(linked.agents_dir().is_dir(), "the link resolves");
            linked.ensure_agents_dir().expect_err("a link is refused");
            assert!(
                std::fs::symlink_metadata(linked.agents_dir())
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "left alone"
            );
        }
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn the_declared_mode_record_round_trips_and_goes_with_the_build() {
        let h = temp_home("declared-modes");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("claude", 2026092201);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        let mut modes = crate::tree::DeclaredModes::new();
        modes.insert(b"bin/claude.exe".to_vec(), 0o755);
        modes.insert(b"lib/data".to_vec(), 0o644);
        write_declared_modes(&build, &modes).unwrap();
        let sidecar = build.with_file_name("2026092201.modes");
        if crate::platform::HAS_POSIX_MODES {
            assert!(
                !sidecar.exists(),
                "Unix reads the inode: no record is written"
            );
            assert!(declared_modes(&build).unwrap().is_empty());
        } else {
            assert!(sidecar.is_file(), "a sibling, outside the tree");
            assert_eq!(
                std::fs::read(&sidecar).unwrap(),
                b"declared-modes v1\nbin/claude.exe\x00755\nlib/data\x00644\n".to_vec(),
                "the line framing is the tree line's, minus its digest"
            );
            assert_eq!(declared_modes(&build).unwrap(), modes);
            clear_declared_modes(&build);
            let err = declared_modes(&build).unwrap_err().to_string();
            assert!(
                err.contains("no declared-mode record") && err.contains("re-stages"),
                "{err}"
            );
            write_declared_modes(&build, &modes).unwrap();
        }
        discard_build(&build);
        assert!(
            !build.exists() && !sidecar.exists(),
            "the discard takes the record"
        );
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn the_declared_mode_record_parser_fails_closed() {
        let ok =
            parse_declared_modes(b"declared-modes v1\nbin/a\x00755\nb\x000\nc\x00644\n").unwrap();
        assert_eq!(ok.get(&b"bin/a"[..]), Some(&0o755));
        assert_eq!(ok.get(&b"b"[..]), Some(&0));
        assert_eq!(ok.get(&b"c"[..]), Some(&0o644));
        assert!(
            parse_declared_modes(b"declared-modes v1\n")
                .unwrap()
                .is_empty()
        );
        for bad in [
            &b"declared-modes v2\nbin/a\x00755\n"[..],
            b"bin/a\x00755\n",
            b"declared-modes v1\nbin/a 755\n",
            b"declared-modes v1\n\x00755\n",
            b"declared-modes v1\nbin/a\0\n",
            b"declared-modes v1\nbin/a\x00758\n",
            b"declared-modes v1\nbin/a\x0007555\n",
        ] {
            assert!(parse_declared_modes(bad).is_none(), "{bad:?}");
        }
        assert_eq!(crate::tree::oct_mode(0o755), "755");
    }

    fn sha256(hex: &str) -> (DigestAlg, &str) {
        (DigestAlg::Sha256, hex)
    }

    #[test]
    fn a_refusal_memo_names_its_digest_by_algorithm() {
        let h = temp_home("refusal-alg");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("agy", 1_000_001_000_003_000_006);
        let marker = build.with_file_name("1000001000003000006.refused");
        let client = aterm_types::version::app_version();
        let earlier = format!(
            "stage-refusal v1\nsha256=aa\ntree_root=bb\nattempts=2\nat=1000\nclient={client}\n\
             why=asset sha256 mismatch: expected aa, got cc\n"
        );
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, &earlier).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert_eq!(
            (m.alg, m.digest.as_str(), m.attempts),
            (DigestAlg::Sha256, "aa", 2)
        );
        assert!(m.binds(sha256("aa"), "bb", 1_001));
        record_stage_refusal(
            &build,
            sha256("aa"),
            "bb",
            "asset sha256 mismatch: expected aa, got cc",
            1_000,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            earlier.replace("attempts=2", "attempts=3"),
            "a sha256 memo is written as it always was"
        );

        let sha512 = "5".repeat(128);
        record_signer_refusal(
            &build,
            (DigestAlg::Sha512, &sha512),
            "bb",
            "signer refused: x",
            2_000,
        )
        .unwrap();
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(text.contains(&format!("\nsha512={sha512}\n")), "{text}");
        assert!(!text.contains("sha256="), "{text}");
        let m = stage_refusal(&build).unwrap();
        assert_eq!(
            (m.alg, m.digest.as_str(), m.attempts),
            (DigestAlg::Sha512, sha512.as_str(), 1)
        );
        assert!(m.binds((DigestAlg::Sha512, &sha512), "bb", i64::MAX));
        assert!(
            !m.binds(sha256(&sha512), "bb", i64::MAX),
            "the same hex under another algorithm is another question"
        );
        for bad in [
            text.replace("sha512=", "sha256=x\nsha512="),
            text.replace(&format!("sha512={sha512}\n"), ""),
        ] {
            std::fs::write(&marker, &bad).unwrap();
            assert_eq!(stage_refusal(&build), None, "{bad}");
        }
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_stage_refusal_binds_only_while_its_digests_and_cooldown_hold() {
        let memo = |attempts| StageRefusal {
            alg: DigestAlg::Sha256,
            digest: "AA".into(),
            tree_root: "bb".into(),
            why: "asset sha256 mismatch: expected aa, got cc".into(),
            at: 1_000,
            attempts,
            signer: false,
            held: false,
            client: aterm_types::version::app_version().into(),
        };
        assert_eq!(memo(1).cooldown_secs(), 0, "one failure holds nothing");
        assert_eq!(memo(2).cooldown_secs(), 12 * 3600);
        assert_eq!(memo(3).cooldown_secs(), 24 * 3600);
        assert_eq!(memo(4).cooldown_secs(), 48 * 3600);
        assert_eq!(
            memo(10).cooldown_secs(),
            7 * 24 * 3600,
            "capped at one week"
        );
        assert_eq!(memo(1_000).cooldown_secs(), 7 * 24 * 3600, "no overflow");
        assert!(
            !memo(1).binds(sha256("aa"), "bb", 1_000),
            "a single failure never holds the next attempt off the wire"
        );

        let m = memo(2);
        assert_eq!(m.retry_after(), 1_000 + 12 * 3600);
        assert!(
            m.binds(sha256("aa"), "BB", 1_000),
            "case-insensitive on both digests"
        );
        assert!(
            !m.binds(sha256("aa"), "BB", m.retry_after()),
            "the cooldown lapses — the next attempt is due"
        );
        assert!(
            !m.binds(sha256("dd"), "bb", 1_000),
            "a re-cut pin's new sha256 binds nothing"
        );
        assert!(
            !m.binds(sha256("aa"), "dd", 1_000),
            "a re-cut pin's new tree_root binds nothing"
        );
        assert!(
            !m.binds(sha256("aa"), "bb", i64::MAX),
            "an unreadable clock (now_unix's fail-closed i64::MAX) reads as lapsed, \
             so it can only cost a download — never block an install"
        );
        assert!(
            !StageRefusal {
                digest: String::new(),
                ..memo(2)
            }
            .binds(sha256(""), "bb", 1_000),
            "a memo with no digest binds nothing"
        );
        let another = format!("{}+another", aterm_types::version::app_version());
        assert!(
            !StageRefusal {
                client: another.clone(),
                ..memo(2)
            }
            .binds(sha256("aa"), "bb", 1_000),
            "a memo another atpkg wrote binds nothing"
        );
        assert!(
            !StageRefusal {
                client: String::new(),
                ..memo(2)
            }
            .binds(sha256("aa"), "bb", 1_000),
            "a pre-field memo binds nothing"
        );
        let held = StageRefusal {
            held: true,
            ..memo(1)
        };
        assert!(
            held.binds(sha256("aa"), "bb", 1_000),
            "held from the first verdict"
        );
        assert!(
            held.binds(sha256("aa"), "bb", i64::MAX),
            "no cooldown to lapse"
        );
        assert!(
            !held.binds(sha256("dd"), "bb", 1_000),
            "a re-cut pin releases it"
        );
        assert!(
            !StageRefusal {
                client: another,
                ..held.clone()
            }
            .binds(sha256("aa"), "bb", 1_000),
            "a new client gets its attempt"
        );
    }

    #[test]
    fn a_signer_refusal_binds_until_the_digests_move() {
        let m = StageRefusal {
            alg: DigestAlg::Sha256,
            digest: "AA".into(),
            tree_root: "bb".into(),
            why: "signer refused: bin/claude is not signed by Developer ID team X".into(),
            at: 1_000,
            attempts: 1,
            signer: true,
            held: false,
            client: aterm_types::version::app_version().into(),
        };
        assert!(
            m.binds(sha256("aa"), "BB", 1_000),
            "the first refusal already binds"
        );
        assert!(
            m.binds(sha256("aa"), "bb", i64::MAX),
            "no cooldown to lapse"
        );
        assert!(
            !m.binds(sha256("dd"), "bb", 1_000),
            "a new sha256 is a new question"
        );
        assert!(
            !m.binds(sha256("aa"), "dd", 1_000),
            "a new tree_root is a new question"
        );
        assert!(
            !StageRefusal {
                digest: String::new(),
                ..m
            }
            .binds(sha256(""), "bb", 1_000)
        );
    }

    #[test]
    fn the_signer_refusal_memo_round_trips() {
        let h = temp_home("signer-refusal");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("claude", 1_000_002_000_001_000_280);
        record_signer_refusal(&build, sha256("aa"), "bb", "signer refused: x", 1_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert!(m.signer);
        assert!(!m.held, "a signer memo is its own kind, not a hold");
        assert_eq!(m.client, aterm_types::version::app_version());
        assert_eq!((m.attempts, m.why.as_str()), (1, "signer refused: x"));
        assert!(m.binds(sha256("aa"), "bb", i64::MAX));
        record_stage_refusal(&build, sha256("aa"), "bb", "asset sha256 mismatch", 2_000).unwrap();
        assert!(!stage_refusal(&build).unwrap().signer);
        record_signer_refusal(&build, sha256("aa"), "bb", "signer refused: x", 3_000).unwrap();
        clear_stage_refusals(&l, "claude");
        assert_eq!(stage_refusal(&build), None);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn the_refusal_memo_round_trips_advances_and_is_one_per_program() {
        let h = temp_home("stage-refusal");
        let l = Layout { prefix: h.clone() };
        let build = l.build_dir("trust", 4900);
        assert_eq!(stage_refusal(&build), None, "nothing recorded yet");

        record_stage_refusal(
            &build,
            sha256("AA"),
            "bb",
            "asset sha256 mismatch:\nexpected aa",
            1_000,
        )
        .unwrap();
        let marker = build.with_file_name("4900.refused");
        assert!(marker.is_file(), "a sibling, outside the tree");
        assert!(!build.exists(), "the build it refused never came to exist");
        let m = stage_refusal(&build).unwrap();
        assert_eq!((m.digest.as_str(), m.tree_root.as_str()), ("AA", "bb"));
        assert_eq!(m.at, 1_000);
        assert_eq!(m.attempts, 1);
        assert!(!m.held, "a sha256 mismatch cools down; it is not held");
        assert_eq!(
            m.client,
            aterm_types::version::app_version(),
            "the memo names the client that wrote it"
        );
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(
            !text.contains("hold="),
            "a cooled-down memo carries no hold line: {text}"
        );
        assert_eq!(
            m.why, "asset sha256 mismatch: expected aa",
            "one line: the newline is folded"
        );

        record_stage_refusal(&build, sha256("aa"), "BB", "again", 50_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert_eq!((m.attempts, m.at), (2, 50_000));
        record_stage_refusal(&build, sha256("cc"), "bb", "new pin, new verdict", 60_000).unwrap();
        assert_eq!(stage_refusal(&build).unwrap().attempts, 1);
        record_held_refusal(&build, sha256("cc"), "bb", "tree_root mismatch", 70_000).unwrap();
        let m = stage_refusal(&build).unwrap();
        assert!(m.held, "the hold line reads back");
        assert_eq!(m.attempts, 2, "the same digests: the count still advances");
        assert!(
            m.binds(sha256("cc"), "bb", i64::MAX),
            "held: no cooldown to lapse"
        );
        assert!(
            std::fs::read_to_string(&marker)
                .unwrap()
                .contains("\nhold=1\n"),
            "the memo carries the hold"
        );

        let next = l.build_dir("trust", 4901);
        record_stage_refusal(&next, sha256("dd"), "ee", "and again", 70_000).unwrap();
        assert!(
            !marker.exists(),
            "the superseded build's memo is reclaimed, not left to accumulate"
        );
        assert!(stage_refusal(&next).is_some());

        std::fs::write(next.with_file_name("4901.refused"), b"nonsense\nat=1\n").unwrap();
        assert_eq!(
            stage_refusal(&next),
            None,
            "a file that does not open with the schema line is not read"
        );

        record_stage_refusal(&next, sha256("dd"), "ee", "once more", 80_000).unwrap();
        clear_stage_refusal(&next);
        assert_eq!(stage_refusal(&next), None);
        clear_stage_refusal(&next);
        record_stage_refusal(&next, sha256("dd"), "ee", "once more", 90_000).unwrap();
        clear_stage_refusals(&l, "trust");
        assert_eq!(stage_refusal(&next), None, "the explicit door forgets it");
        record_stage_refusal(&next, sha256("dd"), "ee", "once more", 95_000).unwrap();
        std::fs::create_dir_all(next.join("bin")).unwrap();
        discard_build(&next);
        assert_eq!(stage_refusal(&next), None, "the discard takes it too");
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn prefix_outside_home_or_with_traversal_falls_back() {
        let home = temp_home("outside");
        assert_eq!(
            vet_prefix(Some(Path::new("/tmp/evil")), &home),
            default_prefix(&home)
        );
        let sneaky = home.join("../somewhere/pkg");
        assert_eq!(vet_prefix(Some(&sneaky), &home), default_prefix(&home));
        assert_eq!(vet_prefix(Some(&home), &home), default_prefix(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn root_owned_prefix_outside_home_is_accepted() {
        use std::os::unix::fs::MetadataExt as _;
        let home = temp_home("sysprefix");
        let Ok(meta) = std::fs::symlink_metadata("/usr/lib") else {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run("no /usr/lib here to stand in as a system chain");
        };
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run(
                "/usr/lib here is not root-owned and closed to group and other writes",
            );
        }
        let prefix = Path::new("/usr/lib/aterm-pkg-system-prefix-test");
        assert!(
            system_chain_trusted(prefix),
            "a fully root-owned chain outside $HOME is a trusted system prefix"
        );
        let expected = if caller_can_use_prefix(prefix) {
            prefix.to_path_buf()
        } else {
            default_prefix(&home)
        };
        assert_eq!(vet_prefix(Some(prefix), &home), expected);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn world_writable_system_ancestor_is_refused() {
        use std::os::unix::fs::MetadataExt as _;
        let home = temp_home("wwancestor");
        let Some((root, meta)) = ["/private/tmp", "/tmp"].into_iter().find_map(|root| {
            std::fs::symlink_metadata(root)
                .ok()
                .filter(std::fs::Metadata::is_dir)
                .map(|meta| (root, meta))
        }) else {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run("no /private/tmp or /tmp directory here");
        };
        if meta.uid() != 0 || meta.mode() & 0o022 == 0 {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run(format!(
                "{root} here is not the root-owned world-writable fixture this test needs"
            ));
        }
        let prefix = Path::new(root).join("aterm-pkg-should-not-be-trusted");
        let prefix = prefix.as_path();
        assert_eq!(
            vet_prefix(Some(prefix), &home),
            default_prefix(&home),
            "a world-writable ancestor must fail closed even when root owns it"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn group_writable_intermediate_parent_is_rejected() {
        let home = temp_home("gwparent");
        let mid = home.join("Library");
        std::fs::create_dir_all(&mid).unwrap();
        std::fs::set_permissions(&mid, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bad = mid.join("shared");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o777)).unwrap();
        let prefix = bad.join("pkg");
        assert_eq!(
            vet_prefix(Some(&prefix), &home),
            default_prefix(&home),
            "a group/other-writable intermediate parent must fail closed to the default"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn safe_under_home_prefix_is_accepted() {
        let home = temp_home("safe");
        let a = home.join("a");
        std::fs::create_dir_all(&a).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o700)).unwrap();
        let prefix = a.join("b").join("pkg");
        assert_eq!(vet_prefix(Some(&prefix), &home), prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    const OPEN_UMASK_ENV: &str = "ATPKG_STORE_OPEN_UMASK_HOME";
    #[cfg(unix)]
    const OPEN_UMASK_PROBE: &str =
        "store::tests::probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor";

    #[cfg(unix)]
    #[test]
    fn probe_a_prefix_made_under_an_open_umask_has_no_shared_ancestor() {
        let Some(home) = std::env::var_os(OPEN_UMASK_ENV) else {
            return;
        };
        let home = PathBuf::from(home);
        // SAFETY: `umask(2)` swaps this process's file-creation mask and cannot fail.
        unsafe { libc::umask(0o002) };
        let mode = |rel: &str| {
            let meta = std::fs::symlink_metadata(home.join(rel)).unwrap();
            format!("{:o}", meta.permissions().mode() & 0o7777)
        };
        let default = Layout {
            prefix: home.join(".local/share/aterm/pkg"),
        };
        default.ensure_dir(&default.prefix).unwrap();
        for made in [".local", ".local/share", ".local/share/aterm"] {
            assert_eq!(mode(made), "755", "{made}: made under umask 002");
        }
        assert_eq!(mode(".local/share/aterm/pkg"), "700", "the $HOME prefix");
        let configured = Layout {
            prefix: home.join("work/aterm/pkg"),
        };
        configured.ensure_dir(&configured.prefix).unwrap();
        for made in ["work", "work/aterm"] {
            assert_eq!(mode(made), "755", "{made}: made under umask 002");
        }
        assert_eq!(mode("work/aterm/pkg"), "700", "the configured prefix");
        assert_eq!(
            vet_prefix(Some(&configured.prefix), &home),
            configured.prefix,
            "the prefix this pass made is refused by the next one"
        );
        let shared = Layout {
            prefix: home.join("opt/aterm/pkg"),
        };
        system_shape::force(&shared.prefix);
        shared.ensure_dir(&shared.prefix).unwrap();
        for made in ["opt", "opt/aterm"] {
            assert_eq!(mode(made), "755", "{made}: made under umask 002");
        }
        assert_eq!(mode("opt/aterm/pkg"), "755", "the system prefix");
    }

    #[cfg(unix)]
    #[test]
    fn a_prefix_made_under_an_open_umask_has_no_shared_ancestor() {
        let home = temp_home("open-umask");
        let out = aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", OPEN_UMASK_PROBE])
            .env(OPEN_UMASK_ENV, home.as_os_str())
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "the prefix under umask 002: {}\n{stdout}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn trusted_system_prefix_that_we_cannot_write_falls_back_to_default() {
        aterm_uds::host::could_not_run_as_root("a prefix this user cannot write");
        let home = temp_home("unwritable-system");
        let prefix = PathBuf::from("/usr/lib/aterm-nux-probe/pkg");
        if !system_chain_trusted(&prefix) {
            let _ = std::fs::remove_dir_all(&home);
            aterm_uds::host::could_not_run(
                "/usr/lib here is not a trusted system chain (root-owned, closed to writes)",
            );
        }
        assert!(
            !caller_can_use_prefix(&prefix),
            "precondition: a non-root user must not be able to write under /usr"
        );
        assert_eq!(
            vet_prefix(Some(&prefix), &home),
            default_prefix(&home),
            "a trusted-but-unwritable system prefix must degrade to the default"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn an_unwritable_prefix_whose_shims_cannot_be_stat_ed_is_still_usable() {
        use std::os::unix::fs::PermissionsExt as _;
        aterm_uds::host::could_not_run_as_root("a mode-0 prefix");
        let prefix = crate::testdir::dir("atpkg-shared-");
        let holder = prefix.join("store/ay/17/bin");
        std::fs::create_dir_all(&holder).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let ay = ToolName::new("ay").unwrap();
        std::fs::write(holder.join(ay.exe_file()), b"#!/bin/true\n").unwrap();
        crate::platform::install_shim(&holder, &ay, &prefix.join("bin").join(ay.shim_file()))
            .unwrap();
        assert!(
            caller_can_use_prefix(&prefix),
            "precondition: a writable prefix serving a real shim is usable"
        );
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o555)).unwrap();
        let target = prefix.join("store/ay/17/bin").join(ay.exe_file());
        let armed = std::fs::metadata(&target).is_err();
        let usable = caller_can_use_prefix(&prefix);
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&prefix);
        assert!(
            armed,
            "the fixture must make the stat fail, or it proves nothing"
        );
        assert!(
            usable,
            "a shared prefix whose tools we may run but not stat must not be abandoned"
        );
    }

    #[test]
    fn shim_names_collision_and_shape_policy() {
        for bad in ["sudo", "SSH", "git", "sh", "env", "rustc", "Codesign"] {
            assert!(!shim_allowed(bad), "{bad} must be refused a shim");
        }
        for shell in SHELLS {
            for name in [
                (*shell).to_string(),
                shell.to_ascii_uppercase(),
                format!("{ALIAS_PREFIX}{shell}"),
                format!("{shell}-preview"),
                format!("{shell}5"),
                format!("{shell}-7.4"),
            ] {
                assert!(
                    !shim_allowed(&name),
                    "the shell {name} must be refused a shim"
                );
            }
        }
        for named in ["pwsh", "elvish", "xonsh", "mksh", "powershell", "busybox"] {
            assert!(SHELLS.contains(&named), "{named} is on the list");
        }
        for lookalike in [
            "pw\u{17F}h",
            "fi\u{17F}h",
            "\u{FB01}sh",
            "xon\u{17F}h",
            "elvi\u{17F}h",
            "\u{212A}sh",
            "\u{17F}h",
            "ba\u{17F}h",
            "alab-pw\u{17F}h",
            "caf\u{E9}",
        ] {
            assert!(
                !shim_allowed(lookalike),
                "{lookalike:?} is not plain ASCII and must be refused a shim"
            );
        }
        for ok in [
            "nushell-lsp",
            "dashboard",
            "pwsh-fmt",
            "fisher",
            "kshrc-lint",
            "nuget",
            "ashes",
            "rcfile",
            "bash-lsp",
            "zsh-5x",
            "ion-lint",
            "trust2",
        ] {
            assert!(shim_allowed(ok), "{ok} is no shell and should be allowed");
        }
        for bad in ["", ".", "..", "a/b", "x\0y"] {
            assert!(!shim_allowed(bad), "{bad:?} is not a valid shim name");
        }
        for ok in ["ay", "ny", "trust-mc", "clean-certify"] {
            assert!(shim_allowed(ok), "{ok} should be allowed");
        }
    }

    #[test]
    fn alias_names_admit_on_their_base_and_refuse_sensitive_bases() {
        for ok in ["alab-ay", "alab-trust", "alab-trust-cg"] {
            assert!(shim_allowed(ok), "{ok} should be allowed");
        }
        for bad in [
            "alab-sudo",
            "alab-SSH",
            "alab-git",
            "alab-cargo",
            "alab-",
            "alab-alab-ay",
            "alab-a/b",
            "alab-..",
        ] {
            assert!(!shim_allowed(bad), "{bad:?} must be refused a shim");
        }
        let trust = ToolName::new("trust").unwrap();
        assert!(!trust.is_alias());
        let alias = trust.alias().expect("a plain admissible name has an alias");
        assert_eq!(alias.as_str(), "alab-trust");
        assert!(alias.is_alias());
        assert_eq!(alias.alias_base(), Some(trust.clone()));
        assert_eq!(trust.alias_base(), None, "a plain name has no base");
        assert_eq!(alias.alias(), None, "an alias never gets an alias");
        let cg = ToolName::new("trust-cg").unwrap();
        let cg_alias = cg.alias().unwrap();
        assert_eq!(cg_alias.as_str(), "alab-trust-cg");
        assert_eq!(cg_alias.alias_base(), Some(cg));
        assert_eq!(
            cg_alias.shim_file(),
            format!("alab-trust-cg{}", crate::platform::SHIM_SUFFIX)
        );
        assert_eq!(ToolName::new("alab-sudo"), None);
        assert_eq!(ToolName::from_shim_file("alab-git"), None);
    }

    #[test]
    fn the_suffix_rule_round_trips_and_never_doubles_on_a_suffixed_platform() {
        assert_eq!(with_suffix("ay", ".cmd"), "ay.cmd");
        assert_eq!(with_suffix("ay", ".exe"), "ay.exe");
        assert_ne!(with_suffix("ay", ".cmd"), with_suffix("ay", ".exe"));

        assert_eq!(without_suffix(&with_suffix("ay", ".cmd"), ".cmd"), "ay");
        assert_eq!(
            with_suffix(&with_suffix("ay", ".cmd"), ".cmd"),
            "ay.cmd.cmd"
        );

        assert_eq!(without_suffix("ay", ".cmd"), "ay");
        assert_eq!(without_suffix("ay.exe", ".cmd"), "ay.exe");

        assert_eq!(with_suffix("ay", ""), "ay");
        assert_eq!(without_suffix("ay", ""), "ay");
    }

    #[test]
    fn a_marker_from_the_other_slice_reads_as_not_installed() {
        let home = temp_home("readyslice");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build));

        let marker = ready_marker_path(&build).unwrap();
        let text = std::fs::read_to_string(&marker).unwrap();
        assert!(
            text.starts_with("ok\n"),
            "line 1 stays what every earlier version wrote: {text:?}"
        );
        assert_eq!(recorded_platform(&text), Some(running_platform().as_str()));

        let mut foreign = String::from("ok\n");
        foreign.push_str(READY_PLATFORM_KEY);
        foreign.push_str("some-other-arch-macos\n");
        std::fs::write(&marker, foreign).unwrap();
        assert!(
            !build_is_complete(&build),
            "a build installed by the other architecture is not installed for us"
        );

        mark_build_ready(&build).unwrap();
        assert!(
            build_is_complete(&build),
            "re-staging natively must clear the mismatch, not leave the build unusable"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_marker_is_refuted_when_the_bin_it_vouched_for_is_gone() {
        let home = temp_home("readygutted");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::create_dir_all(build.join("lib")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"#!/bin/true\n").unwrap();
        std::fs::write(build.join("bin").join("targo"), b"#!/bin/true\n").unwrap();
        std::fs::write(build.join("lib").join("libtrust.dylib"), b"payload").unwrap();
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build), "precondition: a whole build");

        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(
            recorded_contents(&text),
            Some("bin:targo,trustc"),
            "sorted, comma-joined, so the record is a function of the tree: {text:?}"
        );

        std::fs::remove_dir_all(build.join("bin")).unwrap();
        assert!(
            ready_marker_path(&build).unwrap().is_file(),
            "the fixture must leave the marker in place, or it proves nothing"
        );
        assert!(
            build.join("lib").join("libtrust.dylib").is_file(),
            "and the orphaned payload, exactly as on m3"
        );
        assert!(
            !build_is_complete(&build),
            "a marker whose `bin/` is gone vouches for nothing"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn a_bin_symlink_is_recorded_and_checked_by_its_own_name() {
        let home = temp_home("readylink");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
        std::os::unix::fs::symlink("tippy", build.join("bin").join("clippy")).unwrap();
        mark_build_ready(&build).unwrap();
        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(recorded_contents(&text), Some("bin:clippy,tippy"));
        std::fs::remove_file(build.join("bin").join("tippy")).unwrap();
        assert!(
            !build_is_complete(&build),
            "`tippy` was recorded in its own right and is gone"
        );
        std::fs::write(build.join("bin").join("tippy"), b"x").unwrap();
        assert!(build_is_complete(&build));
        std::fs::remove_file(build.join("bin").join("clippy")).unwrap();
        assert!(!build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_build_with_no_bin_records_none_and_stays_complete() {
        let home = temp_home("readynone");
        let build = home.join("store").join("clt").join("3");
        std::fs::create_dir_all(&build).unwrap();
        mark_build_ready(&build).unwrap();
        let text = std::fs::read_to_string(ready_marker_path(&build).unwrap()).unwrap();
        assert_eq!(recorded_contents(&text), Some(READY_CONTENTS_NONE));
        assert!(build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_marker_over_an_absent_build_dir_is_not_complete() {
        let home = temp_home("readyghost");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        mark_build_ready(&build).unwrap();
        std::fs::remove_dir_all(&build).unwrap();
        assert!(
            ready_marker_path(&build).unwrap().is_file(),
            "the fixture keeps the sibling marker, which is the whole point"
        );
        assert!(!build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn contents_still_stand_refutes_only_a_proven_absence() {
        let home = temp_home("readycontents");
        let build = home.join("b");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("ay"), b"x").unwrap();
        assert!(contents_still_stand(&build, READY_CONTENTS_NONE));
        assert!(contents_still_stand(&build, "bin:ay"));
        assert!(!contents_still_stand(&build, "bin:ay,ny"));
        assert!(contents_still_stand(&build, "tree-root:deadbeef"));
        assert!(contents_still_stand(&build, "bin"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn discard_build_unmarks_before_it_removes_so_an_interrupted_discard_leaves_no_lie() {
        let home = temp_home("discardorder");
        let build = home.join("store").join("trust").join("8595");
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin").join("trustc"), b"x").unwrap();
        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build), "precondition");
        std::fs::set_permissions(build.join("bin"), std::fs::Permissions::from_mode(0o500))
            .unwrap();
        discard_build(&build);
        let survived = build.join("bin").join("trustc").is_file();
        std::fs::set_permissions(build.join("bin"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        assert!(
            survived,
            "the fixture must make the removal fail, or it proves nothing"
        );
        assert!(
            !ready_marker_path(&build).unwrap().exists(),
            "the marker comes down FIRST, so a failed removal cannot leave one behind"
        );
        assert!(
            !build_is_complete(&build),
            "and the surviving tree reads as incomplete — reclaimable debris, not an install"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unmark_program_builds_unmarks_every_build_and_only_builds() {
        let home = temp_home("unmarkall");
        let prog = home.join("store").join("trust");
        for n in [8590u64, 8595] {
            let b = prog.join(n.to_string());
            std::fs::create_dir_all(b.join("bin")).unwrap();
            std::fs::write(b.join("bin").join("trustc"), b"x").unwrap();
            mark_build_ready(&b).unwrap();
        }
        std::fs::create_dir_all(prog.join("8595.incoming-7")).unwrap();
        std::fs::write(prog.join("8595.provenance"), b"src\n").unwrap();
        let recording = aterm_durable::seam::Recording::start();
        unmark_program_builds(&prog);
        let flushed: &[aterm_durable::Flush] = if cfg!(windows) {
            &[]
        } else {
            &[aterm_durable::Flush::Ordered]
        };
        assert_eq!(
            recording.take(),
            flushed,
            "every marker down before any tree goes: one ordered directory flush \
             (aterm-durable's counting seam)"
        );
        drop(recording);
        for n in [8590u64, 8595] {
            let b = prog.join(n.to_string());
            assert!(!build_is_complete(&b), "build {n} must read as incomplete");
            assert!(
                b.join("bin").join("trustc").is_file(),
                "and nothing but the marker is touched"
            );
        }
        assert!(
            prog.join("8595.provenance").is_file(),
            "other sidecars stay"
        );
        assert!(prog.join("8595.incoming-7").is_dir());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn ready_text_accepts_only_an_absent_or_matching_record() {
        assert!(ready_text_accepts("ok\n", "aarch64-macos"));
        assert!(!ready_text_accepts("", "aarch64-macos"));
        assert!(!ready_text_accepts("\0\0\0\0", "aarch64-macos"));
        assert!(!ready_text_accepts("o", "aarch64-macos"));
        assert!(!ready_text_accepts(
            "platform=aarch64-macos\n",
            "aarch64-macos"
        ));
        assert!(ready_text_accepts("ok", "aarch64-macos"));
        assert!(ready_text_accepts(
            "ok\nplatform=aarch64-macos\n",
            "aarch64-macos"
        ));
        assert!(!ready_text_accepts(
            "ok\nplatform=x86_64-macos\n",
            "aarch64-macos"
        ));
        assert!(!ready_text_accepts(
            "ok\nplatform=aarch64-macos\n",
            "x86_64-macos"
        ));
        assert!(ready_text_accepts(
            "ok\r\nplatform=aarch64-macos\r\n",
            "aarch64-macos"
        ));
        assert!(ready_text_accepts("ok\nplatform=\n", "aarch64-macos"));
        assert!(ready_text_accepts(
            "ok\narch=x86_64-macos\n",
            "aarch64-macos"
        ));
    }

    #[test]
    fn a_marker_left_empty_or_torn_by_a_crash_reads_as_incomplete() {
        let home = temp_home("readytorn");
        let build = home.join("store").join("ay").join("18");
        std::fs::create_dir_all(&build).unwrap();
        let marker = ready_marker_path(&build).unwrap();

        std::fs::write(&marker, b"").unwrap();
        assert!(
            !build_is_complete(&build),
            "a zero-length marker must never vouch for a build"
        );
        std::fs::write(&marker, vec![0u8; 4096]).unwrap();
        assert!(
            !build_is_complete(&build),
            "a NUL-filled marker is not `ok`"
        );
        std::fs::write(&marker, [0xff_u8, 0xfe, 0xff]).unwrap();
        assert!(!build_is_complete(&build), "a non-UTF-8 marker is not `ok`");
        std::fs::write(&marker, b"o").unwrap();
        assert!(!build_is_complete(&build), "a torn marker is not `ok`");

        mark_build_ready(&build).unwrap();
        assert!(build_is_complete(&build));
        std::fs::write(&marker, b"ok\n").unwrap();
        assert!(build_is_complete(&build));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn sync_tree_flushes_every_file_and_directory_it_walks() {
        let home = temp_home("synctree");
        let root = home.join("18.incoming-1");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("lib").join("nested")).unwrap();
        std::fs::write(root.join("bin").join("ay"), b"#!/bin/true\n").unwrap();
        std::fs::write(root.join("lib").join("a.rlib"), b"payload").unwrap();
        std::fs::write(root.join("lib").join("nested").join("b.rlib"), b"deep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../lib/a.rlib", root.join("bin").join("link")).unwrap();

        let synced = sync_tree(&root).unwrap();
        assert_eq!(
            synced.files, 3,
            "every regular file, and the symlink is not one"
        );
        let dirs_expected = if cfg!(windows) { 0 } else { 4 };
        assert_eq!(
            synced.dirs, dirs_expected,
            "the root, bin, lib and lib/nested — where a directory can be opened"
        );

        assert!(sync_tree(&home.join("absent")).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn append_bin_to_path_appends_never_prepends() {
        let bin = Path::new("/p/bin");
        let inherited = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let out = append_bin_to_path(Some(&inherited), bin);
        assert_eq!(
            out,
            std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin"), bin]).unwrap()
        );
        assert_eq!(append_bin_to_path(None, bin), OsString::from("/p/bin"));
        assert_eq!(
            append_bin_to_path(Some(OsStr::new("")), bin),
            OsString::from("/p/bin")
        );
    }

    #[test]
    fn append_bin_to_path_is_idempotent() {
        let bin = Path::new("/p/bin");
        let bin_first = std::env::join_paths([Path::new("/p/bin"), Path::new("/usr/bin")]).unwrap();
        assert_eq!(append_bin_to_path(Some(&bin_first), bin), bin_first);
        let bin_last = std::env::join_paths([Path::new("/usr/bin"), Path::new("/p/bin")]).unwrap();
        assert_eq!(append_bin_to_path(Some(&bin_last), bin), bin_last);
    }
}
