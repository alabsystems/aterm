// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Registry-free dev loop (§13): symlink curated bins from a sibling checkout into `bin/`
//! under a `0600` per-program marker; `update`/`apply` HARD-SKIP a linked program until
//! `atpkg unlink`.
//!
//! Every linked bin still passes [`crate::store::shim_allowed`] — a dev link can no more
//! shadow `sudo`/`git`/`rustc` than an installed shim can. The link marker is a DISTINCT
//! file from the pin-state file ([`crate::pin`]), so a pin survives a link→unlink cycle
//! untouched (linkmode never reads/writes/clears any pin-state). The marker is a local dev
//! artifact with NO signature — it is not a trust boundary; it can only SUPPRESS registry
//! management of a dev-linked program, never advance a program onto a build, so it cannot
//! bypass a Tombstone or the floor.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::platform::ensure_private_dir;
use crate::store::Layout;

/// What a [`link`]/[`refresh`] did: the bins symlinked, and any refused (sensitive-name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkOutcome {
    /// The bin names symlinked into `bin/`.
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
    /// No linkable bin was found (all missing or refused).
    NoBins,
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
            LinkError::NoBins => write!(f, "no linkable bin found (all missing or refused)"),
            LinkError::NotLinked(p) => write!(f, "{p} is not dev-linked"),
            LinkError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for LinkError {}

/// The `0600` per-program link marker (toml; NOT a trust boundary).
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

/// Reject a program name that is not a single safe path component (same shape rule
/// [`crate::store::shim_allowed`] / [`crate::ops::uninstall`] use).
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
    safe_component(program) && layout.link_marker(program).exists()
}

/// Dev-link `program`: symlink each of `bins` (relative paths under `checkout`) into `bin/`,
/// every name gated through [`crate::store::shim_allowed`], and record a `0600` marker so
/// `update`/`apply` HARD-SKIP the program until [`unlink`]. NEVER touches any pin-state.
/// The shim/tool name for a bin path: its file name with the platform executable extension
/// removed (`foo.exe` → `foo` on Windows, case-insensitively; unchanged on Unix, where
/// `EXE_SUFFIX` is empty). Deriving the name this way is what makes a dev-linked tool
/// invokable by its bare name (Windows resolves `foo` → `foo.cmd` via `PATHEXT`, never
/// `foo.exe.cmd`) AND lets the sensitive-name deny-list see the real name (`ssh.exe` is
/// refused as `ssh`). Returns `None` if the path has no usable UTF-8 file name. `link` and
/// `unlink` MUST derive names identically so an unlink removes the exact shim a link created.
fn bin_tool_name(rel: &Path) -> Option<&str> {
    let name = rel.file_name().and_then(|s| s.to_str())?;
    let ext = std::env::consts::EXE_SUFFIX; // ".exe" on Windows, "" on Unix
    match name.len().checked_sub(ext.len()) {
        Some(cut) if !ext.is_empty() && cut > 0 && name[cut..].eq_ignore_ascii_case(ext) => {
            Some(&name[..cut])
        }
        _ => Some(name),
    }
}

pub fn link(
    layout: &Layout,
    program: &str,
    checkout: &Path,
    bins: &[PathBuf],
) -> Result<LinkOutcome, LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    // Absolutize the checkout so the shim embeds an ABSOLUTE target. A relative target is
    // resolved at INVOCATION time, not link time: a Windows `.cmd` resolves it against the
    // caller's CWD (tool not found from elsewhere, or worse runs a same-named binary in the
    // caller's CWD), and a Unix relative symlink resolves against `bin/`, not the checkout.
    // `path::absolute` is lexical (no filesystem hit, no `\\?\` verbatim prefix), so the
    // embedded path stays clean; the marker then also records an absolute checkout so
    // `refresh` works from any directory.
    let checkout = std::path::absolute(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    if !checkout.is_dir() {
        return Err(LinkError::NoCheckout(checkout.clone()));
    }
    ensure_private_dir(&layout.bin_dir()).map_err(|e| LinkError::Io(e.to_string()))?;
    ensure_private_dir(&layout.links_dir()).map_err(|e| LinkError::Io(e.to_string()))?;

    let mut linked = Vec::new();
    let mut refused = Vec::new();
    for rel in bins {
        let Some(name) = bin_tool_name(rel) else {
            continue;
        };
        if !crate::store::shim_allowed(name) {
            // SECURITY: the sensitive-name deny-list is honored for dev links too.
            refused.push(name.to_string());
            continue;
        }
        let src = checkout.join(rel);
        if !src.is_file() {
            continue; // a not-yet-built bin is simply skipped (refresh picks it up later)
        }
        crate::platform::install_shim_to(&layout.shim(name), &src)
            .map_err(|e| LinkError::Io(e.to_string()))?;
        linked.push(name.to_string());
    }
    if linked.is_empty() {
        return Err(LinkError::NoBins);
    }

    let marker = LinkMarker {
        schema: 1,
        program: program.to_string(),
        checkout: checkout.display().to_string(),
        bins: bins.iter().map(|b| b.display().to_string()).collect(),
        linked_at: String::new(),
    };
    write_marker(&layout.link_marker(program), &marker)
        .map_err(|e| LinkError::Io(e.to_string()))?;
    Ok(LinkOutcome { linked, refused })
}

/// Un-link `program`: remove the shims that STILL point into its recorded checkout (never a
/// re-installed store shim), then delete the marker. Leaves any pin-state untouched, so a
/// pin survives the cycle.
pub fn unlink(layout: &Layout, program: &str) -> Result<(), LinkError> {
    if !safe_component(program) {
        return Err(LinkError::BadName(program.to_string()));
    }
    let marker_path = layout.link_marker(program);
    let text =
        fs::read_to_string(&marker_path).map_err(|_| LinkError::NotLinked(program.to_string()))?;
    let marker: LinkMarker =
        toml::from_str(&text).map_err(|_| LinkError::NotLinked(program.to_string()))?;
    let checkout = PathBuf::from(&marker.checkout);
    for rel in &marker.bins {
        let Some(name) = bin_tool_name(Path::new(rel)) else {
            continue;
        };
        let link = layout.shim(name);
        // Only remove a link STILL pointing into the checkout — never nuke a re-installed
        // store shim that happens to share the name. `resolve_shim` reads the forward
        // target cross-platform (symlink target on Unix, the `.cmd` target on Windows) —
        // a bare `fs::read_link` would Err on a Windows `.cmd` and leak the dev shim.
        if let Some(target) = crate::platform::resolve_shim(&link)
            && target.starts_with(&checkout)
        {
            let _ = fs::remove_file(&link);
        }
    }
    let _ = fs::remove_file(&marker_path);
    Ok(())
}

/// The recorded checkout root of a dev-linked `program` (from its `0600` marker), or
/// `None` when it is not linked / the marker is unreadable. Read-only. The config
/// link reconciliation (`[packages.links]`) uses this to detect a HAND-MADE link
/// pointing at a DIFFERENT checkout — which it must refuse to touch, loudly, rather
/// than silently re-point a developer's live dev loop.
#[must_use]
pub fn linked_checkout(layout: &Layout, program: &str) -> Option<PathBuf> {
    if !safe_component(program) {
        return None;
    }
    let text = fs::read_to_string(layout.link_marker(program)).ok()?;
    let marker: LinkMarker = toml::from_str(&text).ok()?;
    Some(PathBuf::from(marker.checkout))
}

/// Re-assert a program's dev links (idempotent): re-run [`link`] from the recorded marker,
/// picking up any newly-built/added bins. No build is invoked — building is producer scope,
/// absent from the consumer (a documented divergence from aterm-pkg's `refresh`).
pub fn refresh(layout: &Layout, program: &str) -> Result<LinkOutcome, LinkError> {
    let marker_path = layout.link_marker(program);
    let text =
        fs::read_to_string(&marker_path).map_err(|_| LinkError::NotLinked(program.to_string()))?;
    let marker: LinkMarker =
        toml::from_str(&text).map_err(|_| LinkError::NotLinked(program.to_string()))?;
    let checkout = PathBuf::from(&marker.checkout);
    let bins: Vec<PathBuf> = marker.bins.iter().map(PathBuf::from).collect();
    link(layout, program, &checkout, &bins)
}

/// The names of all dev-linked programs (the safe-component file names under `links/`).
#[must_use]
pub fn linked_programs(layout: &Layout) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(layout.links_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        if let Some(n) = e.file_name().to_str()
            && safe_component(n)
        {
            out.push(n.to_string());
        }
    }
    out.sort();
    out
}

/// Write the marker `0600` via temp + rename.
fn write_marker(dest: &Path, marker: &LinkMarker) -> std::io::Result<()> {
    let text = toml::to_string(marker)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let parent = dest.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "link marker has no parent",
        )
    })?;
    let tmp = parent.join(format!(".link.tmp-{}", std::process::id()));
    fs::write(&tmp, text.as_bytes())?;
    crate::platform::harden_file(&tmp)?;
    fs::rename(&tmp, dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::fs::Permissions;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-link-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&p, Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    /// A fake checkout with `target/release/<bins>`.
    fn checkout(label: &str, bins: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-checkout-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("target/release")).unwrap();
        for b in bins {
            fs::write(d.join("target/release").join(b), b"#!/bin/true\n").unwrap();
        }
        d
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
        // Shims point INTO the checkout; marker exists 0600; is_linked true. resolve_shim
        // reads the forward target cross-platform (a `.cmd` is not read_link-able).
        assert_eq!(
            crate::platform::resolve_shim(&l.shim("ay")).unwrap(),
            co.join("target/release/ay")
        );
        assert!(is_linked(&l, "ay"));
        // marker is 0600 — Unix-only mode check.
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
        assert!(fs::symlink_metadata(l.shim("ay")).is_err(), "shim removed");
        assert!(fs::symlink_metadata(l.shim("ny")).is_err(), "shim removed");
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn sensitive_bin_name_is_refused_a_link() {
        let l = layout("sensitive");
        let co = checkout("sensitive", &["ay", "git"]);
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/git"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string()]);
        assert_eq!(out.refused, vec!["git".to_string()]);
        assert!(
            fs::symlink_metadata(l.shim("git")).is_err(),
            "sensitive name never shimmed"
        );
        assert!(!crate::store::shim_allowed("git"));
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn unlink_only_removes_links_into_its_checkout() {
        let l = layout("scoped");
        let co = checkout("scoped", &["ay"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        // Overwrite the ay shim with a store-style shim (a re-install) OUTSIDE the checkout,
        // via the same primitive a real install uses (a symlink on Unix, a `.cmd` on Windows
        // — atomic_symlink is the DIRECTORY-junction primitive there, wrong for a bin shim).
        let store_target = l.build_dir("ay", 18).join("bin/ay");
        fs::create_dir_all(store_target.parent().unwrap()).unwrap();
        fs::write(&store_target, b"#!/bin/true\n").unwrap();
        crate::platform::install_shim_to(&l.shim("ay"), &store_target).unwrap();
        unlink(&l, "ay").unwrap();
        // The store shim survived — unlink only removes links into the checkout.
        assert_eq!(
            crate::platform::resolve_shim(&l.shim("ay")).unwrap(),
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
        // Link with both bins recorded, but ny not yet built.
        let bins = [
            PathBuf::from("target/release/ay"),
            PathBuf::from("target/release/ny"),
        ];
        let out = link(&l, "ay", &co, &bins).unwrap();
        assert_eq!(out.linked, vec!["ay".to_string()], "ny not built yet");
        // Build ny, then refresh.
        fs::write(co.join("target/release/ny"), b"#!/bin/true\n").unwrap();
        let out2 = refresh(&l, "ay").unwrap();
        assert!(
            out2.linked.contains(&"ny".to_string()),
            "refresh picks up the new bin"
        );
        assert_eq!(
            crate::platform::resolve_shim(&l.shim("ny")).unwrap(),
            co.join("target/release/ny")
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }

    #[test]
    fn linked_programs_lists_markers() {
        let l = layout("list");
        let co = checkout("list", &["ay", "ny"]);
        link(&l, "ay", &co, &[PathBuf::from("target/release/ay")]).unwrap();
        link(&l, "ny", &co, &[PathBuf::from("target/release/ny")]).unwrap();
        assert_eq!(
            linked_programs(&l),
            vec!["ay".to_string(), "ny".to_string()]
        );
        let _ = fs::remove_dir_all(&l.prefix);
        let _ = fs::remove_dir_all(&co);
    }
}
