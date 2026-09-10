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

/// Maximum serialized size of the operator-readable status record.
///
/// A normal record is a few KiB. Two MiB leaves room for a large managed fleet
/// while making Settings/doctor/verify parsing and allocation strictly finite.
pub const MAX_STATUS_BYTES: usize = 2 * 1024 * 1024;

/// Maximum number of per-program rows admitted from one status snapshot.
pub const MAX_STATUS_PROGRAMS: usize = 2048;

/// One program's last-known state, for `status.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramStatus {
    /// The currently-active build, if any.
    pub installed_build: Option<u64>,
    /// The program's state line. For a program the index names it is one of the CANONICAL
    /// spellings of [`crate::state`] — `managed <build> — pinned by index <N>`, `system:
    /// <path> — not managed by aterm`, `managed <build> — SHADOWED by <path>`, `extra — not
    /// installed (opt in: …)`, `installed via <protocol>: <path>`, `needs admin — …`,
    /// `unavailable on <target>: <hint>` — the same words the pass log, `doctor` and
    /// `which` print. Faults keep their prefixed free text (`"error: …"`, `"tombstoned:
    /// yanked@N"`, `"deferred: …"`, `"rejected: unsigned index at build N"`, …), which
    /// `doctor` matches by prefix.
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
    /// The rustup toolchain seams atpkg owns, as `rustup:<name>` ([`crate::seam`],
    /// Lockstep S1): written when an attach succeeds, dropped by `seam detach` and
    /// `uninstall --all`, and what every re-assertion walks. Absent from records
    /// written before seams existed, so it defaults EMPTY — and an older app reading
    /// a newer record simply ignores the key.
    #[serde(default)]
    pub seams: Vec<String>,
    /// RFC3339 UTC time of the last SUCCESSFUL update pass — stamped by
    /// [`stamp_success`] alone, never by the per-program writers, which move
    /// `updated_at` on every write (a failed resolve, a shadow reconcile, a seed offer
    /// row) and so made "N day(s) since the last successful update" a sentence about
    /// the last write of any kind (2026-09-10 audit). Empty ⇒ no successful pass has
    /// completed since this field existed: [`never_checked`] reads it, and every
    /// read-only verb prints [`NEVER_CHECKED_LINE`] on stderr while it is empty. A
    /// record written before the field parses as empty — a machine that has been
    /// updating for a month reads "never checked" for exactly one pass, then the next
    /// success stamps it.
    #[serde(default)]
    pub last_success_at: String,
    /// Per-program states, keyed by program name.
    #[serde(default)]
    pub programs: BTreeMap<String, ProgramStatus>,
}

/// The stderr line every read-only verb (`list`, `which`, `status`, `doctor`, the
/// `__pending` stub) prints while [`never_checked`] holds — R3 (owner requirement
/// 2026-09-10): a console must SAY that packages cannot be updated yet, not leave it
/// to a window nobody has opened. Byte-stable: the GUI and the session lane print the
/// same words from this constant.
pub const NEVER_CHECKED_LINE: &str = "atpkg: no update check has run yet on this machine — \
                                      packages cannot be updated until the first pass \
                                      completes (run: aterm pkg update)";

/// Whether NO update pass has ever completed successfully on this machine:
/// `status.toml` absent, unreadable, or present with an empty
/// [`Status::last_success_at`]. This is the R3 condition, and nothing weaker: a record
/// that exists because a failed resolve wrote its `*index*` row is not a check that
/// ran.
#[must_use]
pub fn never_checked(layout: &Layout) -> bool {
    read(layout).is_none_or(|s| s.last_success_at.trim().is_empty())
}

/// Stamp `last_success_at = now` (and `updated_at`, which every write moves) on the
/// record, creating a minimal one when none exists. Called by the CLI at the END of a
/// pass that resolved the signed index and applied it without a failure — the one
/// event that makes "packages can be updated on this machine" true. Best-effort like
/// every status write.
pub fn stamp_success(layout: &Layout, now: &str) -> io::Result<()> {
    let mut status = read(layout).unwrap_or(Status {
        schema: 1,
        ..Default::default()
    });
    status.last_success_at = now.to_string();
    status.updated_at = now.to_string();
    write(layout, &status)
}

impl Status {
    /// Serialize to TOML.
    ///
    /// # Errors
    /// The serializer's message, prefixed, when the map cannot be rendered.
    pub fn to_toml(&self) -> Result<String, String> {
        aterm_toml::to_string(self).map_err(|e| {
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
    if status.programs.len() > MAX_STATUS_PROGRAMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "status.toml has {} programs; limit is {MAX_STATUS_PROGRAMS}",
                status.programs.len()
            ),
        ));
    }
    let text = status
        .to_toml()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if text.len() > MAX_STATUS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("status.toml exceeds the {MAX_STATUS_BYTES}-byte limit"),
        ));
    }
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

/// Read + parse `status.toml` with explicit admission/parse diagnostics.
///
/// Missing is a normal `Ok(None)`. Existing paths must be bounded regular
/// non-link files containing valid UTF-8 TOML and no more than
/// [`MAX_STATUS_PROGRAMS`] rows.
pub fn read_checked(layout: &Layout) -> io::Result<Option<Status>> {
    let path = layout.status();
    let text = match crate::metadata_io::read_bounded_regular_utf8(&path, MAX_STATUS_BYTES) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let status: Status = aterm_toml::from_str(&text).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("status.toml is invalid: {error}"),
        )
    })?;
    if status.programs.len() > MAX_STATUS_PROGRAMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "status.toml has {} programs; limit is {MAX_STATUS_PROGRAMS}",
                status.programs.len()
            ),
        ));
    }
    Ok(Some(status))
}

/// Compatibility projection: absent or malformed diagnostics yield `None`.
/// Interactive surfaces which can show an error should use [`read_checked`].
#[must_use]
pub fn read(layout: &Layout) -> Option<Status> {
    read_checked(layout).ok().flatten()
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
            seams: Vec::new(),
            last_success_at: "2026-06-29T00:00:00Z".into(),
            programs,
        };
        write(&l, &s).unwrap();
        let back = read(&l).expect("status reads back");
        assert_eq!(back, s);
        // The signed tree_root survives the TOML round-trip (drives `atpkg verify`).
        assert_eq!(back.programs["ay"].tree_root, "abc123");
        // It is valid TOML on disk.
        let text = std::fs::read_to_string(l.status()).unwrap();
        let _: aterm_toml::Value = aterm_toml::from_str(&text).expect("valid TOML");
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
        assert_eq!(read_checked(&l).unwrap(), None);
        std::fs::write(l.status(), "this is not valid toml {{{").unwrap();
        assert!(read(&l).is_none(), "corrupt status is never load-bearing");
        let error = read_checked(&l).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("status.toml is invalid"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    #[test]
    fn status_program_count_is_bounded_on_read_and_write() {
        let l = layout("program-cap");
        let mut programs = BTreeMap::new();
        for index in 0..=MAX_STATUS_PROGRAMS {
            programs.insert(format!("p{index}"), ProgramStatus::default());
        }
        let status = Status {
            schema: 1,
            programs,
            ..Default::default()
        };
        let write_error = write(&l, &status).unwrap_err();
        assert_eq!(write_error.kind(), io::ErrorKind::InvalidData);
        assert!(write_error.to_string().contains("limit is"));

        // Bypass the writer to prove a hostile otherwise-valid on-disk record
        // is rejected by the independent read-side count gate too.
        std::fs::write(l.status(), status.to_toml().unwrap()).unwrap();
        let read_error = read_checked(&l).unwrap_err();
        assert_eq!(read_error.kind(), io::ErrorKind::InvalidData);
        assert!(read_error.to_string().contains("limit is"));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// Lockstep S1: the rustup `seams` record round-trips beside the program rows —
    /// aterm-toml orders values before sub-tables, so the array never lands inside
    /// the last `[programs.*]` table — and a record that predates the field (the
    /// 0.65 shape) parses as "no seams", never as an error.
    #[test]
    fn seams_round_trip_and_default_empty() {
        let l = layout("seams");
        let mut programs = BTreeMap::new();
        programs.insert(
            "trust".to_string(),
            ProgramStatus {
                installed_build: Some(6808),
                state: "managed 6808 — pinned by index 9".into(),
                tree_root: "r".into(),
            },
        );
        programs.insert("ay".to_string(), ProgramStatus::default());
        let s = Status {
            schema: 1,
            updated_at: "2026-08-29T00:00:00Z".into(),
            enabled: true,
            index_source: "o/r".into(),
            outcome: "up to date".into(),
            seams: vec!["rustup:trust".into()],
            last_success_at: String::new(),
            programs,
        };
        write(&l, &s).unwrap();
        let back = read(&l).expect("status reads back");
        assert_eq!(back, s);
        assert_eq!(back.seams, vec!["rustup:trust".to_string()]);
        let text = std::fs::read_to_string(l.status()).unwrap();
        let seams_at = text.find("seams = [").expect("seams array on disk");
        let table_at = text.find("[programs.").expect("program tables on disk");
        assert!(
            seams_at < table_at,
            "seams must precede the program tables:\n{text}"
        );

        // The 0.65 shape: no `seams` key at all.
        let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
             index_source = \"o/r\"\noutcome = \"up to date\"\n\n[programs.trust]\n\
             installed_build = 6808\nstate = \"managed 6808 — pinned by index 9\"\n\
             tree_root = \"r\"\n";
        std::fs::write(l.status(), legacy).unwrap();
        let back = read_checked(&l).unwrap().expect("legacy record parses");
        assert!(back.seams.is_empty(), "no key ⇒ no seams");
        assert_eq!(back.programs["trust"].installed_build, Some(6808));
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// R3: "never checked" is exactly `status.toml` absent OR `last_success_at` empty —
    /// a record a FAILED pass wrote (its `*index*` row, a fresh `updated_at`) still reads
    /// as never checked; only [`stamp_success`] clears it, and the stamp round-trips
    /// beside the program rows.
    #[test]
    fn never_checked_holds_until_a_successful_pass_stamps_last_success_at() {
        let l = layout("never-checked");
        assert!(never_checked(&l), "no status.toml at all");
        // A failed pass's record: written, stamped, and still not a completed check.
        let mut programs = BTreeMap::new();
        programs.insert(
            "*index*".to_string(),
            ProgramStatus {
                installed_build: None,
                state: "error: index unreachable".into(),
                tree_root: String::new(),
            },
        );
        write(
            &l,
            &Status {
                schema: 1,
                updated_at: "2026-09-10T06:40:53Z".into(),
                outcome: "update failed: index unreachable".into(),
                programs,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            never_checked(&l),
            "a failed pass moves updated_at but is not a check that ran"
        );
        // A record from before the field existed parses as never checked too.
        let legacy = "schema = 1\nupdated_at = \"2026-08-01T00:00:00Z\"\nenabled = true\n\
             index_source = \"o/r\"\noutcome = \"up to date\"\n";
        std::fs::write(l.status(), legacy).unwrap();
        assert!(never_checked(&l), "pre-field record: empty last_success_at");
        // The success stamp — and only it — clears the condition, keeping the rest.
        stamp_success(&l, "2026-09-10T07:00:00Z").unwrap();
        assert!(!never_checked(&l));
        let back = read(&l).unwrap();
        assert_eq!(back.last_success_at, "2026-09-10T07:00:00Z");
        assert_eq!(back.updated_at, "2026-09-10T07:00:00Z");
        assert_eq!(back.outcome, "up to date", "the sentence is not touched");
        let text = std::fs::read_to_string(l.status()).unwrap();
        assert!(text.contains("last_success_at = \"2026-09-10T07:00:00Z\""));
        // Whitespace is not a stamp.
        std::fs::write(l.status(), "schema = 1\nlast_success_at = \"  \"\n").unwrap();
        assert!(never_checked(&l));
        // No record at all: the stamp creates a minimal one.
        std::fs::remove_file(l.status()).unwrap();
        stamp_success(&l, "2026-09-10T08:00:00Z").unwrap();
        assert!(!never_checked(&l));
        assert_eq!(read(&l).unwrap().schema, 1);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// The R3 line is a byte-stable contract shared with the session lane and the GUI.
    #[test]
    fn the_never_checked_line_is_the_contract_string() {
        assert_eq!(
            NEVER_CHECKED_LINE,
            "atpkg: no update check has run yet on this machine — packages cannot be \
             updated until the first pass completes (run: aterm pkg update)"
        );
    }
}
