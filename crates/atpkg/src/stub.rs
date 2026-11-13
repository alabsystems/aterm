// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pending-program stubs (R6).

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use crate::store::{Layout, ToolName};
use aterm_toolchain::lasting::temp_roots;

/// The compile-time default-set roster the stubs cover before the first signed index resolves.
pub const DEFAULT_SET_STUB_NAMES: &[(&str, &str)] = &[
    ("ay", "ALab's SMT solver"),
    ("clean", "ALab's theorem prover"),
    ("nn", "ALab's neural-network tool"),
    ("ny", "ALab's neural-network verifier"),
    (
        "trust",
        "the Trust compiler bundle — a Rust compiler that verifies what it compiles",
    ),
    (
        "trust-cg",
        "the Trust compiler's codegen member (coherence group)",
    ),
    (
        "trust-ir",
        "the Trust compiler's IR member (coherence group)",
    ),
    ("trust-mc", "the Trust model checker"),
    (
        "trust-vc",
        "the Trust compiler's verification-condition member (coherence group)",
    ),
    ("ty", "ALab's specification checker"),
];

/// Coding agents managed by `aterm` vendor-direct, one per [`VENDORS`](crate::vendor_direct::VENDORS)
/// row, every one in the default set.
pub const AGENT_PROGRAMS: &[&str] = &{
    use crate::vendor_direct::VENDORS;
    let mut names = [""; VENDORS.len()];
    let mut i = 0;
    while i < names.len() {
        names[i] = VENDORS[i].program;
        i += 1;
    }
    names
};

/// Whether `name` is one of [`AGENT_PROGRAMS`].
#[must_use]
pub fn is_agent_program(name: &str) -> bool {
    AGENT_PROGRAMS.contains(&name)
}

/// The authored one-line description for `name`.
#[must_use]
pub fn describe(name: &str) -> Option<&'static str> {
    DEFAULT_SET_STUB_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, d)| *d)
        .or_else(|| crate::vendor_direct::spec(name).map(|s| s.description))
}

fn compiled_names() -> impl Iterator<Item = &'static str> {
    DEFAULT_SET_STUB_NAMES
        .iter()
        .map(|(n, _)| *n)
        .chain(AGENT_PROGRAMS.iter().copied())
}

fn commands_of(program: &str) -> impl Iterator<Item = &'static str> + '_ {
    crate::reroute::TABLE
        .iter()
        .filter(move |row| crate::reroute::program_of(row) == program)
        .map(crate::reroute::branded_of)
        .filter(move |branded| *branded != program)
}

/// The program a pending stub named `name` reports on and waits for.
#[must_use]
pub fn program_of(name: &str) -> &str {
    match crate::reroute::TABLE
        .iter()
        .find(|row| crate::reroute::branded_of(row) == name)
    {
        Some(row) => crate::reroute::program_of(row),
        None => name,
    }
}

/// [`program_of`] in `layout`.
#[must_use]
pub fn program_of_in(layout: &Layout, name: &str) -> String {
    let table = program_of(name);
    if table != name {
        return table.to_string();
    }
    ToolName::new(name)
        .and_then(|tool| stub_comes_with(&layout.shim(&tool)))
        .unwrap_or_else(|| name.to_string())
}

fn stub_comes_with(path: &Path) -> Option<String> {
    stub_comes_with_in(&read_pending_stub(path)?)
}

fn stub_comes_with_in(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|l| {
            l.strip_prefix("# ")
                .or_else(|| l.strip_prefix("rem "))?
                .strip_prefix(COMES_WITH)
        })
        .and_then(|program| ToolName::new(program.trim()))
        .map(|program| program.as_str().to_string())
}

fn command_stub_executables<'a>(
    layout: &Layout,
    program: &str,
    exposed: &'a [String],
    embedder: &Embedder,
    keep: &mut BTreeSet<&'a str>,
    files: &mut Vec<crate::lay::Executable>,
) {
    let mut commands: Vec<&'a str> = commands_of(program).collect();
    for name in exposed {
        if name != program && !commands.contains(&name.as_str()) {
            commands.push(name);
        }
    }
    for command in commands {
        let Some(tool) = ToolName::new(command) else {
            continue;
        };
        match pending_command_stub_executable(layout, &tool, program, embedder) {
            Ok(Some(file)) => {
                files.push(file);
                keep.insert(command);
            }
            Ok(None) => {
                keep.insert(command);
            }
            Err(_) => {}
        }
    }
}

const STUB_MARKER: &str = "# atpkg pending-program stub v1";

const COMES_WITH: &str = "comes with: ";

/// Static last-resort fallback message when `atpkg` is unavailable.
pub const STUB_UNREACHABLE_MSG: &str =
    "aterm's package manager is not reachable — open aterm to finish installing";

/// Single-quote `s` for safe embedding in a `/bin/sh` script (the POSIX `'\''` escape).
pub(crate) fn sh_single_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn sh_single_unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut rest = word;
    while !rest.is_empty() {
        if let Some(quoted) = rest.strip_prefix('\'') {
            let end = quoted.find('\'')?;
            out.push_str(&quoted[..end]);
            rest = &quoted[end + 1..];
        } else {
            rest = rest.strip_prefix("\\'")?;
            out.push('\'');
        }
    }
    Some(out)
}

/// The atpkg a laid `/bin/sh` body names on its `<var>=` line.
#[must_use]
pub(crate) fn sh_named_atpkg(body: &str, var: &str) -> Option<PathBuf> {
    body.lines()
        .find_map(|line| {
            let value = line.trim_start().strip_prefix(var)?.strip_prefix('=')?;
            sh_single_unquote(value.trim_end())
        })
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// The atpkg a laid batch stub ([`stub_content_cmd`]) names on its `if exist "…" (` line, or
/// `None` when it names none.
#[must_use]
fn cmd_named_atpkg(body: &str) -> Option<PathBuf> {
    body.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("if exist \"")?
                .strip_suffix("\" (")
                .map(str::to_owned)
        })
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn stub_named_atpkg(body: &str) -> Option<PathBuf> {
    sh_named_atpkg(body, "ATPKG").or_else(|| cmd_named_atpkg(body))
}

/// The stub script body for `tool`, naming `atpkg` as the fallback-1 path, or none
/// ([`Embedder::embed`] decides).
#[must_use]
fn stub_content(tool: &ToolName, comes_with: Option<&str>, atpkg: Option<&Path>) -> String {
    let atpkg = named(atpkg);
    if cfg!(windows) {
        stub_content_cmd(tool, comes_with, atpkg)
    } else {
        stub_content_sh(tool, comes_with, atpkg)
    }
}

/// How a renderer is told to name no atpkg: the empty path.
#[must_use]
pub(crate) fn named(atpkg: Option<&Path>) -> &Path {
    atpkg.unwrap_or_else(|| Path::new(""))
}

/// The POSIX `/bin/sh` stub body (macOS/Linux — the shim there is a plain
/// executable file).
#[must_use]
fn stub_content_sh(tool: &ToolName, comes_with: Option<&str>, atpkg: &Path) -> String {
    let name = sh_single_quote(tool.as_str());
    let mut s = String::from("#!/bin/sh\n");
    s.push_str(STUB_MARKER);
    s.push('\n');
    if let Some(program) = comes_with {
        s.push_str(&format!("# {COMES_WITH}{program}\n"));
    }
    s.push_str("# Replaced by the real shim when the program installs.\nATPKG=");
    s.push_str(&sh_single_quote(&atpkg.to_string_lossy()));
    s.push_str("\nif [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" __pending ");
    s.push_str(&name);
    s.push_str(" \"$@\"\nfi\n");
    s.push_str(&crate::reroute::sh_path_walk(
        "",
        "atpkg",
        &format!("__pending {name} "),
        &[],
    ));
    s.push_str("printf '%s\\n' ");
    s.push_str(&sh_single_quote(STUB_UNREACHABLE_MSG));
    s.push_str(" 1>&2\nexit 127\n");
    s
}

const CMD_STUB_FIRST_LINE: &str = "@echo off\r\n";

const _: () = assert!(CMD_STUB_FIRST_LINE.len() >= crate::platform::CMD_FRAME_HEAD.len() - 2);

/// The batch twin: on Windows the shim slot is `bin/<tool>.cmd`
/// ([`crate::store::ToolName::shim_file`]), where cmd.exe would misread a `#!/bin/sh` body.
#[must_use]
fn stub_content_cmd(tool: &ToolName, comes_with: Option<&str>, atpkg: &Path) -> String {
    let name = tool.as_str();
    let atpkg = atpkg.to_string_lossy();
    let mut s = String::from(CMD_STUB_FIRST_LINE);
    s.push_str("rem ");
    s.push_str(STUB_MARKER);
    s.push_str("\r\n");
    if let Some(program) = comes_with {
        s.push_str(&format!("rem {COMES_WITH}{program}\r\n"));
    }
    s.push_str("rem Replaced by the real shim when the program installs.\r\n");
    if !atpkg.is_empty() {
        s.push_str(&format!(
            "if exist \"{atpkg}\" (\r\n  \"{atpkg}\" __pending \"{name}\" %*\r\n  exit /b 127\r\n)\r\n"
        ));
    }
    s.push_str(&format!(
        "where atpkg >nul 2>nul\r\nif not errorlevel 1 (\r\n  atpkg __pending \"{name}\" %*\r\n  exit /b 127\r\n)\r\n"
    ));
    let unreachable = STUB_UNREACHABLE_MSG.replace('\u{2014}', "-");
    s.push_str(&format!("echo {unreachable} 1>&2\r\nexit /b 127\r\n"));
    crate::platform::cmd_framed(&s)
}

/// Whether `name` can ride a batch script without becoming syntax:
/// [`crate::store::shim_allowed`] admits quotes, `%`, carets and ampersands, and cmd.exe
/// has no robust quoting (`%VAR%` expands even inside double quotes).
#[must_use]
fn cmd_stub_name_safe(name: &str) -> bool {
    !name.chars().any(|c| {
        matches!(
            c,
            '"' | '%' | '^' | '&' | '<' | '>' | '|' | '!' | '\r' | '\n'
        )
    })
}

/// The `atpkg` beside the running executable, what this process offers as fallback 1's path ([`Embedder`] decides whether a laid file names it).
pub(crate) fn co_located_atpkg_path() -> PathBuf {
    let atpkg = format!("atpkg{}", std::env::consts::EXE_SUFFIX);
    let path = running_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(&atpkg)))
        .unwrap_or_else(|| PathBuf::from(atpkg));
    if cfg!(windows) {
        crate::platform::strip_verbatim_prefix(&path)
    } else {
        path
    }
}

fn running_exe() -> io::Result<PathBuf> {
    #[cfg(all(test, unix))]
    {
        if let Some(exe) = CURRENT_EXE.with(|e| e.borrow().clone()) {
            return Ok(exe);
        }
    }
    std::env::current_exe()
}

/// Whether this exact physical path still names the kernel's loaded image. Retained
/// helper code is not an observation of the producer's original file identity.
pub(crate) fn is_running_image(candidate: &Path) -> bool {
    let running = running_exe().and_then(std::fs::canonicalize).ok();
    if running.as_deref() != Some(candidate) {
        return false;
    }
    #[cfg(all(test, unix))]
    if CURRENT_EXE.with(|e| e.borrow().is_some()) {
        return true;
    }
    i32::try_from(std::process::id())
        .ok()
        .is_some_and(|pid| aterm_uds::process::image_matches_file(pid, candidate) == Some(true))
}

#[cfg(all(test, unix))]
thread_local! {
    static CURRENT_EXE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with `exe` as the path this process runs from on this thread ([`running_exe`]).
#[cfg(all(test, unix))]
pub(crate) fn with_current_exe<R>(exe: &Path, f: impl FnOnce() -> R) -> R {
    let prior = CURRENT_EXE.with(|e| e.replace(Some(exe.to_path_buf())));
    let out = f();
    CURRENT_EXE.with(|e| {
        *e.borrow_mut() = prior;
    });
    out
}

/// Whether `path` is an atpkg a laid file can hand over to.
#[must_use]
pub(crate) fn is_live_atpkg(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// How long an atpkg path can be expected to keep standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Durability {
    /// Goes away on its own.
    Transient,
    /// A plain binary anywhere else.
    Plain,
    /// Inside an app bundle (`…/<X>.app/Contents/MacOS/`) anywhere not transient.
    Bundle,
}

fn in_app_bundle(path: &Path) -> bool {
    let Some(macos) = path.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && contents
            .parent()
            .and_then(Path::extension)
            .is_some_and(|ext| ext == "app")
}

/// The [`Durability`] of the atpkg at `atpkg`, with `temp_roots` the machine's temporary directories ([`temp_roots`]).
#[must_use]
pub(crate) fn durability(atpkg: &Path, temp_roots: &[PathBuf]) -> Durability {
    if aterm_toolchain::lasting::transient(atpkg, temp_roots) {
        return Durability::Transient;
    }
    if in_app_bundle(atpkg) {
        Durability::Bundle
    } else {
        Durability::Plain
    }
}

/// Whether the executable at `path` is not in a transient root ([`Durability::Transient`],
/// [`aterm_toolchain::lasting`]).
#[must_use]
pub fn lasting(path: &Path) -> bool {
    aterm_toolchain::lasting::lasting(path)
}

/// Which atpkg a file laid now names.
#[derive(Debug, Clone)]
pub(crate) struct Embedder {
    /// The atpkg this process offers ([`co_located_atpkg_path`]); [`Self::embed`] takes it
    /// only when it is live.
    offered: PathBuf,
    /// The machine's temporary directories ([`temp_roots`]).
    temp_roots: Vec<PathBuf>,
}

impl Embedder {
    /// The embedder of this process: its co-located atpkg, this machine's temp dirs.
    #[must_use]
    pub(crate) fn this_process() -> Self {
        Self {
            offered: co_located_atpkg_path(),
            temp_roots: temp_roots(),
        }
    }

    /// An embedder offering `offered`, with `temp_roots` as the temporary directories.
    #[cfg(all(test, unix))]
    #[must_use]
    pub(crate) fn offering(offered: &Path, temp_roots: &[PathBuf]) -> Self {
        Self {
            offered: offered.to_path_buf(),
            temp_roots: temp_roots.to_vec(),
        }
    }

    /// The atpkg a file names once laid, given the one it names now (`standing`: `None` for a file not there yet or naming none), or `None` to name none.
    #[must_use]
    pub(crate) fn embed(&self, standing: Option<&Path>) -> Option<PathBuf> {
        let live = |path: &Path| path.is_absolute() && is_live_atpkg(path);
        let offered = Some(self.offered.as_path()).filter(|p| live(p));
        let standing = standing.filter(|p| live(p));
        match (offered, standing) {
            (None, None) => None,
            (Some(offered), None) => Some(offered.to_path_buf()),
            (None, Some(standing)) => Some(standing.to_path_buf()),
            (Some(offered), Some(standing)) => {
                let keep = offered != standing
                    && durability(offered, &self.temp_roots)
                        < durability(standing, &self.temp_roots);
                Some(if keep { standing } else { offered }.to_path_buf())
            }
        }
    }
}

fn is_marker_line(line: &str, marker: &str) -> bool {
    let l = line.trim();
    l == marker
        || l.strip_prefix("rem ")
            .is_some_and(|rest| rest.trim() == marker)
}

pub(crate) fn is_pending_stub_body(text: &str) -> bool {
    text.lines().any(|l| is_marker_line(l, STUB_MARKER))
}

fn read_pending_stub(path: &Path) -> Option<String> {
    let text = crate::metadata_io::read_bounded_regular_utf8(path, 64 * 1024).ok()?;
    is_pending_stub_body(&text).then_some(text)
}

/// Whether the file at `path` is a pending stub written by this module.
#[must_use]
pub fn is_pending_stub(path: &Path) -> bool {
    read_pending_stub(path).is_some()
}

/// Whether `tool` currently resolves to a pending stub in this layout.
#[must_use]
pub fn pending_stub_exists(layout: &Layout, tool: &str) -> bool {
    ToolName::new(tool).is_some_and(|t| is_pending_stub(&layout.shim(&t)))
}

/// Lay (or refresh) the pending stub for `tool`, atomically (temp `0755` + `rename(2)`, the tombstone writer's shape).
#[cfg(test)]
pub(crate) fn write_pending_stub(layout: &Layout, tool: &ToolName) -> io::Result<()> {
    match pending_stub_executable(layout, tool)? {
        Some(file) => crate::lay::write_regenerable(&file),
        None => Ok(()),
    }
}

/// The pending stub `write_pending_stub` would lay, rendered but not written.
#[cfg(test)]
pub(crate) fn pending_stub_executable(
    layout: &Layout,
    tool: &ToolName,
) -> io::Result<Option<crate::lay::Executable>> {
    pending_bin_stub_executable(layout, tool, None, &Embedder::this_process())
}

fn pending_command_stub_executable(
    layout: &Layout,
    tool: &ToolName,
    program: &str,
    embedder: &Embedder,
) -> io::Result<Option<crate::lay::Executable>> {
    let Some(program) = ToolName::new(program) else {
        return Ok(None);
    };
    if program.as_str().chars().any(char::is_control)
        || (cfg!(windows) && !cmd_stub_name_safe(program.as_str()))
    {
        return Ok(None);
    }
    pending_bin_stub_executable(layout, tool, Some(program.as_str()), embedder)
}

fn pending_bin_stub_executable(
    layout: &Layout,
    tool: &ToolName,
    comes_with: Option<&str>,
    embedder: &Embedder,
) -> io::Result<Option<crate::lay::Executable>> {
    if cfg!(windows) && !cmd_stub_name_safe(tool.as_str()) {
        return Ok(None);
    }
    if tool.is_alias() {
        return Ok(None);
    }
    pending_stub_at(layout, tool, comes_with, layout.shim(tool), embedder)
}

fn pending_stub_at(
    layout: &Layout,
    tool: &ToolName,
    comes_with: Option<&str>,
    shim: PathBuf,
    embedder: &Embedder,
) -> io::Result<Option<crate::lay::Executable>> {
    let standing = match crate::metadata_io::read_bounded_regular_utf8(&shim, 64 * 1024) {
        Ok(text) if text.lines().any(|l| is_marker_line(l, STUB_MARKER)) => Some(text),
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(_) if std::fs::symlink_metadata(&shim).is_ok() => return Ok(None),
        Err(_) => None,
    };
    let atpkg = embedder.embed(standing.as_deref().and_then(stub_named_atpkg).as_deref());
    let body = stub_content(tool, comes_with, atpkg.as_deref());
    if standing.as_deref().is_some_and(|have| have == body) {
        return Ok(None);
    }
    if let Some(dir) = shim.parent() {
        layout.ensure_dir(dir)?;
    }
    Ok(Some(crate::lay::Executable::new(shim, body)))
}

#[cfg(all(test, unix))]
fn stub_is_current(shim: &Path, body: &str) -> bool {
    crate::metadata_io::read_bounded_regular(shim, 64 * 1024)
        .is_ok_and(|have| have == body.as_bytes())
}

/// Remove `program`'s stub and its commands'.
pub fn remove_stub(layout: &Layout, program: &str) {
    let commands: Vec<&str> = commands_of(program).collect();
    for name in std::iter::once(program).chain(commands) {
        if let Some(tool) = ToolName::new(name) {
            let shim = layout.shim(&tool);
            if is_pending_stub(&shim) {
                let _ = std::fs::remove_file(&shim);
            }
        }
    }
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if stub_comes_with(&path).as_deref() == Some(program) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Remove every pending stub in `bin/` — `uninstall --all` / a recorded decline.
pub fn remove_all_stubs(layout: &Layout) {
    let Ok(entries) = std::fs::read_dir(layout.bin_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if is_pending_stub(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Lay the compiled roster stubs at adoption time before any network activity.
pub fn lay_adoption_stubs(layout: &Layout, cfg: &crate::config::PackagesConfig) {
    let installed = crate::ops::active_builds(layout);
    let Ok(removed) = layout.removed_programs() else {
        return;
    };
    let triple = crate::cli::current_triple();
    let embedder = Embedder::this_process();
    let mut files = Vec::new();
    let mut commands = BTreeSet::new();
    for name in compiled_names() {
        if installed.contains_key(name)
            || !crate::cli::vendor_wanted(cfg, &removed, name)
            || crate::vendor_direct::spec(name).is_some_and(|s| !s.serves(triple))
        {
            continue;
        }
        if let Some(tool) = ToolName::new(name) {
            if let Ok(Some(file)) = pending_bin_stub_executable(layout, &tool, None, &embedder) {
                files.push(file);
            }
            command_stub_executables(layout, name, &[], &embedder, &mut commands, &mut files);
            if is_agent_program(name)
                && let Ok(Some(twin)) =
                    pending_stub_at(layout, &tool, None, layout.agent_shim(&tool), &embedder)
            {
                files.push(twin);
            }
        }
    }
    if let Err(e) = files.iter().try_for_each(crate::lay::write_regenerable) {
        eprintln!("atpkg: warn — pending stubs not laid: {e}");
    }
}

/// The index-resolve reconcile.
pub fn reconcile(
    layout: &Layout,
    wanted: &BTreeSet<String>,
    installed: &BTreeMap<String, u64>,
    exposed: &BTreeMap<String, Vec<String>>,
) {
    let embedder = Embedder::this_process();
    let mut keep: BTreeSet<&str> = BTreeSet::new();
    let mut files = Vec::new();
    for name in wanted {
        if installed.contains_key(name.as_str()) {
            continue;
        }
        let Some(tool) = ToolName::new(name) else {
            continue;
        };
        match pending_bin_stub_executable(layout, &tool, None, &embedder) {
            Ok(Some(file)) => {
                files.push(file);
                keep.insert(name.as_str());
            }
            Ok(None) => {
                keep.insert(name.as_str());
            }
            Err(_) => {}
        }
        if is_agent_program(name)
            && let Ok(Some(twin)) =
                pending_stub_at(layout, &tool, None, layout.agent_shim(&tool), &embedder)
        {
            files.push(twin);
        }
        command_stub_executables(
            layout,
            name,
            exposed.get(name).map_or(&[], Vec::as_slice),
            &embedder,
            &mut keep,
            &mut files,
        );
    }
    if let Err(e) = files.iter().try_for_each(crate::lay::write_regenerable) {
        eprintln!("atpkg: warn — pending stubs not laid: {e}");
    }
    for dir in [layout.bin_dir(), layout.agents_dir()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let path = entry.path();
            let Some(text) = read_pending_stub(&path) else {
                continue;
            };
            let logical = ToolName::from_shim_file(name);
            let stays = logical.as_ref().is_some_and(|t| keep.contains(t.as_str()))
                || stub_comes_with_in(&text).is_some_and(|program| {
                    wanted.contains(&program)
                        && !installed.contains_key(&program)
                        && !exposed.contains_key(&program)
                });
            if !stays {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
#[path = "stub_tests.rs"]
mod tests;
