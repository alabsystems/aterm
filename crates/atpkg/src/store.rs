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
    /// Whether this is a system prefix ([`system_chain_trusted`]).
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

    /// Ensure `agents/` exists ([`Self::agents_dir`], [`crate::activate::reconcile_agents`]).
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

    /// `store/<program>/<build>/`.
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

    /// `reroute/`: session-scoped upstream-name stubs ([`crate::reroute`], [`crate::hooks`]).
    #[inline]
    #[must_use]
    pub fn reroute_dir(&self) -> PathBuf {
        crate::reroute::dir(self)
    }

    /// `agents/`: the one managed directory that goes first on `PATH` ([`crate::hooks`]).
    #[inline]
    #[must_use]
    pub fn agents_dir(&self) -> PathBuf {
        self.prefix.join("agents")
    }

    /// `agents/<tool>` front-of-`PATH` shim ([`Self::agents_dir`], [`ToolName`], [`Self::shim`]).
    #[inline]
    #[must_use]
    pub fn agent_shim(&self, tool: &ToolName) -> PathBuf {
        self.agents_dir().join(tool.shim_file())
    }

    /// `landing/`: where older passes wrote landing markers ([`crate::landing`]).
    #[inline]
    #[must_use]
    pub fn landing_dir(&self) -> PathBuf {
        self.prefix.join("landing")
    }

    /// `bin/<tool>`: a single shim ([`ToolName::shim_file`]).
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

    /// `staging/<program>/`.
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

    /// `roster.floor`: the `0600` durable high-water `roster_seq`.
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

    /// `progress.json` ([`crate::progress`], [`Self::status`]).
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

    /// `pass-owed.json` ([`crate::progress::pass_owed`]).
    #[inline]
    #[must_use]
    pub fn pass_owed_file(&self) -> PathBuf {
        self.prefix.join("pass-owed.json")
    }

    /// `adopted`: the durable marker that this machine runs the ALab toolset as a set.
    #[inline]
    #[must_use]
    pub fn adopted(&self) -> PathBuf {
        self.prefix.join("adopted")
    }

    /// `adopted-by-seed` ([`Self::adopted`]).
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

    /// The programs this machine removed on purpose. Missing is an empty set; refused evidence
    /// is an error, never permission to reinstall or overwrite it. IO is bounded per record
    /// and by the admitted file's initial length; the returned set grows with distinct names.
    /// # Errors
    /// The file is not an admitted regular UTF-8 file, changes during the read, or contains a
    /// record longer than the producer's 4096-byte line limit.
    pub fn removed_programs(&self) -> std::io::Result<std::collections::BTreeSet<String>> {
        let mut names = std::collections::BTreeSet::new();
        match crate::metadata_io::read_regular_lines(&self.removed(), REMOVAL_LINE_BYTES, |line| {
            let name = line.trim();
            if !name.is_empty() && !name.starts_with('#') {
                names.insert(name.to_string());
            }
            Ok(true)
        }) {
            Ok(()) => Ok(names),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(error) => Err(error),
        }
    }

    /// `declined`: the durable "this machine does not want the bundled toolset" marker.
    #[inline]
    #[must_use]
    pub fn declined(&self) -> PathBuf {
        self.prefix.join("declined")
    }

    /// `units-off`: the durable "this machine shares no toolset archive on the LAN" marker
    /// `aterm pkg serve-units --off` writes and `--on` removes ([`crate::peers`]).
    #[inline]
    #[must_use]
    pub fn units_off(&self) -> PathBuf {
        self.prefix.join("units-off")
    }

    /// `units-serving`: where a server of this store's units listens, and its pid, while it
    /// serves ([`crate::peers::serving`]).
    #[inline]
    #[must_use]
    pub fn units_serving(&self) -> PathBuf {
        self.prefix.join("units-serving")
    }

    /// `retired/`: per-program retirement markers.
    #[inline]
    #[must_use]
    pub fn retired_dir(&self) -> PathBuf {
        self.prefix.join("retired")
    }

    /// `retired/<program>`: one retirement marker ([`ToolName`]).
    #[inline]
    #[must_use]
    pub fn retired_marker(&self, program: &str) -> PathBuf {
        self.retired_dir().join(program)
    }

    /// The `YYYY-MM-DD` day a managed copy of `program` was retired for a system install.
    #[must_use]
    pub fn retired_date(&self, program: &str) -> Option<String> {
        ToolName::new(program)?;
        let path = self.retired_marker(program);
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

    /// `links/<program>`.
    #[inline]
    #[must_use]
    pub fn link_marker(&self, program: &str) -> PathBuf {
        self.links_dir().join(program)
    }
}

/// The one stderr line for a refused `agents/` ([`Layout::ensure_agents_dir`]).
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

/// The ready record beside this named build, shared by install and publication admission.
pub fn ready_marker_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    Some(build_dir.with_file_name(format!("{name}{}", aterm_toolchain::layout::READY_SUFFIX)))
}

const READY_PLATFORM_KEY: &str = "platform=";

const READY_CONTENTS_KEY: &str = "contents=";

const READY_CONTENTS_NONE: &str = "none";

const READY_CONTENTS_BIN: &str = "bin:";

const READY_OK_LINE: &str = "ok";

/// The removal writer and streaming reader share this record bound, including its newline.
pub(crate) const REMOVAL_LINE_BYTES: usize = 4096;

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

/// What this process could learn about whether `path` is there.
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
    let Ok(host) = aterm_toolchain::gnu_runtime::host() else {
        return false;
    };
    build_is_complete_on(build_dir, host)
}

pub(crate) fn build_is_complete_on(
    build_dir: &Path,
    host: Option<aterm_toolchain::gnu_runtime::Host>,
) -> bool {
    let Some(marker) = ready_marker_path(build_dir) else {
        return false;
    };
    if is_provably_absent(build_dir) {
        return false;
    }
    // A runtime this process cannot read, or one past the inspector's entry bound, is no
    // verdict: the marker, written only after a whole admission, answers for it as it does where
    // no admission runs, and gc never sweeps a marked build for it. A build laid aside is judged
    // by the tree its link names, as it stands ([`build_stands`]) and is looked up.
    let tree = aterm_toolchain::layout::aside_of(build_dir)
        .filter(|_| aterm_toolchain::layout::is_linked_aside(build_dir))
        .unwrap_or_else(|| build_dir.to_path_buf());
    if host.is_some_and(|host| {
        aterm_toolchain::gnu_runtime::admit_tree_on(&tree, host).is_err_and(|why| why.is_refusal())
    }) {
        return false;
    }
    match crate::metadata_io::read_bounded_regular_utf8(&marker, 4 * 1024 * 1024) {
        Ok(text) => {
            ready_text_accepts(&text, &running_platform())
                && ready_contents_accept(&text, build_dir)
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput
            ) =>
        {
            false
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// Whether `build_dir`'s ready record names a `bin/` tool that is provably gone, which makes
/// [`build_is_complete`] answer no. No record, or one this process cannot read, says nothing.
pub(crate) fn lost_a_recorded_tool(build_dir: &Path) -> bool {
    ready_marker_path(build_dir)
        .and_then(|marker| {
            crate::metadata_io::read_bounded_regular_utf8(&marker, 4 * 1024 * 1024).ok()
        })
        .is_some_and(|text| !ready_contents_accept(&text, build_dir))
}

/// Whether `build_dir` records the other slice of the universal binary.
/// # Errors
/// Its present marker cannot be admitted or its leading complete/platform evidence cannot
/// be read as bounded UTF-8. A caller deciding deletion must preserve this unknown case.
pub(crate) fn build_marks_another_slice(build_dir: &Path) -> std::io::Result<bool> {
    let Some(marker) = ready_marker_path(build_dir) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "build has no marker name",
        ));
    };
    let mut first = true;
    let mut another = false;
    // The producer writes ok, then platform, before its arbitrarily large bin inventory.
    // Legacy markers may lack platform; unknown/overlong evidence is kept out of GC.
    match crate::metadata_io::read_regular_lines(&marker, 4096, |line| {
        if first {
            first = false;
            if line.trim() != READY_OK_LINE {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ready marker has no complete header",
                ));
            }
            return Ok(true);
        }
        if let Some(platform) = line
            .trim()
            .strip_prefix(READY_PLATFORM_KEY)
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            another = platform != running_platform();
            return Ok(false);
        }
        Ok(true)
    }) {
        Ok(()) if first => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ready marker is empty",
        )),
        Ok(()) => Ok(another),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Atomically and durably mark `build_dir` complete.
pub fn mark_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let host = aterm_toolchain::gnu_runtime::host().map_err(std::io::Error::other)?;
    mark_build_ready_on(build_dir, host)
}

pub(crate) fn mark_build_ready_on(
    build_dir: &Path,
    host: Option<aterm_toolchain::gnu_runtime::Host>,
) -> std::io::Result<()> {
    if let Some(host) = host {
        aterm_toolchain::gnu_runtime::admit_tree_on(build_dir, host)
            .map_err(std::io::Error::other)?;
    }
    let dest = ready_marker_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let body = ready_body(build_dir);
    crate::metadata_io::write_record_durably(&dest, body.as_bytes())
}

fn ready_body(build_dir: &Path) -> String {
    ready_body_with_inventory(bin_inventory(build_dir).as_deref())
}

pub(crate) fn ready_body_with_inventory(inventory: Option<&str>) -> String {
    let mut body = format!(
        "{READY_OK_LINE}\n{READY_PLATFORM_KEY}{}\n",
        running_platform()
    );
    if let Some(contents) = inventory {
        body.push_str(READY_CONTENTS_KEY);
        body.push_str(contents);
        body.push('\n');
    }
    body
}

/// Local selection needs positively readable, canonical readiness, never GC's keep-on-error.
#[cfg(all(unix, test))]
pub(crate) fn local_build_ready(build_dir: &Path) -> std::io::Result<bool> {
    let marker = ready_marker_path(build_dir).ok_or(std::io::ErrorKind::InvalidInput)?;
    let text = crate::metadata_io::read_bounded_regular_utf8(&marker, 4096)?;
    Ok(bin_inventory(build_dir).is_some() && text == ready_body(build_dir))
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

/// Take down the completeness marker of every build under `prog_store`.
///
/// # Errors
/// The store scan, an entry's metadata or a marker removal failed, or the directory flush
/// failed other than as unavailable. Some markers may remain; the caller must remove no tree.
pub(crate) fn unmark_program_builds(prog_store: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(prog_store) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut unmarked = false;
    for entry in entries {
        let entry = entry?;
        // Numeric build dirs only, by the same test every other store scan uses: `current`
        // is a symlink, and `<n>.ready` / `<n>.provenance` / `<n>.shim-env` are files. A
        // build laid aside is its `<n>` link.
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if parse_build_name(name).is_none() {
            continue;
        }
        let path = entry.path();
        if !entry.file_type()?.is_dir() && !aterm_toolchain::layout::is_linked_aside(&path) {
            continue;
        }
        clear_build_ready(&path)?;
        unmarked = true;
    }
    if unmarked {
        // Required: every marker's removal reaches the media before any tree goes, or a power
        // cut part way through the removal can leave a marker over a torn tree.
        sync_dir_ordered(prog_store)?;
    }
    Ok(())
}

/// The suffix of the declared-mode record beside a build dir.
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
    if let Some(parent) = build_dir.parent().filter(|p| !p.as_os_str().is_empty()) {
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
            m.push_str(
                "): staged before this platform kept one; `aterm pkg uninstall <program>`, then \
                 `aterm pkg install <program>`, re-stages it",
            );
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
    /// Platform signer check refused ([`crate::install::StageError::SignerRefused`]).
    pub signer: bool,
    /// Whether the row is held rather than cooled down.
    pub held: bool,
    /// Recorded by an install of a build named by number, a lock's ([`MemoLane::Named`]): it
    /// stands for its own build, apart from the update lane's one memo per program.
    pub named: bool,
    /// The atpkg version that reached the verdict ([`aterm_stamp::app_version`]).
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

    /// Whether this record blocks re-staging these signed digests at `now_unix`.
    #[inline]
    #[must_use]
    pub fn binds(&self, digest: (DigestAlg, &str), tree_root: &str, now_unix: i64) -> bool {
        self.same_digests(digest, tree_root)
            && self.client == aterm_stamp::app_version()
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

/// Test shorthand: record that staging `build_dir` failed on the signed digests, in the update
/// lane ([`record_refusal_in`]).
#[cfg(test)]
pub(crate) fn record_stage_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    record_refusal_in(
        MemoLane::Update,
        RefusalKind::Cooldown,
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
    )
}

/// [`record_stage_refusal`] for a platform signer refusal ([`StageRefusal::signer`]).
#[cfg(test)]
pub(crate) fn record_signer_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    record_refusal_in(
        MemoLane::Update,
        RefusalKind::Signer,
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
    )
}

/// [`record_stage_refusal`] for a `tree_root` mismatch.
#[cfg(test)]
pub(crate) fn record_held_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    record_refusal_in(
        MemoLane::Update,
        RefusalKind::Held,
        build_dir,
        digest,
        tree_root,
        why,
        now_unix,
    )
}

/// Which install recorded a memo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoLane {
    /// An update or a pinned install: one memo per program, each new one replacing the rest.
    Update,
    /// An install of a build named by number (`install <p> --build <N>`, the build a
    /// `trust.lock` names). Several named builds of one program can fail at once (two trees
    /// locking two builds), so its memo neither replaces the program's other memos nor is
    /// replaced by them, and gc forgets it once its build is absent and it is older than
    /// [`crate::trust_lock::LOCKED_KEEP`] ([`is_stale_named_refusal`]).
    Named,
}

/// How a memo binds: from the second attempt for a cooldown, from the first otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefusalKind {
    /// Binds from the second attempt on the same digests, for a cooldown that grows.
    Cooldown,
    /// A platform signer refusal ([`StageRefusal::signer`]): binds from the first.
    Signer,
    /// A verdict on the pack (a tree root mismatch, or a sysroot bundle whose loader cannot
    /// resolve it here): binds from the first until the digests move or a retry forgets it.
    Held,
}

/// Record a stage refusal of `kind` for `build_dir` in `lane` ([`MemoLane`], [`RefusalKind`]).
pub(crate) fn record_refusal_in(
    lane: MemoLane,
    kind: RefusalKind,
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
) -> std::io::Result<()> {
    write_stage_refusal(build_dir, digest, tree_root, why, now_unix, kind, lane)
}

/// Whether the memo at `path` is a named build's ([`MemoLane::Named`]) recorded more than
/// [`crate::trust_lock::LOCKED_KEEP`] before `now_unix`: gc forgets it once its build is absent.
pub(crate) fn is_stale_named_refusal(path: &Path, now_unix: i64) -> bool {
    let keep = i64::try_from(crate::trust_lock::LOCKED_KEEP.as_secs()).unwrap_or(i64::MAX);
    crate::metadata_io::read_bounded_regular_utf8(path, MAX_REFUSAL_BYTES)
        .ok()
        .and_then(|text| parse_stage_refusal(&text))
        .is_some_and(|memo| memo.named && now_unix.saturating_sub(memo.at) > keep)
}

fn write_stage_refusal(
    build_dir: &Path,
    digest: (DigestAlg, &str),
    tree_root: &str,
    why: &str,
    now_unix: i64,
    kind: RefusalKind,
    lane: MemoLane,
) -> std::io::Result<()> {
    let dest = stage_refusal_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let attempts = stage_refusal(build_dir)
        .filter(|prior| prior.same_digests(digest, tree_root))
        .map_or(1, |prior| prior.attempts.saturating_add(1));
    let parent = dest.parent().unwrap_or(build_dir);
    make_build_parent(build_dir)?;
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
    if lane == MemoLane::Named {
        body.push_str("\nnamed=1");
    }
    body.push_str("\nclient=");
    body.push_str(aterm_stamp::app_version());
    body.push_str("\nwhy=");
    let one_line: String = why
        .replace(['\n', '\r'], " ")
        .chars()
        .take(MAX_REFUSAL_WHY_CHARS)
        .collect();
    body.push_str(&one_line);
    body.push('\n');
    crate::metadata_io::write_record_atomic(&dest, body.as_bytes())?;
    if lane == MemoLane::Update
        && let Some(name) = dest.file_name()
    {
        sweep_other_refusals(parent, name);
    }
    Ok(())
}

/// The update lane's one memo per program: every other memo in `dir` but `keep` goes, except a
/// named build's ([`MemoLane::Named`]), which stands for its own build.
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
            && !crate::metadata_io::read_bounded_regular_utf8(&entry.path(), MAX_REFUSAL_BYTES)
                .ok()
                .and_then(|text| parse_stage_refusal(&text))
                .is_some_and(|memo| memo.named)
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
    let (mut held, mut named, mut client) = (false, false, String::new());
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
            let s = v.trim();
            let digits = s.strip_prefix('-').unwrap_or(s);
            let canonical = !digits.is_empty()
                && !(digits.len() > 1 && digits.starts_with('0'))
                && s != "-0"
                && digits.bytes().all(|b| b.is_ascii_digit());
            at = canonical.then(|| s.parse::<i64>().ok()).flatten();
        } else if let Some(v) = line.strip_prefix("attempts=") {
            let s = v.trim();
            attempts =
                (!s.is_empty() && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| s.parse::<u32>().ok())
                    .flatten()
                    .filter(|n| *n >= 1);
        } else if let Some(v) = line.strip_prefix("signer=") {
            signer = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("hold=") {
            held = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("named=") {
            named = v.trim() == "1";
        } else if let Some(v) = line.strip_prefix("client=") {
            client = v.trim().to_string();
        }
    }
    let (alg, digest) = match (sha256, sha512) {
        (Some(v), None) => (DigestAlg::Sha256, v),
        (None, Some(v)) => (DigestAlg::Sha512, v),
        _ => return None,
    };
    if digest.is_empty() || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(StageRefusal {
        alg,
        digest: digest.to_string(),
        tree_root: tree_root?,
        why: why.unwrap_or_default(),
        at: at?,
        attempts: attempts?,
        signer,
        held,
        named,
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

/// The suffix of a build's record of the commands it puts on PATH
/// (`store/<program>/<build>.exposes`, [`write_exposes`]).
pub(crate) const EXPOSES_SUFFIX: &str = ".exposes";

pub(crate) const EXPOSES_HEADER: &str = "exposes v1";

const MAX_EXPOSES_BYTES: usize = 64 * 1024;

/// The suffix of a build's record of the version its signed manifest names
/// (`store/<program>/<build>.version`, [`write_version`]): display only, as the manifest's
/// `version` is, so a surface can say which release a build is without the manifest.
pub(crate) const VERSION_SUFFIX: &str = ".version";

pub(crate) const VERSION_HEADER: &str = "version v1";

/// The longest version a record keeps.
const MAX_VERSION_LEN: usize = 128;

/// Whether `version` is one a record keeps: 1 to [`MAX_VERSION_LEN`] bytes of visible ASCII
/// (no space or control byte), so it prints on one line as one word.
fn version_word(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= MAX_VERSION_LEN
        && version.bytes().all(|b| b.is_ascii_graphic())
}

/// Record `version`, the signed manifest's display version, beside `build_dir`
/// ([`write_sidecar_durably`]). A version that is empty or not one word ([`version_word`]) is not
/// recorded.
pub(crate) fn write_version(build_dir: &Path, version: &str) -> std::io::Result<()> {
    if !version_word(version) {
        return Ok(());
    }
    write_sidecar_durably(
        build_dir,
        VERSION_SUFFIX,
        format!("{VERSION_HEADER}\n{version}\n").as_bytes(),
    )
}

/// The version `build_dir`'s signed manifest named ([`write_version`]), or `None` for a build
/// staged before that record existed, one whose manifest named none, or a record that does not
/// parse.
#[must_use]
pub fn recorded_version(build_dir: &Path) -> Option<String> {
    let path = sidecar_path(build_dir, VERSION_SUFFIX)?;
    let text = crate::metadata_io::read_bounded_regular_utf8(&path, 2 * MAX_VERSION_LEN).ok()?;
    let mut lines = text.lines();
    if lines.next()? != VERSION_HEADER {
        return None;
    }
    let version = lines.next()?;
    (version_word(version) && lines.next().is_none()).then(|| version.to_string())
}

/// Metadata suffixes an admitted stage keeps beside its build.
/// The ready record has its own path ([`ready_marker_path`]). Garbage collection removes these
/// records after their build is gone.
pub const STAGE_SIDECAR_SUFFIXES: &[&str] = &[
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    crate::engineering_sdk::SUFFIX,
    crate::trust_lock::TREE_ROOT_SUFFIX,
    crate::trust_lock::LOCKED_SUFFIX,
    VENDOR_SIDECAR_SUFFIX,
    crate::shim_env::SIDECAR_SUFFIX,
    DECLARED_MODES_SUFFIX,
    EXPOSES_SUFFIX,
    VERSION_SUFFIX,
    crate::peers::UNIT_SUFFIX,
    #[cfg(unix)]
    crate::local_harness::SUFFIX,
];

/// Record `tools`, the admitted names of the build's signed `exposes`, beside `build_dir`
/// ([`write_sidecar_durably`]).
pub(crate) fn write_exposes(build_dir: &Path, tools: &[ToolName]) -> std::io::Result<()> {
    let mut body = String::from(EXPOSES_HEADER);
    body.push('\n');
    for tool in tools {
        body.push_str(tool.as_str());
        body.push('\n');
    }
    write_sidecar_durably(build_dir, EXPOSES_SUFFIX, body.as_bytes())
}

/// The commands `build_dir` recorded it puts on PATH ([`write_exposes`]), or `None` for a
/// build staged before that record existed or a record that does not parse.
pub(crate) fn recorded_exposes(build_dir: &Path) -> Option<Vec<ToolName>> {
    let path = sidecar_path(build_dir, EXPOSES_SUFFIX)?;
    let text = crate::metadata_io::read_bounded_regular_utf8(&path, MAX_EXPOSES_BYTES).ok()?;
    let mut lines = text.lines();
    if lines.next()? != EXPOSES_HEADER {
        return None;
    }
    lines
        .filter(|line| !line.is_empty())
        .map(ToolName::new)
        .collect()
}

/// `<build><suffix>` beside `build_dir`, or `None` for a path with no UTF-8 file name.
pub(crate) fn sidecar_path(build_dir: &Path, suffix: &str) -> Option<PathBuf> {
    let name = build_dir.file_name()?;
    let name = name.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(suffix);
    Some(build_dir.with_file_name(sidecar))
}

/// Write `<build><suffix>` durably.
pub(crate) fn write_sidecar_durably(
    build_dir: &Path,
    suffix: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    let dest = sidecar_path(build_dir, suffix).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    crate::metadata_io::write_record_durably(&dest, bytes)
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

/// The sibling scratch directory a stage extracts into before it swaps.
pub(crate) fn incoming_dir(build_dir: &Path) -> Option<PathBuf> {
    scratch_sibling(build_dir, ".incoming-")
}

/// Create `store/<program>/` ([`crate::extract::safe_mode`], [`crate::platform::make_dirs_born`]).
pub(crate) fn make_build_parent(build_dir: &Path) -> std::io::Result<()> {
    match build_dir.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            crate::platform::make_dirs_born(parent, crate::extract::safe_mode(0, true))
        }
        _ => Ok(()),
    }
}

/// The sibling the outgoing tree is moved to during a swap.
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
    if pid.is_empty()
        || pid.starts_with('0')
        || !pid.bytes().all(|b| b.is_ascii_digit())
        || pid.parse::<u32>().is_err()
    {
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

pub use aterm_toolchain::layout::{
    ExposedBuild, ExposedView, spotlight_exposed_builds, spotlight_exposed_views,
};

/// Whether a stage lays the tree `tree` aside, in `<build>.noindex` behind a `<build>` link
/// ([`aterm_toolchain::layout`]'s Spotlight layout): on macOS, a tree that ships a Rust sysroot.
pub(crate) fn lays_aside(tree: &Path) -> bool {
    cfg!(target_os = "macos") && aterm_toolchain::layout::ships_rust_sysroot(tree)
}

/// Whether nothing at `build_dir` holds a tree: nothing is there (by `lstat`: a dangling link
/// that is not the build's own is still something), or the build's link names an aside that
/// is gone.
pub(crate) fn tree_absent(build_dir: &Path) -> bool {
    match std::fs::symlink_metadata(build_dir) {
        Err(_) => true,
        Ok(_) => {
            aterm_toolchain::layout::is_linked_aside(build_dir)
                && aterm_toolchain::layout::aside_of(build_dir)
                    .is_some_and(|aside| std::fs::symlink_metadata(aside).is_err())
        }
    }
}

/// `lstat` of `path` as an answer: `Ok(true)` for a real directory, `Ok(false)` for
/// provably none (absent, `ENOTDIR` above it, or a non-directory such as a symlink at the
/// name), and `Err` when the question went unanswered (a component that cannot be
/// searched).
#[cfg(unix)]
pub(crate) fn real_dir(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Ok(m.is_dir()),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.raw_os_error() == Some(libc::ENOTDIR) =>
        {
            Ok(false)
        }
        Err(e) => Err(std::io::Error::new(
            e.kind(),
            format!("cannot lstat {} ({e})", path.display()),
        )),
    }
}

/// [`real_dir`] for the store build `build`, which also stands as its `<n>` link to a real
/// `<n>.noindex` once laid aside ([`aterm_toolchain::layout`]).
#[cfg(unix)]
pub(crate) fn build_stands(build: &Path) -> std::io::Result<bool> {
    match aterm_toolchain::layout::aside_of(build) {
        Some(aside) if aterm_toolchain::layout::is_linked_aside(build) => real_dir(&aside),
        _ => real_dir(build),
    }
}

/// Elsewhere no build is laid aside ([`lays_aside`]).
#[cfg(not(unix))]
pub(crate) fn build_stands(build: &Path) -> std::io::Result<bool> {
    Ok(std::fs::symlink_metadata(build).is_ok_and(|m| m.is_dir()))
}

/// Point `build_dir` at its aside: the link `<build>` -> `<build>.noindex`, relative.
#[cfg(unix)]
pub(crate) fn link_aside(build_dir: &Path) -> std::io::Result<()> {
    let aside = aterm_toolchain::layout::aside_of(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let name = aside.file_name().unwrap_or_default();
    std::os::unix::fs::symlink(name, build_dir)
}

/// Elsewhere no build is laid aside ([`lays_aside`]).
#[cfg(not(unix))]
pub(crate) fn link_aside(_build_dir: &Path) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// Lay back the `<build>` link a crash kept from an aside (complete or partial) when nothing is
/// at `build_dir` and `<build>.noindex` is a real directory; whether it did.
pub(crate) fn relink_aside(build_dir: &Path) -> bool {
    let Some(aside) = aterm_toolchain::layout::aside_of(build_dir) else {
        return false;
    };
    std::fs::symlink_metadata(build_dir).is_err()
        && std::fs::symlink_metadata(&aside).is_ok_and(|m| m.is_dir())
        && link_aside(build_dir).is_ok()
}

/// Move the real build directory `build_dir` into `<build>.noindex` and link `<build>` to it.
/// The caller has proved nothing runs from it and nothing names it as the active build:
/// a tree macOS may still be assessing a first exec in never moves.
///
/// # Errors
/// `build_dir` is not a real directory, its aside's name is taken, or the rename or the link
/// failed (a failed link puts the directory back).
pub(crate) fn lay_aside(build_dir: &Path) -> std::io::Result<()> {
    let _first_exec = first_exec_removal(build_dir)?;
    let aside = aterm_toolchain::layout::aside_of(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    if !std::fs::symlink_metadata(build_dir)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a real directory", build_dir.display()),
        ));
    }
    if std::fs::symlink_metadata(&aside).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} is already there", aside.display()),
        ));
    }
    std::fs::rename(build_dir, &aside)?;
    if let Err(e) = link_aside(build_dir) {
        let _ = std::fs::rename(&aside, build_dir);
        return Err(e);
    }
    // Accepted, failed or not: the move and its link are in place, and a lay-aside a power
    // cut takes back in part (the tree moved, no link yet) is linked again by relink_aside.
    if let Some(parent) = build_dir.parent() {
        let _moved = sync_dir_ordered(parent);
    }
    Ok(())
}

/// Recover the one crash window a swap cannot make atomic.
pub(crate) fn recover_interrupted_swap(build_dir: &Path) -> bool {
    let Some(program) = build_dir.parent() else {
        return false;
    };
    let Ok(first_exec) = first_exec::program_gate(program) else {
        return false;
    };
    recover_interrupted_swap_under_gate(build_dir, &first_exec)
}

fn recover_interrupted_swap_under_gate(build_dir: &Path, first_exec: &FirstExecRemoval) -> bool {
    if first_exec.check(build_dir).is_err() {
        return false;
    }
    relink_aside(build_dir);
    // `symlink_metadata`, not `exists()`: a dangling symlink at the build path is still
    // something there, and the window means nothing is, unless it is the build's own link
    // to an aside the swap moved off, which is where the parked tree goes back.
    let back_to = if std::fs::symlink_metadata(build_dir).is_err() {
        build_dir.to_path_buf()
    } else if tree_absent(build_dir)
        && let Some(aside) = aterm_toolchain::layout::aside_of(build_dir)
    {
        aside
    } else {
        return false;
    };
    let mut superseded = scratch_siblings(build_dir).into_iter().filter(|(kind, p)| {
        *kind == Scratch::Superseded && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
    });
    let Some((_, old)) = superseded.next() else {
        return false;
    };
    if superseded.next().is_some() {
        return false;
    }
    if first_exec.check(&old).is_err() || first_exec.check(&back_to).is_err() {
        return false;
    }
    std::fs::rename(&old, back_to).is_ok()
}

/// Delete stage-scratch siblings after recovery, under the stable program gate.
pub(crate) fn sweep_stage_scratch(build_dir: &Path) {
    let Some(program) = build_dir.parent() else {
        return;
    };
    let Ok(first_exec) = first_exec::program_gate(program) else {
        return;
    };
    let recovered = recover_interrupted_swap_under_gate(build_dir, &first_exec);
    let parked_only_copy = !recovered && tree_absent(build_dir);
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
                let _ = remove_tree_under_gate(&path, &first_exec);
            }
            Ok(_) => {
                if first_exec.check(&path).is_ok() {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Err(_) => {}
        }
    }
}

/// A full flush of this directory's entries ([`aterm_durable::Flush::dir_best_effort`]): a
/// directory the volume cannot flush, or cannot open, is no error (no flush is to be had
/// there), and a failed flush is returned. Each store step says at its call what a failed
/// one means: one that orders a mark's removal ahead of a tree's stops before the tree.
///
/// # Errors
/// The flush failed other than as unavailable.
pub(crate) fn sync_dir(dir: &Path) -> std::io::Result<()> {
    aterm_durable::Flush::Durable
        .dir_best_effort(dir)
        .map(|_flushed_or_unavailable| ())
}

/// [`sync_dir`] ordered, not full ([`aterm_durable::Flush::Ordered`]).
///
/// # Errors
/// As [`sync_dir`].
pub(crate) fn sync_dir_ordered(dir: &Path) -> std::io::Result<()> {
    aterm_durable::Flush::Ordered
        .dir_best_effort(dir)
        .map(|_flushed_or_unavailable| ())
}

/// [`crate::platform::sync_file_contents`], with a refusal degraded to success
/// ([`aterm_durable::refusal_accepted`]: Darwin's `ENOTSUP` among them).
pub(crate) fn sync_contents_or_accept_refusal(f: &std::fs::File) -> std::io::Result<()> {
    aterm_durable::refusal_accepted(crate::platform::sync_file_contents(f))
}

/// What [`sync_tree`] flushed.
#[derive(Debug, Default)]
pub(crate) struct Synced {
    /// Regular files whose contents were pushed to the filesystem.
    pub files: u64,
    /// Directories whose entries were pushed to the filesystem.
    pub dirs: u64,
}

/// Flush a staged tree.
pub(crate) fn sync_tree(root: &Path) -> std::io::Result<Synced> {
    let mut counts = Synced::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for child in std::fs::read_dir(&dir)? {
            let child = child?;
            let path = child.path();
            let ft = child.file_type()?;
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
        if let Ok(handle) = crate::platform::open_for_sync(&dir) {
            sync_contents_or_accept_refusal(&handle)?;
            counts.dirs = counts.dirs.saturating_add(1);
        }
    }
    Ok(counts)
}

/// The mode of a single-binary build's `bin/` once its file is laid ([`seal_bin`]).
pub(crate) const SEALED_BIN_MODE: u32 = 0o555;

/// Seal a staged tree's `bin/` at [`SEALED_BIN_MODE`] ([`crate::install::verify_and_stage`]).
pub(crate) fn seal_bin(tree: &Path) -> std::io::Result<()> {
    crate::platform::set_mode(&tree.join("bin"), SEALED_BIN_MODE)
}

/// Give a sealed `<tree>/bin` ([`seal_bin`]) its owner's write bit back.
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

/// Remove the store tree at `dir` ([`unseal_bin`]); a build laid aside goes whole, its
/// `<build>.noindex` tree first and then the `<build>` link, so a crash between the two
/// leaves a link to nothing, which the next sweep removes.
pub(crate) fn remove_tree(dir: &Path) -> std::io::Result<()> {
    remove_tree_after_admission(dir, || {})
}

fn remove_tree_after_admission(dir: &Path, admitted: impl FnOnce()) -> std::io::Result<()> {
    let first_exec = first_exec_removal(dir)?;
    admitted();
    remove_tree_under_gate(dir, &first_exec)
}

/// Remove under an already-held program gate. Swap, discard and scratch sweep must not
/// reacquire that nonblocking lock; every other generic caller enters through `remove_tree`.
pub(crate) fn remove_tree_under_gate(
    dir: &Path,
    first_exec: &FirstExecRemoval,
) -> std::io::Result<()> {
    first_exec.check(dir)?;
    if aterm_toolchain::layout::is_linked_aside(dir)
        && let Some(aside) = aterm_toolchain::layout::aside_of(dir)
    {
        if std::fs::symlink_metadata(&aside).is_ok_and(|m| m.is_dir()) {
            unseal_bin(&aside);
            std::fs::remove_dir_all(&aside)?;
        }
        return std::fs::remove_file(dir);
    }
    unseal_bin(dir);
    std::fs::remove_dir_all(dir)
}

/// Legacy scalar marks remain readable beside independent first-start marks.
pub(crate) const UNPROVEN_SUFFIX: &str = ".unproven";

#[path = "store_first_exec.rs"]
mod first_exec;
#[cfg(all(test, unix))]
pub(crate) use first_exec::MARKS_SUFFIX as FIRST_EXEC_MARKS_SUFFIX;
pub use first_exec::{FirstExec, hold_first_exec};
pub(crate) use first_exec::{
    Removal as FirstExecRemoval, first_exec_program_removal, first_exec_removal, held_build_in,
    held_unproven,
};
#[cfg(all(test, unix))]
use first_exec::{held_unproven_at, hold_first_exec_at};

#[cfg(all(test, unix))]
#[path = "store_first_exec_tests.rs"]
mod first_exec_tests;

/// Discard a build after its marker's removal is flushed. A failed removal or flush
/// retains the tree; a held build ([`held_unproven`]) stays unmarked until a later sweep.
pub(crate) fn discard_build(build_dir: &Path) {
    discard_build_as(build_dir, Memo::Clear);
}

/// [`discard_build`], keeping the build's stage refusal memo ([`stage_refusal`]): gc's sweep of a
/// tree that never completed, and a named build its resolve check refused, whose memo is a
/// verdict on the pack, not on the tree.
pub(crate) fn discard_build_keeping_memo(build_dir: &Path) {
    discard_build_as(build_dir, Memo::Keep);
}

/// Whether a discard forgets the build's stage refusal memo.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Memo {
    Clear,
    Keep,
}

fn discard_build_as(build_dir: &Path, memo: Memo) {
    let Ok(sdk) = crate::compat::sdk_mutation_guard_for_build(build_dir) else {
        return;
    };
    match sdk {
        Some(guard) => discard_build_as_reclaimed(build_dir, memo, &guard),
        None => discard_build_after_sdk_admission(build_dir, memo),
    }
}

pub(crate) fn discard_build_reclaimed(build_dir: &Path, guard: &crate::lease::ReclaimGuard) {
    discard_build_as_reclaimed(build_dir, Memo::Clear, guard);
}

pub(crate) fn discard_build_keeping_memo_reclaimed(
    build_dir: &Path,
    guard: &crate::lease::ReclaimGuard,
) {
    discard_build_as_reclaimed(build_dir, Memo::Keep, guard);
}

fn discard_build_as_reclaimed(build_dir: &Path, memo: Memo, guard: &crate::lease::ReclaimGuard) {
    if crate::compat::check_sdk_reclaim(build_dir, guard).is_err() {
        return;
    }
    discard_build_after_sdk_admission(build_dir, memo);
}

fn discard_build_after_sdk_admission(build_dir: &Path, memo: Memo) {
    let Ok(first_exec) = first_exec_removal(build_dir) else {
        return;
    };
    if clear_build_ready(build_dir).is_err() {
        return;
    }
    // Required: the marker's removal reaches the media before the tree goes, or a power cut
    // part way through the removal can leave the marker over a torn tree.
    let unmarked = build_dir
        .parent()
        .is_none_or(|parent| sync_dir(parent).is_ok());
    if !unmarked || held_unproven(build_dir) {
        return;
    }
    crate::compat::discard_root_of(build_dir);
    let _ = remove_tree_under_gate(build_dir, &first_exec);
    if let Some(mark) = sidecar_path(build_dir, UNPROVEN_SUFFIX) {
        let _ = std::fs::remove_file(mark);
    }
    if let Some(marker) = ready_marker_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
    if let Some(name) = build_dir.file_name().and_then(|n| n.to_str()) {
        let mut prov = String::from(name);
        prov.push_str(".provenance");
        let _ = std::fs::remove_file(build_dir.with_file_name(prov));
    }
    crate::shim_env::remove_sidecar(build_dir);
    if memo == Memo::Clear {
        clear_stage_refusal(build_dir);
    }
    clear_declared_modes(build_dir);
    for &suffix in STAGE_SIDECAR_SUFFIXES {
        if let Some(record) = sidecar_path(build_dir, suffix) {
            let _ = std::fs::remove_file(record);
        }
    }
}

/// Take a refused build that [`discard_build`] left in place (its first-exec hold keeps the tree)
/// out of every reader's reach: its ready marker and its tree-root record go, so completeness
/// checks, discovery and a lock's standing read it absent and gc's partial arm sweeps the tree
/// once the hold lets go. Best effort: a marker that cannot be removed leaves the build as it was.
pub(crate) fn unmark_refused(build_dir: &Path) {
    if clear_build_ready(build_dir).is_err() {
        return;
    }
    if let Some(record) = sidecar_path(build_dir, crate::trust_lock::TREE_ROOT_SUFFIX) {
        let _ = std::fs::remove_file(record);
    }
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

/// Resolve the layout the user configured.
#[must_use]
pub fn resolve_configured() -> Option<Layout> {
    resolve_from(&crate::config::load())
}

/// Resolve the same admitted prefix without configuration notices or host-log writes.
pub(crate) fn resolve_for_lookup() -> Result<Option<Layout>, &'static str> {
    let cfg = crate::config::load_for_lookup()?;
    let Some(home) = aterm_types::dirs::home_dir() else {
        return Ok(None);
    };
    let configured = cfg.prefix_path(Some(&home));
    Ok(Some(Layout {
        prefix: vet_prefix_reporting(configured.as_deref(), &home, false),
    }))
}

/// [`resolve_configured`] over a `[packages]` table already read.
#[must_use]
pub fn resolve_from(cfg: &crate::config::PackagesConfig) -> Option<Layout> {
    resolve(
        cfg.prefix_path(aterm_types::dirs::home_dir().as_deref())
            .as_deref(),
    )
}

/// Validate a configured prefix against `home` ([`default_prefix`]).
#[must_use]
pub fn vet_prefix(configured: Option<&Path>, home: &Path) -> PathBuf {
    vet_prefix_reporting(configured, home, true)
}

fn vet_prefix_reporting(configured: Option<&Path>, home: &Path, report: bool) -> PathBuf {
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
            // A host's log keeps the configured path by its length: it is the person's
            // aterm.toml (`[packages].prefix`).
            let said = p.display().to_string();
            let notice = |prefix: &dyn std::fmt::Display| {
                format!(
                    "configured prefix {prefix} is not writable by this user — using the default \
                     {} instead (set [packages].prefix only for a shared multi-user store this \
                     user can write)",
                    default.display()
                )
            };
            if report {
                crate::notice::say_once_with_log(
                    &notice(&said),
                    &notice(&aterm_log::words::Len(&said)),
                );
            }
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
#[path = "store_system_shape_tests.rs"]
pub(crate) mod system_shape;

fn system_chain_trusted(p: &Path) -> bool {
    p.ancestors().all(|anc| {
        std::fs::symlink_metadata(anc).map_or(true, |m| {
            !crate::platform::is_reparse(&m) && crate::platform::dir_meta_is_system(&m)
        })
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

/// A logical tool name.
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
    /// Admit `raw` as a tool name ([`shim_allowed`]).
    #[inline]
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        shim_allowed(raw).then(|| Self(raw.to_string()))
    }

    /// The `bin/` **shim** file name: `<tool><SHIM_SUFFIX>`.
    #[inline]
    #[must_use]
    pub fn shim_file(&self) -> String {
        with_suffix(&self.0, crate::platform::SHIM_SUFFIX)
    }

    /// The **executable** file name inside a build's `bin/`: `<tool><EXE_SUFFIX>`.
    #[inline]
    #[must_use]
    pub fn exe_file(&self) -> String {
        with_suffix(&self.0, crate::platform::EXE_SUFFIX)
    }

    /// Recover the logical name from a `bin/` entry ([`crate::platform::SHIM_SUFFIX`]).
    #[inline]
    #[must_use]
    pub fn from_shim_file(name: &str) -> Option<Self> {
        Self::new(without_suffix(name, crate::platform::SHIM_SUFFIX))
    }

    /// The logical name.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is an [`ALIAS_PREFIX`]ed name.
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

    /// The tool an alias names: `alab-trust` → `trust`.
    #[inline]
    #[must_use]
    pub fn alias_base(&self) -> Option<Self> {
        self.0.strip_prefix(ALIAS_PREFIX).and_then(Self::new)
    }
}

/// Split raw `exposes` into allowed [`ToolName`]s and refused names.
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

/// Move `bin_dir` to the front of `inherited` `PATH`, appearing once so managed tools (`git`,
/// `gh`) resolve ahead of `/usr/bin` inside an `aterm` session even when an outer shell appended
/// `bin_dir` last.
#[must_use]
pub fn prepend_bin_to_path(inherited: Option<&OsStr>, bin_dir: &Path) -> OsString {
    let mut dirs = vec![bin_dir.to_path_buf()];
    if let Some(p) = inherited
        && !p.is_empty()
    {
        dirs.extend(std::env::split_paths(p).filter(|d| d != bin_dir));
    }
    std::env::join_paths(&dirs)
        .unwrap_or_else(|_| inherited.map(OsStr::to_os_string).unwrap_or_default())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
