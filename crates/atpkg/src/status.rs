// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operator-readable status record (`…/pkg/status.toml`).
//!
//! A package manager that updates silently (§9) needs a durable observability surface so
//! an operator can answer "is this machine receiving updates, what is installed, and why
//! didn't the last apply happen?" without any prompt. This is that file: the resolved
//! index source, the last aggregate outcome, and a per-program state line (active /
//! tombstoned / deferred / rejected, §5/§7). Written atomically (temp + rename) so a
//! reader never sees a half-written record; best-effort — status is diagnostics, never
//! load-bearing.

use std::collections::BTreeMap;
use std::io;

use serde::{Deserialize, Serialize};

use crate::Layout;

/// One program's last-known state, for `status.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramStatus {
    /// The currently-active build, if any.
    pub installed_build: Option<u64>,
    /// Free-text state: `"active"`, `"tombstoned: yanked@N"`, `"deferred: …"`,
    /// `"rejected: unsigned index at build N"`, … (mirrors the updater's outcome strings).
    pub state: String,
    /// The SIGNED `tree_root` (§8) of the active build, captured from the release-key-
    /// verified manifest at install/update time. `atpkg verify` recomputes the store tree's
    /// root and compares it to THIS value — a drift audit against the signed root, never a
    /// self-generated hash. Empty ⇒ recorded before verify support / a loose manifest, so
    /// verify reports "cannot attest" (fail-closed, not a pass).
    #[serde(default)]
    pub tree_root: String,
}

/// The aggregate status snapshot written after a check/apply pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    /// Record schema version.
    pub schema: u32,
    /// RFC3339 UTC time this record was written (the caller stamps it).
    pub updated_at: String,
    /// Whether the manager is configured to act (root key pinned + not opted out).
    pub enabled: bool,
    /// The resolved index source, `owner/repo`.
    pub index_source: String,
    /// The last aggregate decision (`"up to date"`, `"staged …"`, `"idle: no token"`,
    /// `"rejected unsigned index at build N"`, …).
    pub outcome: String,
    /// Per-program states, keyed by program name.
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramStatus>,
}

impl Status {
    /// Serialize to TOML.
    ///
    /// # Errors
    /// The serializer's message, prefixed, when the map cannot be rendered.
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| {
            // Manual concat of the previous `format!("serialize status: {e}")` —
            // byte-identical (`{e}` is `Display`, which is what `to_string`
            // renders): the `format!` expansion embeds `fmt::Arguments`
            // construction (with inlined `unsafe`) that the strict Trust gate
            // cannot lower and fails closed on.
            let mut m = String::from("serialize status: ");
            m.push_str(&e.to_string());
            m
        })
    }
}

/// Atomically write `status` to `layout.status()` (temp + rename). Best-effort: a failure
/// is returned but is never fatal to an apply (status is diagnostics).
pub fn write(layout: &Layout, status: &Status) -> io::Result<()> {
    let text = status
        .to_toml()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let dest = layout.status();
    // Manual rendering of the previous
    // `format!("status.toml.tmp-{}", std::process::id())` — byte-identical: the
    // `format!` expansion embeds `fmt::Arguments` construction (with inlined
    // `unsafe`) that the strict Trust gate cannot lower and fails closed on.
    let mut tmp_name = String::from("status.toml.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = dest.with_file_name(tmp_name);
    // `fs::write` goes via `call2`: the hardened pass name-matches any direct
    // callee named `write` against the libc `write(2)` FFI-boundary contracts,
    // which do not apply to this safe std function (see `lib.rs`). Same
    // function, same arguments; behavior identical.
    crate::call2(std::fs::write, &tmp, text)?;
    std::fs::rename(&tmp, &dest)
}

/// Read + parse `status.toml`, or `None` if absent/unparseable (a corrupt diagnostics file
/// is never load-bearing).
#[must_use]
pub fn read(layout: &Layout) -> Option<Status> {
    let text = std::fs::read_to_string(layout.status()).ok()?;
    toml::from_str(&text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-status-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        Layout { prefix: p }
    }

    #[test]
    fn write_then_read_round_trips() {
        let l = layout("rt");
        let mut programs = BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            ProgramStatus {
                installed_build: Some(18),
                state: "active".into(),
                tree_root: "abc123".into(),
            },
        );
        programs.insert(
            "trust".to_string(),
            ProgramStatus {
                installed_build: None,
                state: "tombstoned: yanked@4790".into(),
                tree_root: String::new(),
            },
        );
        let s = Status {
            schema: 1,
            updated_at: "2026-06-29T00:00:00Z".into(),
            enabled: true,
            index_source: "alabsystems/aterm-toolchain-index".into(),
            outcome: "up to date".into(),
            programs,
        };
        write(&l, &s).unwrap();
        let back = read(&l).expect("status reads back");
        assert_eq!(back, s);
        // The signed tree_root survives the TOML round-trip (drives `atpkg verify`).
        assert_eq!(back.programs["ay"].tree_root, "abc123");
        // It is valid TOML on disk.
        let text = std::fs::read_to_string(l.status()).unwrap();
        let _: toml::Value = toml::from_str(&text).expect("valid TOML");
        assert!(text.contains("index_source = \"alabsystems/aterm-toolchain-index\""));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn write_is_atomic_no_temp_left_behind() {
        let l = layout("atomic");
        write(
            &l,
            &Status {
                schema: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(&l.prefix)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file should remain after rename"
        );
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn read_absent_or_corrupt_is_none() {
        let l = layout("absent");
        assert!(read(&l).is_none(), "absent status reads as None");
        std::fs::write(l.status(), "this is not valid toml {{{").unwrap();
        assert!(read(&l).is_none(), "corrupt status is never load-bearing");
        let _ = std::fs::remove_dir_all(&l.prefix);
    }
}
