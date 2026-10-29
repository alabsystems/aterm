// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Local dev-link shims and per-program markers ([`crate::store::shim_allowed`], [`crate::pin`]).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// Maximum serialized size of one private dev-link marker.
pub const MAX_LINK_MARKER_BYTES: usize = 128 * 1024;
/// Maximum bin paths retained in one dev-link marker.
pub const MAX_LINK_MARKER_BINS: usize = 1024;
/// Maximum directory entries inspected and program names returned by one linked-program
/// enumeration.
pub const MAX_LINKED_PROGRAMS: usize = 256;

/// The dev-linked programs whose every tool shim resolves to an executable file inside the recorded
/// checkout.
pub(crate) fn live_dev_links(layout: &Layout) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let mut live = std::collections::BTreeSet::new();
    let mut problems = Vec::new();
    let programs = match linked_programs_checked(layout) {
        Ok(programs) => programs,
        Err(e) => return (live, vec![format!("cannot read development links: {e}")]),
    };
    for program in programs {
        if live_linked_checkout(layout, &program).is_ok_and(|root| root.is_some()) {
            live.insert(program);
        } else {
            problems.push(format!(
                "{program}: development link has a missing, non-executable, or redirected tool; \
                 rebuild its checkout and run `aterm pkg refresh {program}`"
            ));
        }
    }
    (live, problems)
}

/// The resolved checkout of a dev link whose recorded tools all still run executable files
/// inside it. `None` means no marker; it does not mean a recorded link is healthy.
///
/// # Errors
/// The program name is unsafe, the marker cannot be read, the checkout is missing, or a
/// recorded tool is missing, non-executable, or resolves outside the checkout.
pub fn live_linked_checkout(layout: &Layout, program: &str) -> std::io::Result<Option<PathBuf>> {
    if !safe_component(program) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsafe program name",
        ));
    }
    let marker = match read_marker_for_program(layout, program) {
        Ok(marker) => marker,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let root = fs::canonicalize(marker.checkout)?;
    let broken = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "development link has a missing, non-executable, or redirected tool",
        )
    };
    if marker.bins.is_empty() || !fs::metadata(&root)?.is_dir() {
        return Err(broken());
    }
    for rel in marker.bins {
        let tool = bin_tool_name(Path::new(&rel))
            .and_then(crate::store::ToolName::new)
            .ok_or_else(broken)?;
        let target = crate::platform::resolve_shim(&layout.shim(&tool)).ok_or_else(broken)?;
        let target = fs::canonicalize(target)?;
        let meta = fs::metadata(&target)?;
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt as _;
            meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = true;
        if !target.starts_with(&root) || !meta.is_file() || !executable {
            return Err(broken());
        }
    }
    Ok(Some(root))
}

/// Outcome of [`link`] or [`refresh`]: shimmed bin names and refused protected names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOutcome {
    /// The bin names shimmed into `bin/`.
    pub linked: Vec<String>,
    /// The bin names refused a link (on the [`crate::store::shim_allowed`] deny-list).
    pub refused: Vec<String>,
}

/// Why a link operation failed.
#[derive(Debug)]
pub enum LinkError {
    /// The program name is not a single safe path component.
    BadName(String),
    /// The checkout directory does not exist.
    NoCheckout(PathBuf),
    /// Nothing was linked.
    NoBins {
        missing: Vec<PathBuf>,
        refused: Vec<String>,
    },
    /// The program is not currently dev-linked.
    NotLinked(String),
    /// An underlying IO failure.
    Io(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::BadName(n) => write!(f, "{n:?} is not a safe program name"),
            LinkError::NoCheckout(p) => write!(f, "checkout {} is not a directory", p.display()),
            LinkError::NoBins { missing, refused } => {
                if missing.is_empty() && refused.is_empty() {
                    return write!(f, "nothing to link");
                }
                if !missing.is_empty() {
                    let paths: Vec<String> =
                        missing.iter().map(|p| p.display().to_string()).collect();
                    let verb = if missing.len() == 1 { "is" } else { "are" };
                    write!(f, "{} {verb} not there", paths.join(", "))?;
                    if !refused.is_empty() {
                        write!(f, "; ")?;
                    }
                }
                if !refused.is_empty() {
                    write!(
                        f,
                        "{} refused (protected command names)",
                        refused.join(", ")
                    )?;
                }
                Ok(())
            }
            LinkError::NotLinked(p) => write!(f, "{p} is not dev-linked"),
            LinkError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// The `0600` per-program link marker (toml; not a trust boundary).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LinkMarker {
    schema: u32,
    program: String,
    /// Absolute checkout root the linked bins live under.
    checkout: String,
    /// The relative bin paths (as given) linked from the checkout.
    #[serde(default)]
    bins: Vec<String>,
    #[serde(default)]
    linked_at: String,
}

fn safe_component(name: &str) -> bool {
    !(name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0'))
}

/// Whether `program` is currently dev-linked (its `0600` link marker exists).
#[must_use]
pub fn is_linked(layout: &Layout, program: &str) -> bool {
    safe_component(program) && read_marker_for_program(layout, program).is_ok()
}

fn bin_tool_name(rel: &Path) -> Option<&str> {
    let name = rel.file_name().and_then(|s| s.to_str())?;
    Some(strip_exe_suffix(name, std::env::consts::EXE_SUFFIX))
}

fn strip_exe_suffix<'a>(name: &'a str, ext: &str) -> &'a str {
    if ext.is_empty() {
        return name;
    }
    match name.len().checked_sub(ext.len()) {
        Some(cut) if cut > 0 && name.as_bytes()[cut..].eq_ignore_ascii_case(ext.as_bytes()) => {
            &name[..cut]
        }
        _ => name,
    }
}

enum PriorShim {
    /// Nothing stood there.
    Absent,
    /// A regular file (an exec stub, a tombstone, a Windows `.cmd`).
    File(Vec<u8>, fs::Permissions),
    /// A symlink, as an older atpkg laid shims.
    Symlink(PathBuf),
}

fn capture_shim(shim: &Path) -> Result<PriorShim, LinkError> {
    let meta = match fs::symlink_metadata(shim) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(PriorShim::Absent),
        Err(e) => return Err(LinkError::Io(format!("{}: {e}", shim.display()))),
    };
    if meta.is_symlink() {
        return fs::read_link(shim)
            .map(PriorShim::Symlink)
            .map_err(|e| LinkError::Io(format!("{}: {e}", shim.display())));
    }
    if !meta.is_file() {
        return Err(LinkError::Io(format!(
            "{} is neither a file nor a symlink; refusing to replace what cannot be put back",
            shim.display()
        )));
    }
    if meta.len() > crate::platform::MAX_SHIM_BYTES as u64 {
        return Err(LinkError::Io(format!(
            "{} is {} bytes; a shim is at most {}",
            shim.display(),
            meta.len(),
            crate::platform::MAX_SHIM_BYTES
        )));
    }
    fs::read(shim)
        .map(|bytes| PriorShim::File(bytes, meta.permissions()))
        .map_err(|e| LinkError::Io(format!("{}: {e}", shim.display())))
}

fn rollback_tmp(shim: &Path) -> std::io::Result<PathBuf> {
    let name = shim.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shim path has no file name",
        )
    })?;
    Ok(shim.with_file_name(format!(".{name}.atpkg-rollback-{}", std::process::id())))
}

fn restore_file(shim: &Path, bytes: &[u8], mode: &fs::Permissions) -> std::io::Result<()> {
    let tmp = rollback_tmp(shim)?;
    let _ = fs::remove_file(&tmp);
    let restored = fs::write(&tmp, bytes)
        .and_then(|()| fs::set_permissions(&tmp, mode.clone()))
        .and_then(|()| fs::rename(&tmp, shim));
    if restored.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    restored
}

/// Put a captured symlink shim back, temp + rename like [`restore_file`].
#[cfg(unix)]
fn restore_symlink(shim: &Path, target: &Path) -> std::io::Result<()> {
    let tmp = rollback_tmp(shim)?;
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    let restored = fs::rename(&tmp, shim);
    if restored.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    restored
}

/// Windows shims are `.cmd` files.
#[cfg(not(unix))]
fn restore_symlink(shim: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("cannot restore a symlink shim at {}", shim.display()),
    ))
}

fn roll_back(replaced: &[(PathBuf, PriorShim)]) {
    for (shim, prior) in replaced.iter().rev() {
        let _ = match prior {
            PriorShim::Absent => fs::remove_file(shim),
            PriorShim::File(bytes, mode) => restore_file(shim, bytes, mode),
            PriorShim::Symlink(target) => restore_symlink(shim, target),
        };
    }
}

/// Dev-link `program` ([`crate::store::shim_allowed`], [`unlink`]).
pub fn link(
    layout: &Layout,
    program: &str,
    checkout: &Path,
    bins: &[PathBuf],
) -> Result<LinkOutcome, LinkError> {
    let (marker, checkout) = requested_marker(program, checkout, bins)?;
    apply_link(
        layout,
        marker,
        bins.iter().map(|rel| link_bin(&checkout, rel)),
    )
}

fn requested_marker(
    program: &str,
    checkout: &Path,
    bins: &[PathBuf],
) -> Result<(LinkMarker, PathBuf), LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let checkout = std::path::absolute(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    if !checkout.is_dir() {
        return Err(LinkError::NoCheckout(checkout.clone()));
    }
    if bins.len() > MAX_LINK_MARKER_BINS {
        return Err(LinkError::Io(format!(
            "link marker has {} bins; limit is {MAX_LINK_MARKER_BINS}",
            bins.len()
        )));
    }
    let marker = LinkMarker {
        schema: 1,
        program: program.to_string(),
        checkout: checkout.display().to_string(),
        bins: bins.iter().map(|b| b.display().to_string()).collect(),
        linked_at: String::new(),
    };
    serialize_marker(&marker).map_err(|e| LinkError::Io(e.to_string()))?;
    Ok((marker, checkout))
}

struct ReadyBin {
    name: String,
    tool: crate::store::ToolName,
    src: PathBuf,
}

enum LinkBin {
    Ready(ReadyBin),
    Refused(String),
    Missing(PathBuf),
    Unnamed,
}

fn link_bin(checkout: &Path, rel: &Path) -> LinkBin {
    let Some(name) = bin_tool_name(rel) else {
        return LinkBin::Unnamed;
    };
    let Some(tool) = crate::store::ToolName::new(name) else {
        return LinkBin::Refused(name.to_string());
    };
    let src = checkout.join(rel);
    if !src.is_file() {
        return LinkBin::Missing(src);
    }
    LinkBin::Ready(ReadyBin {
        name: name.to_string(),
        tool,
        src,
    })
}

fn apply_link(
    layout: &Layout,
    marker: LinkMarker,
    bins: impl IntoIterator<Item = LinkBin>,
) -> Result<LinkOutcome, LinkError> {
    layout
        .ensure_dir(&layout.bin_dir())
        .map_err(|e| LinkError::Io(e.to_string()))?;
    ensure_private_dir(&layout.links_dir()).map_err(|e| LinkError::Io(e.to_string()))?;

    let mut linked = Vec::new();
    let mut refused = Vec::new();
    let mut missing = Vec::new();
    let mut replaced: Vec<(PathBuf, PriorShim)> = Vec::new();
    for bin in bins {
        let ReadyBin { name, tool, src } = match bin {
            LinkBin::Ready(bin) => bin,
            LinkBin::Refused(name) => {
                refused.push(name);
                continue;
            }
            LinkBin::Missing(src) => {
                missing.push(src);
                continue;
            }
            LinkBin::Unnamed => continue,
        };
        let shim = layout.shim(&tool);
        let prior = match capture_shim(&shim) {
            Ok(prior) => prior,
            Err(e) => {
                roll_back(&replaced);
                return Err(e);
            }
        };
        if let Err(e) = crate::platform::install_shim_to(&shim, &src) {
            roll_back(&replaced);
            return Err(LinkError::Io(e.to_string()));
        }
        replaced.push((shim, prior));
        linked.push(name);
    }
    if linked.is_empty() {
        roll_back(&replaced);
        return Err(LinkError::NoBins { missing, refused });
    }

    if let Err(e) = write_marker(&layout.link_marker(&marker.program), &marker) {
        roll_back(&replaced);
        return Err(LinkError::Io(e.to_string()));
    }
    crate::activate::reconcile_agents(layout);
    Ok(LinkOutcome { linked, refused })
}

/// An admitted conditional selection keeps its store's kernel writer lock borrowed until
/// application.
pub(crate) struct ConditionalLink<'a> {
    guard: &'a mut crate::lock::StoreLock,
    marker: LinkMarker,
    bins: Vec<ReadyBin>,
}

impl ConditionalLink<'_> {
    pub(crate) const fn guard(&mut self) -> &mut crate::lock::StoreLock {
        self.guard
    }

    pub(crate) fn apply(self) -> Result<LinkOutcome, LinkError> {
        apply_link(
            &self.guard.layout(),
            self.marker,
            self.bins.into_iter().map(LinkBin::Ready),
        )
    }
}

/// Admits the candidate and observed selection under the store writer lock.
pub(crate) fn link_if_checkout<'a>(
    guard: &'a mut crate::lock::StoreLock,
    configured: &Layout,
    program: &str,
    checkout: &Path,
    bins: &[PathBuf],
    expected: &Path,
    expected_prefix: &Path,
) -> Result<ConditionalLink<'a>, LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let layout = guard.layout();
    check_link_prefix(&layout, expected_prefix).map_err(LinkError::Io)?;
    check_link_prefix(configured, expected_prefix).map_err(LinkError::Io)?;
    let links = fs::symlink_metadata(layout.links_dir())
        .map_err(|e| LinkError::Io(format!("current development links are unavailable: {e}")))?;
    if !links.is_dir() || crate::platform::is_reparse(&links) {
        return Err(LinkError::Io(
            "development links are not a regular directory".into(),
        ));
    }
    let marker = read_marker_for_program(&layout, program)
        .map_err(|e| LinkError::Io(format!("current development link is unavailable: {e}")))?;
    let expected_bins: Vec<String> = bins.iter().map(|b| b.display().to_string()).collect();
    if marker.schema != 1
        || !Path::new(&marker.checkout).is_absolute()
        || !expected.is_absolute()
        || expected.as_os_str() != std::ffi::OsStr::new(&marker.checkout)
        || marker.bins != expected_bins
    {
        return Err(LinkError::Io(
            "development selection changed; leaving it in place".into(),
        ));
    }
    let (marker, checkout) = requested_marker(program, checkout, bins)?;
    let mut ready = Vec::with_capacity(bins.len());
    for rel in bins {
        if rel.as_os_str().is_empty()
            || rel
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(LinkError::Io(format!(
                "conditional bin is not checkout-relative: {}",
                rel.display()
            )));
        }
        match link_bin(&checkout, rel) {
            LinkBin::Ready(bin) => ready.push(bin),
            LinkBin::Missing(src) => {
                return Err(LinkError::NoBins {
                    missing: vec![src],
                    refused: Vec::new(),
                });
            }
            LinkBin::Refused(name) => {
                return Err(LinkError::NoBins {
                    missing: Vec::new(),
                    refused: vec![name],
                });
            }
            LinkBin::Unnamed => {
                return Err(LinkError::Io(format!(
                    "conditional bin has no tool name: {}",
                    rel.display()
                )));
            }
        }
    }
    if ready.is_empty() {
        return Err(LinkError::NoBins {
            missing: Vec::new(),
            refused: Vec::new(),
        });
    }
    Ok(ConditionalLink {
        guard,
        marker,
        bins: ready,
    })
}

/// A guarded producer must address the same physical store it inspected.
pub(crate) fn check_link_prefix(layout: &Layout, expected: &Path) -> Result<(), String> {
    let actual = fs::canonicalize(&layout.prefix).map_err(|e| {
        format!(
            "cannot inspect package prefix {}: {e}",
            layout.prefix.display()
        )
    })?;
    let expected = fs::canonicalize(expected).map_err(|e| {
        format!(
            "cannot inspect expected package prefix {}: {e}",
            expected.display()
        )
    })?;
    if actual != expected {
        return Err("package prefix changed; leaving development selection in place".into());
    }
    Ok(())
}

/// Un-link `program`.
pub fn unlink(layout: &Layout, program: &str) -> Result<(), LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let marker_path = layout.link_marker(program);
    let marker = read_marker_for_program(layout, program).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LinkError::NotLinked(program.to_string())
        } else {
            LinkError::Io(format!("could not read link marker for {program}: {error}"))
        }
    })?;
    let checkout = PathBuf::from(&marker.checkout);
    for rel in &marker.bins {
        let Some(name) = bin_tool_name(Path::new(rel)) else {
            continue;
        };
        let Some(tool) = crate::store::ToolName::new(name) else {
            continue;
        };
        let link = layout.shim(&tool);
        if let Some(target) = crate::platform::resolve_shim(&link)
            && target.starts_with(&checkout)
        {
            let _ = fs::remove_file(&link);
        }
    }
    let _ = fs::remove_file(&marker_path);
    crate::activate::reconcile_agents(layout);
    Ok(())
}

/// The recorded checkout root of a dev-linked `program`.
#[must_use]
pub fn linked_checkout(layout: &Layout, program: &str) -> Option<PathBuf> {
    linked_checkout_checked(layout, program).ok().flatten()
}

/// Checked counterpart of [`linked_checkout`].
pub fn linked_checkout_checked(layout: &Layout, program: &str) -> std::io::Result<Option<PathBuf>> {
    if !safe_component(program) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{program:?} is not a safe program name"),
        ));
    }
    match read_marker_for_program(layout, program) {
        Ok(marker) => Ok(Some(PathBuf::from(marker.checkout))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The tool names a dev-linked `program` puts on PATH, from its marker's bins.
#[must_use]
pub fn linked_bins(layout: &Layout, program: &str) -> Vec<String> {
    if !safe_component(program) {
        return Vec::new();
    }
    let Ok(marker) = read_marker_for_program(layout, program) else {
        return Vec::new();
    };
    marker
        .bins
        .iter()
        .filter_map(|rel| bin_tool_name(Path::new(rel)).map(str::to_string))
        .collect()
}

/// Re-assert a program's dev links (idempotent) ([`link`]).
pub fn refresh(layout: &Layout, program: &str) -> Result<LinkOutcome, LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let marker = read_marker_for_program(layout, program).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            LinkError::NotLinked(program.to_string())
        } else {
            LinkError::Io(format!("could not read link marker for {program}: {error}"))
        }
    })?;
    let checkout = PathBuf::from(&marker.checkout);
    let bins: Vec<PathBuf> = marker.bins.iter().map(PathBuf::from).collect();
    link(layout, program, &checkout, &bins)
}

/// The names of all dev-linked programs (the safe-component file names under `links/`).
#[must_use]
pub fn linked_programs(layout: &Layout) -> Vec<String> {
    linked_programs_checked(layout).unwrap_or_default()
}

#[cfg(unix)]
fn directory_identity(_path: &Path, metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn directory_identity(path: &Path, _metadata: &fs::Metadata) -> Option<(u32, u64)> {
    crate::platform::directory_identity(path)
}

#[cfg(not(any(unix, windows)))]
fn directory_identity(
    _path: &Path,
    metadata: &fs::Metadata,
) -> Option<(u64, Option<std::time::SystemTime>)> {
    Some((metadata.len(), metadata.modified().ok()))
}

/// Enumerate linked program names with a strict entry/result cap and explicit diagnostics for a
/// link-like, replaced, or non-directory `links/` path.
pub fn linked_programs_checked(layout: &Layout) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let directory = layout.links_dir();
    let before = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error),
    };
    if !before.file_type().is_dir() || crate::platform::is_reparse(&before) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "package links path is not a real directory",
        ));
    }
    let Some(identity) = directory_identity(&directory, &before) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "could not determine the identity of the package links directory",
        ));
    };
    let entries = fs::read_dir(&directory)?;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_LINKED_PROGRAMS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("package links directory exceeds the {MAX_LINKED_PROGRAMS}-entry limit"),
            ));
        }
        let e = entry?;
        if let Some(n) = e.file_name().to_str()
            && safe_component(n)
        {
            out.push(n.to_string());
        }
    }
    let after = fs::symlink_metadata(&directory)?;
    if !after.file_type().is_dir()
        || crate::platform::is_reparse(&after)
        || directory_identity(&directory, &after) != Some(identity)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "package links directory changed while it was enumerated",
        ));
    }
    out.sort();
    Ok(out)
}

/// The tool names a program's dev link owns, or `None` when it is not linked.
pub fn linked_tool_names(layout: &Layout, program: &str) -> Option<Vec<String>> {
    let marker = read_marker_for_program(layout, program).ok()?;
    let names: Vec<String> = marker
        .bins
        .iter()
        .filter_map(|rel| bin_tool_name(Path::new(rel)).map(str::to_string))
        .collect();
    if names.is_empty() { None } else { Some(names) }
}

fn read_marker_for_program(layout: &Layout, program: &str) -> std::io::Result<LinkMarker> {
    let marker = read_marker(&layout.link_marker(program))?;
    if marker.program != program {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "link marker program {:?} does not match filename {program:?}",
                marker.program
            ),
        ));
    }
    Ok(marker)
}

fn read_marker(path: &Path) -> std::io::Result<LinkMarker> {
    let text = crate::metadata_io::read_bounded_regular_utf8(path, MAX_LINK_MARKER_BYTES)?;
    let marker: LinkMarker = aterm_toml::from_str(&text).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("link marker is invalid: {error}"),
        )
    })?;
    if !safe_component(&marker.program) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "link marker contains an unsafe program name",
        ));
    }
    if marker.bins.len() > MAX_LINK_MARKER_BINS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "link marker has {} bins; limit is {MAX_LINK_MARKER_BINS}",
                marker.bins.len()
            ),
        ));
    }
    Ok(marker)
}

fn serialize_marker(marker: &LinkMarker) -> std::io::Result<String> {
    if marker.bins.len() > MAX_LINK_MARKER_BINS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "link marker has {} bins; limit is {MAX_LINK_MARKER_BINS}",
                marker.bins.len()
            ),
        ));
    }
    let text = aterm_toml::to_string(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if text.len() > MAX_LINK_MARKER_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("link marker exceeds the {MAX_LINK_MARKER_BYTES}-byte limit"),
        ));
    }
    Ok(text)
}

/// Write the marker `0600` via temp + rename.
fn write_marker(dest: &Path, marker: &LinkMarker) -> std::io::Result<()> {
    let text = serialize_marker(marker)?;
    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "link marker has no parent",
        )
    })?;
    let tmp = parent.join(format!(".link.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let staged = crate::platform::create_private_new(&tmp)
        .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
        .and_then(|()| crate::platform::harden_file(&tmp));
    if let Err(e) = staged {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, dest).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ay`'s shim as this platform names it (`ay.cmd` on Windows).
    fn ay_shim(layout: &Layout) -> PathBuf {
        layout.shim(&crate::store::ToolName::new("ay").expect("a tool name"))
    }

    #[test]
    fn guarded_link_replaces_only_the_still_selected_checkout() {
        let root = crate::testdir::dir("atpkg-guarded-link-");
        let layout = Layout {
            prefix: root.join("pkg"),
        };
        let old = checkout("guard-old", &["ay"]);
        let new = checkout("guard-new", &["ay"]);
        let manual = checkout("guard-manual", &["ay"]);
        let bins = vec![PathBuf::from("target/release/ay")];
        let mut guard = crate::lock::try_lock_store(&layout).unwrap();
        link(&layout, "ay", &old, &bins).unwrap();
        let alias = PathBuf::from(format!("{}/.", old.display()));
        let before = fs::read(layout.link_marker("ay")).unwrap();
        assert!(
            link_if_checkout(
                &mut guard,
                &layout,
                "ay",
                &new,
                &bins,
                &alias,
                &layout.prefix
            )
            .is_err()
        );
        assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), before);
        let mut marker = read_marker_for_program(&layout, "ay").unwrap();
        marker.checkout = alias.display().to_string();
        write_marker(&layout.link_marker("ay"), &marker).unwrap();
        assert!(
            link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &old, &layout.prefix).is_err()
        );
        link_if_checkout(
            &mut guard,
            &layout,
            "ay",
            &new,
            &bins,
            &alias,
            &layout.prefix,
        )
        .unwrap()
        .apply()
        .unwrap();
        assert_eq!(linked_checkout(&layout, "ay"), Some(new.to_path_buf()));
        link(&layout, "ay", &manual, &bins).unwrap();
        let marker = fs::read(layout.link_marker("ay")).unwrap();
        let shim = fs::read(ay_shim(&layout)).unwrap();
        assert!(
            link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &old, &layout.prefix).is_err()
        );
        assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), marker);
        assert_eq!(fs::read(ay_shim(&layout)).unwrap(), shim);
        let valid = read_marker_for_program(&layout, "ay").unwrap();
        for kind in ["schema", "program", "relative", "malformed"] {
            let mut invalid = valid.clone();
            match kind {
                "schema" => invalid.schema = 2,
                "program" => invalid.program = "other".into(),
                "relative" => invalid.checkout = "relative".into(),
                _ => {}
            }
            let bytes = if kind == "malformed" {
                "not a marker".into()
            } else {
                serialize_marker(&invalid).unwrap()
            };
            fs::write(layout.link_marker("ay"), bytes.as_bytes()).unwrap();
            assert!(
                link_if_checkout(
                    &mut guard,
                    &layout,
                    "ay",
                    &new,
                    &bins,
                    &manual,
                    &layout.prefix
                )
                .is_err(),
                "{kind}"
            );
            assert_eq!(
                fs::read(layout.link_marker("ay")).unwrap(),
                bytes.as_bytes()
            );
            assert_eq!(fs::read(ay_shim(&layout)).unwrap(), shim);
        }
    }

    #[test]
    fn guarded_link_requires_its_store_lock_and_original_bin_mapping() {
        let root = crate::testdir::dir("atpkg-guarded-owner-");
        let layout = Layout {
            prefix: root.join("pkg"),
        };
        let old = checkout("guard-mapping", &["ay", "aux"]);
        let new = checkout("guard-mapping-new", &["ay"]);
        let bins = vec![PathBuf::from("target/release/ay")];
        let both = vec![
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/aux"),
        ];
        link(&layout, "ay", &old, &both).unwrap();
        let marker = fs::read(layout.link_marker("ay")).unwrap();
        let mut guard = crate::lock::try_lock_store(&layout).unwrap();
        assert_eq!(guard.layout().prefix, layout.prefix);
        assert!(
            link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &old, &layout.prefix).is_err()
        );
        let shim = fs::read(ay_shim(&layout)).unwrap();
        assert!(
            link_if_checkout(&mut guard, &layout, "ay", &new, &both, &old, &layout.prefix).is_err()
        );
        assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), marker);
        assert_eq!(fs::read(ay_shim(&layout)).unwrap(), shim);
        fs::write(new.join("target/release/aux"), b"fixture\n").unwrap();
        let outcome =
            link_if_checkout(&mut guard, &layout, "ay", &new, &both, &old, &layout.prefix)
                .unwrap()
                .apply()
                .unwrap();
        assert_eq!(outcome.linked, ["ay", "aux"]);
        assert_eq!(linked_checkout(&layout, "ay"), Some(new.to_path_buf()));
        unlink(&layout, "ay").unwrap();
        assert!(
            link_if_checkout(&mut guard, &layout, "ay", &new, &bins, &new, &layout.prefix).is_err()
        );
        assert!(!layout.link_marker("ay").exists());
        assert!(!ay_shim(&layout).exists());
    }

    #[test]
    fn guarded_link_prefix_refuses_a_different_store() {
        let root = crate::testdir::dir("atpkg-guarded-prefix-");
        let layout = Layout {
            prefix: root.join("a"),
        };
        let other = Layout {
            prefix: root.join("b"),
        };
        let mut guard = crate::lock::try_lock_store(&layout).unwrap();
        let mut other_guard = crate::lock::try_lock_store(&other).unwrap();
        assert!(check_link_prefix(&guard.layout(), &layout.prefix).is_ok());
        assert!(check_link_prefix(&guard.layout(), &other.prefix).is_err());
        let old = checkout("guard-prefix-old", &["ay"]);
        let new = checkout("guard-prefix-new", &["ay"]);
        let bins = [PathBuf::from("target/release/ay")];
        link(&layout, "ay", &old, &bins).unwrap();
        let marker = fs::read(layout.link_marker("ay")).unwrap();
        assert!(
            link_if_checkout(
                &mut other_guard,
                &layout,
                "ay",
                &new,
                &bins,
                &old,
                &layout.prefix
            )
            .is_err()
        );
        assert!(
            link_if_checkout(&mut guard, &other, "ay", &new, &bins, &old, &layout.prefix).is_err()
        );
        assert_eq!(fs::read(layout.link_marker("ay")).unwrap(), marker);
    }
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> crate::testdir::ScratchLayout {
        crate::testdir::layout(&format!("atpkg-link-{label}-"))
    }

    /// `bin/<name>` for a name the test knows is admissible.
    fn shim_of(layout: &Layout, name: &str) -> PathBuf {
        layout.shim(&crate::store::ToolName::new(name).unwrap())
    }

    /// A fake checkout with `target/release/<bins>`.
    fn checkout(label: &str, bins: &[&str]) -> crate::testdir::Dir {
        let d = crate::testdir::dir(&format!("atpkg-checkout-{label}-"));
        fs::create_dir_all(d.join("target/release")).unwrap();
        for b in bins {
            fs::write(d.join("target/release").join(b), b"#!/bin/true\n").unwrap();
        }
        d
    }

    #[test]
    fn the_exe_suffix_is_stripped_without_ever_slicing_mid_character() {
        assert_eq!(strip_exe_suffix("日本語", ".exe"), "日本語");
        assert_eq!(strip_exe_suffix("日本語.exe", ".exe"), "日本語");
        assert_eq!(strip_exe_suffix("日本語.EXE", ".exe"), "日本語");
        assert_eq!(strip_exe_suffix("é", ".exe"), "é");
        assert_eq!(strip_exe_suffix("éxe", ".exe"), "éxe");
        assert_eq!(strip_exe_suffix("é.exe", ".exe"), "é");
        assert_eq!(strip_exe_suffix("ay.exe", ".exe"), "ay");
        assert_eq!(strip_exe_suffix("ay.ExE", ".exe"), "ay");
        assert_eq!(strip_exe_suffix("ay", ".exe"), "ay");
        assert_eq!(strip_exe_suffix("ay", ""), "ay");
        assert_eq!(strip_exe_suffix("ay.exe", ""), "ay.exe");
        assert_eq!(strip_exe_suffix(".exe", ".exe"), ".exe");
        assert_eq!(bin_tool_name(Path::new("target/release/ay")), Some("ay"));
    }

    #[test]
    fn nothing_to_link_names_the_unbuilt_path_and_the_refused_name() {
        let l = layout("nobins");
        let co = checkout("nobins", &[]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/sudo"),
        ];
        let err = link(&l, "ay", &co, &bins).unwrap_err();
        let want = co.join("target/release/ay");
        assert!(
            matches!(&err, LinkError::NoBins { missing, refused }
                if *missing == [want.clone()] && *refused == ["sudo".to_string()]),
            "{err:?}"
        );
        assert!(!shim_of(&l, "ay").exists());
    }
    #[cfg(unix)]
    #[test]
    fn a_link_whose_marker_cannot_be_written_rolls_back_and_leaves_no_temp() {
        let l = layout("markerfail");
        let co = checkout("markerfail", &["ay"]);
        let bins = [PathBuf::from("target/release/ay")];
        fs::create_dir_all(l.links_dir().join("ay").join("occupied")).unwrap();

        let out = link(&l, "ay", &co, &bins);
        assert!(
            out.is_err(),
            "a marker that cannot be written must fail the link"
        );
        assert!(
            !shim_of(&l, "ay").exists(),
            "the shim this call created must be rolled back — an UNTRACKED dev shim is \
             worse than no link at all"
        );
        let litter: Vec<String> = fs::read_dir(l.links_dir())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| n.starts_with(".link.tmp-"))
            .collect();
        assert!(
            litter.is_empty(),
            "no marker temp may survive a failed write, found {litter:?}"
        );
        assert!(
            !linked_programs(&l)
                .iter()
                .any(|p| p.starts_with(".link.tmp-")),
            "a leaked temp must never be reported as a dev-linked program"
        );

        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_link_restores_the_shim_it_overwrote() {
        let l = layout("overwrite");
        let co = checkout("overwrite", &["ay"]);
        let bins = [PathBuf::from("target/release/ay")];
        fs::create_dir_all(l.bin_dir()).unwrap();
        let store_bin = l.prefix.join("store-build").join("ay");
        fs::create_dir_all(store_bin.parent().unwrap()).unwrap();
        fs::write(&store_bin, b"#!/bin/true\n").unwrap();
        let shim = shim_of(&l, "ay");
        crate::platform::install_shim_to(&shim, &store_bin).unwrap();
        let before = fs::read(&shim).unwrap();
        let mode_before = fs::metadata(&shim).unwrap().permissions().mode() & 0o777;

        fs::create_dir_all(l.links_dir().join("ay").join("occupied")).unwrap();

        let out = link(&l, "ay", &co, &bins);
        assert!(
            out.is_err(),
            "a marker that cannot be written must fail the link"
        );
        assert_eq!(
            fs::read(&shim).unwrap(),
            before,
            "the OVERWRITTEN store shim must be restored byte for byte, never left \
             pointing into the dev checkout"
        );
        assert_eq!(
            fs::metadata(&shim).unwrap().permissions().mode() & 0o777,
            mode_before,
            "the restored shim keeps its mode"
        );
        assert_eq!(
            crate::platform::resolve_shim(&shim).as_deref(),
            Some(store_bin.as_path()),
            "the restored shim forwards to the STORE build, never to the checkout"
        );
        assert!(
            !is_linked(&l, "ay"),
            "no marker, so no dev link is recorded"
        );
        let litter: Vec<String> = fs::read_dir(l.bin_dir())
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().to_str().map(str::to_owned))
            .filter(|n| n.contains(".atpkg-rollback-"))
            .collect();
        assert!(
            litter.is_empty(),
            "the rollback must leave no temp behind, found {litter:?}"
        );

        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_and_unlink_round_trip() {
        let l = layout("rt");
        let co = checkout("rt", &["ay", "ny"]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/ny"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string(), "ny".to_string()]);
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "ay")).unwrap(),
            co.join("target/release/ay")
        );
        assert!(is_linked(&l, "ay"));
        #[cfg(unix)]
        {
            let mode = fs::metadata(l.link_marker("ay"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        unlink(&l, "ay").unwrap();
        assert!(!is_linked(&l, "ay"));
        assert!(
            fs::symlink_metadata(shim_of(&l, "ay")).is_err(),
            "shim removed"
        );
        assert!(
            fs::symlink_metadata(shim_of(&l, "ny")).is_err(),
            "shim removed"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_and_unlink_sweep_the_front_of_path_agents_twin() {
        let l = layout("agents-twin");
        let co = checkout("agents-twin", &["claude"]);
        let claude = crate::store::ToolName::new("claude").unwrap();
        let store_bin = l.build_dir("claude", 2_026_091_001).join("bin");
        fs::create_dir_all(&store_bin).unwrap();
        let target = store_bin.join(claude.exe_file());
        fs::write(&target, b"#!/bin/true\n").unwrap();
        let lay_twin = || {
            l.ensure_dir(&l.agents_dir()).unwrap();
            crate::platform::install_shim_to(&l.agent_shim(&claude), &target).unwrap();
        };
        l.ensure_dir(&l.bin_dir()).unwrap();
        crate::platform::install_shim_to(&shim_of(&l, "claude"), &target).unwrap();
        lay_twin();

        let bins = [PathBuf::from("target/release/claude")];
        link(&l, "claude", &co, &bins).unwrap();
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "claude")).unwrap(),
            co.join("target/release/claude"),
            "fixture: bin/claude is the dev link"
        );
        assert_eq!(
            crate::platform::resolve_shim(&l.agent_shim(&claude)),
            Some(co.join("target/release/claude")),
            "the twin at the front of PATH runs the dev link"
        );
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "claude")),
            Some(co.join("target/release/claude")),
            "bin/ stays the link as laid"
        );
        #[cfg(unix)]
        assert!(
            !fs::read_to_string(l.agent_shim(&claude))
                .unwrap()
                .contains("__selfupdate"),
            "a dev link's twin hands no verb to atpkg's self-update"
        );
        crate::activate::reconcile_agents(&l);
        crate::activate::sweep_agents_dir(&l);
        assert!(fs::symlink_metadata(l.agent_shim(&claude)).is_ok());
        let stray = checkout("agents-twin-stray", &["claude"]);
        let stray_bin = stray.join("target/release/claude");
        crate::platform::install_shim_to(&shim_of(&l, "claude"), &stray_bin).unwrap();
        crate::platform::install_shim_to(&l.agent_shim(&claude), &stray_bin).unwrap();
        crate::activate::sweep_agents_dir(&l);
        assert!(
            fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
            "a twin onto an unrecorded checkout is swept"
        );
        crate::activate::reconcile_agents(&l);
        assert!(
            fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
            "and never laid"
        );
        crate::platform::install_shim_to(&shim_of(&l, "claude"), &co.join("target/release/claude"))
            .unwrap();
        let _ = fs::remove_dir_all(&stray);

        lay_twin();
        unlink(&l, "claude").unwrap();
        assert!(
            fs::symlink_metadata(shim_of(&l, "claude")).is_err(),
            "fixture: the dev shim is gone"
        );
        assert!(
            fs::symlink_metadata(l.agent_shim(&claude)).is_err(),
            "agents/ does not outlive the bin/ shim that vouched for it"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn sensitive_bin_name_is_refused_a_link() {
        let l = layout("sensitive");
        let co = checkout("sensitive", &["ay", "ssh"]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/ssh"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string()]);
        assert_eq!(out.refused, vec!["ssh".to_string()]);
        assert!(crate::store::ToolName::new("ssh").is_none());
        assert!(
            fs::symlink_metadata(
                l.bin_dir()
                    .join(format!("ssh{}", crate::platform::SHIM_SUFFIX))
            )
            .is_err(),
            "sensitive name never shimmed"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn unlink_only_removes_links_into_its_checkout() {
        let l = layout("scoped");
        let co = checkout("scoped", &["ay"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        let store_target = l.build_dir("ay", 18).join("bin/ay");
        fs::create_dir_all(store_target.parent().unwrap()).unwrap();
        fs::write(&store_target, b"#!/bin/true\n").unwrap();
        crate::platform::install_shim_to(&shim_of(&l, "ay"), &store_target).unwrap();
        unlink(&l, "ay").unwrap();
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "ay")).unwrap(),
            store_target
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_preserves_a_pre_existing_pin() {
        let l = layout("pin");
        crate::pin::set_pinned(&l, "ay", true).unwrap();
        let before = fs::read(l.prefix.join("pins")).unwrap();
        let co = checkout("pin", &["ay"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        unlink(&l, "ay").unwrap();
        let after = fs::read(l.prefix.join("pins")).unwrap();
        assert_eq!(
            before, after,
            "pin file is byte-identical across a link→unlink cycle"
        );
        assert!(crate::pin::is_pinned(&l, "ay"));
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn refresh_relinks_newly_built_bins() {
        let l = layout("refresh");
        let co = checkout("refresh", &["ay"]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/ny"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string()], "ny not built yet");
        fs::write(co.join("target/release/ny"), b"#!/bin/true\n").unwrap();
        let out2 = refresh(&l, "ay").unwrap();
        assert!(
            out2.linked.contains(&"ny".to_string()),
            "refresh picks up the new bin"
        );
        assert_eq!(
            crate::platform::resolve_shim(&shim_of(&l, "ny")).unwrap(),
            co.join("target/release/ny")
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn link_marker_bin_count_is_rejected_before_shim_mutation() {
        let l = layout("bin-cap");
        let co = checkout("bin-cap", &["ay"]);
        let bins = vec![PathBuf::from("target/release/ay"); MAX_LINK_MARKER_BINS + 1];
        assert!(link(&l, "ay", &co, &bins).is_err());
        assert!(
            crate::platform::resolve_shim(&shim_of(&l, "ay")).is_none(),
            "an unrecordable request must not mutate shims"
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn linked_program_enumeration_accepts_exact_cap_and_rejects_one_more() {
        let l = layout("entry-cap");
        fs::create_dir_all(l.links_dir()).unwrap();
        for index in 0..MAX_LINKED_PROGRAMS {
            fs::write(l.links_dir().join(format!("p{index:03}")), b"x").unwrap();
        }
        assert_eq!(
            linked_programs_checked(&l).unwrap().len(),
            MAX_LINKED_PROGRAMS
        );
        fs::write(l.links_dir().join("overflow"), b"x").unwrap();
        let error = linked_programs_checked(&l).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            linked_programs(&l).is_empty(),
            "compatibility wrapper fails closed instead of returning a partial set"
        );
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_and_retargetable_link_markers_are_explicit_errors() {
        use std::os::unix::ffi::OsStrExt as _;

        let l = layout("marker-special");
        fs::create_dir_all(l.links_dir()).unwrap();
        let marker = l.link_marker("ay");
        let marker_c = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
        // SAFETY: `marker_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(marker_c.as_ptr(), 0o600) }, 0);
        let fifo_error = linked_checkout_checked(&l, "ay").unwrap_err();
        assert_eq!(fifo_error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!is_linked(&l, "ay"));
        fs::remove_file(&marker).unwrap();

        let first = l.prefix.join("first.toml");
        let second = l.prefix.join("second.toml");
        let marker_text = |checkout: &str| {
            aterm_toml::to_string(&LinkMarker {
                schema: 1,
                program: "ay".to_string(),
                checkout: checkout.to_string(),
                bins: Vec::new(),
                linked_at: String::new(),
            })
            .unwrap()
        };
        fs::write(&first, marker_text("/first")).unwrap();
        fs::write(&second, marker_text("/second")).unwrap();
        std::os::unix::fs::symlink(&first, &marker).unwrap();
        assert!(linked_checkout_checked(&l, "ay").is_err());
        fs::remove_file(&marker).unwrap();
        std::os::unix::fs::symlink(&second, &marker).unwrap();
        assert!(
            linked_checkout_checked(&l, "ay").is_err(),
            "retargeting the link never makes a link-like marker admissible"
        );
        let _ = fs::remove_dir_all(&l.prefix);
    }

    #[cfg(unix)]
    #[test]
    fn link_directory_symlink_is_rejected_instead_of_followed() {
        let l = layout("directory-link");
        let foreign = layout("directory-link-foreign");
        fs::create_dir_all(foreign.links_dir()).unwrap();
        fs::write(foreign.links_dir().join("ay"), b"foreign").unwrap();
        std::os::unix::fs::symlink(foreign.links_dir(), l.links_dir()).unwrap();
        let error = linked_programs_checked(&l).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&foreign.prefix);
    }
}
