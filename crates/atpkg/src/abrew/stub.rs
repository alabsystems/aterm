// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! The `brew` reroute stub.

use std::path::{Path, PathBuf};

use super::prefix::Prefix;
use crate::store::Layout;

/// Retired escape flag, refused before a native command runs.
pub const REAL_FLAG: &str = "--real";
/// Retired escape environment; engaged values block ordinary commands, leaving `on` and `status` available.
pub const REAL_ENV: &str = "ABREW_REAL";
/// Native-only capability route: an older backend refuses this instead of entering its legacy brew handler.
pub const HIDDEN_VERB: &str = "__abrew_native";
const STUB_MARKER: &str = "# atpkg-abrew stub v2 — regenerable; native atpkg only";
const LEGACY_STUB_MARKER: &str =
    "# atpkg-abrew stub v1 — regenerable; brew --real runs real Homebrew";

/// The stub file for a layout.
#[must_use]
pub fn stub_path(layout: &Layout) -> PathBuf {
    crate::reroute::dir(layout).join("brew")
}

/// Format the native-only stub body for `atpkg`.
#[must_use]
pub fn stub_body(atpkg: &Path) -> String {
    format!(
        "#!/bin/sh\n{STUB_MARKER}\nATPKG='{atpkg}'\n{STUB_GUARDS}\
if [ -x \"$ATPKG\" ]; then\n  exec \"$ATPKG\" {HIDDEN_VERB} \"$@\"\nfi\n\
echo \"abrew: atpkg unreachable; run aterm pkg repair; real Homebrew is never a fallback\" >&2\nexit 127\n",
        atpkg = sh_single(&atpkg.to_string_lossy()),
    )
}

fn sh_single(s: &str) -> String {
    s.replace('\'', "'\\''")
}

const STUB_GUARDS: &str = r#"_ABREW_COMMAND=${1:-}
case "$_ABREW_COMMAND" in
  --verbose|-v|--debug|-d)
    case "${2:-}" in
      ''|-*) ;;
      *) _ABREW_COMMAND=$2 ;;
    esac
    ;;
esac
case "$_ABREW_COMMAND" in
  on|status) ;;
  *)
    case "${ABREW_REAL:-}" in
      ''|0) ;;
      *) echo "abrew: ABREW_REAL is unsupported; brew uses native atpkg, never real Homebrew" >&2; exit 2 ;;
    esac
    case "${__ATERM_REROUTE_PASSTHROUGH:-}" in
      ''|0) ;;
      *) echo "abrew: brew is unavailable in a --no-reroute session; real Homebrew is never a fallback" >&2; exit 2 ;;
    esac
    ;;
esac
if [ "${1:-}" = "--real" ]; then
  echo "abrew: --real is unsupported; brew uses native atpkg, never real Homebrew" >&2
  exit 2
fi
"#;

enum AbrewSlot {
    Stub(Vec<u8>),
    Missing,
    Foreign,
}

fn classify_abrew_slot(path: &Path) -> AbrewSlot {
    match crate::metadata_io::read_bounded_regular(path, 16 * 1024) {
        Ok(bytes)
            if String::from_utf8_lossy(&bytes)
                .lines()
                .take(3)
                .any(|l| matches!(l.trim(), STUB_MARKER | LEGACY_STUB_MARKER)) =>
        {
            AbrewSlot::Stub(bytes)
        }
        Ok(_) => AbrewSlot::Foreign,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AbrewSlot::Missing,
        Err(_) if std::fs::symlink_metadata(path).is_ok() => AbrewSlot::Foreign,
        Err(_) => AbrewSlot::Missing,
    }
}

fn read_abrew_stub(path: &Path) -> Option<Vec<u8>> {
    match classify_abrew_slot(path) {
        AbrewSlot::Stub(bytes) => Some(bytes),
        AbrewSlot::Missing | AbrewSlot::Foreign => None,
    }
}

/// Whether the file at `path` is an abrew stub (by its marker line).
#[must_use]
pub fn is_abrew_stub(path: &Path) -> bool {
    read_abrew_stub(path).is_some()
}

/// Lay or refresh the stub for a layout (never over a foreign file).
pub fn lay(layout: &Layout) -> std::io::Result<()> {
    lay_with(layout, &crate::stub::Embedder::this_process())
}

/// [`lay`] with the caller's embedder — the reroute lay's own, so the
/// brew stub follows the same standing-aware rule as every reroute
/// stub: a copy run from a scratch dir keeps the standing stub's
/// (installed) atpkg; a moved app re-points it at its own.
pub(crate) fn lay_with(layout: &Layout, embedder: &crate::stub::Embedder) -> std::io::Result<()> {
    match plan(layout, embedder) {
        Plan::Nothing => Ok(()),
        Plan::Lay(path, body) => {
            layout.ensure_dir(&crate::reroute::dir(layout))?;
            crate::lay::write_regenerable(&crate::lay::Executable::new(&path, body.as_bytes()))
        }
    }
}

/// Whether [`lay_with`] would write the stub now: what `aterm pkg repair`'s
/// `base.pkg.stubs` item reads beside the reroute stubs. Changes nothing.
pub(crate) fn lay_pending(layout: &Layout, embedder: &crate::stub::Embedder) -> bool {
    !matches!(plan(layout, embedder), Plan::Nothing)
}

enum Plan {
    Nothing,
    Lay(PathBuf, String),
}

fn plan(layout: &Layout, embedder: &crate::stub::Embedder) -> Plan {
    if cfg!(windows) {
        return Plan::Nothing;
    }
    if layout.declined().is_file() {
        return Plan::Nothing;
    }
    let path = stub_path(layout);
    let slot = classify_abrew_slot(&path);
    let standing = match &slot {
        AbrewSlot::Stub(bytes) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|body| crate::stub::sh_named_atpkg(body, "ATPKG")),
        AbrewSlot::Missing | AbrewSlot::Foreign => None,
    };
    let offered = embedder.embed(standing.as_deref());
    let atpkg = crate::stub::named(offered.as_deref());
    let body = stub_body(atpkg);
    match slot {
        AbrewSlot::Stub(have) if have == body.as_bytes() => Plan::Nothing,
        AbrewSlot::Stub(_) | AbrewSlot::Missing => Plan::Lay(path, body),
        AbrewSlot::Foreign => Plan::Nothing, // foreign: never touched
    }
}

/// Doctor row and convergence class for the brew stub.
#[must_use]
pub fn doctor_row(layout: &Layout) -> (crate::doctor::Row, aterm_converge::Class) {
    let prefix = Prefix::resolve();
    doctor_row_with(
        layout,
        crate::config::load_abrew().enabled(),
        prefix.is_disabled(),
    )
}

fn doctor_row_with(
    layout: &Layout,
    enabled: bool,
    disabled: bool,
) -> (crate::doctor::Row, aterm_converge::Class) {
    use crate::doctor::{Level, Row};
    use aterm_converge::Class;
    let path = stub_path(layout);
    let slot = classify_abrew_slot(&path);
    let row = |level, text| (Row { level, text }, Class::Relay);
    if let AbrewSlot::Stub(bytes) = &slot
        && std::str::from_utf8(bytes).ok().is_some_and(|body| {
            body.lines()
                .take(3)
                .any(|line| line.trim() == LEGACY_STUB_MARKER)
        })
    {
        return row(
            Level::Warn,
            "reroute stub brew needs native-only refresh; run aterm pkg repair".to_string(),
        );
    }
    if !enabled {
        return row(
            Level::Note,
            "abrew off by [abrew] enabled=false; brew refuses commands until re-enabled"
                .to_string(),
        );
    }
    if disabled {
        return row(
            Level::Note,
            "abrew off (`abrew on` re-enables); brew never falls back to Homebrew".to_string(),
        );
    }
    match slot {
        AbrewSlot::Stub(bytes) => {
            let body = std::str::from_utf8(&bytes).unwrap_or_default();
            if !body.lines().take(3).any(|line| line.trim() == STUB_MARKER) {
                return row(
                    Level::Warn,
                    "reroute stub brew needs native-only refresh; run aterm pkg repair".to_string(),
                );
            }
            let live = stub_names_live_atpkg(body);
            if live {
                row(
                    Level::Ok,
                    "reroute stub brew laid (native atpkg only)".to_string(),
                )
            } else {
                row(
                    Level::Warn,
                    "reroute stub brew names no live atpkg; brew refuses commands until aterm pkg repair re-lays it".to_string(),
                )
            }
        }
        AbrewSlot::Foreign => (
            Row {
                level: Level::Warn,
                text: format!(
                    "{} is not ours (foreign file; never touched)",
                    path.display()
                ),
            },
            Class::Person,
        ),
        AbrewSlot::Missing => row(
            Level::Warn,
            "reroute stub brew missing; the next aterm session lays it — now: aterm pkg repair"
                .to_string(),
        ),
    }
}

/// Whether `body` names an executable `atpkg`.
#[must_use]
pub fn stub_names_live_atpkg(body: &str) -> bool {
    crate::stub::sh_named_atpkg(body, "ATPKG").is_some_and(|p| crate::stub::is_live_atpkg(&p))
}

/// Remove an owned stub during uninstall.
pub fn remove(layout: &Layout) {
    let path = stub_path(layout);
    if is_abrew_stub(&path) {
        let _ = std::fs::remove_file(path);
    }
}

/// Whether an env value counts as engaged.
#[must_use]
pub fn engaged(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

#[cfg(test)]
#[path = "stub_tests.rs"]
mod tests;
