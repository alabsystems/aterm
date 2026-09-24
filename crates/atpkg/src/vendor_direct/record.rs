// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `store/<program>/<build>.vendor` (design §1.3): what a vendor build was verified
//! against. The stager writes it after the swap and the codesign check, then flushes the
//! directory, then marks the build ready — so a reader trusts it only beside a complete
//! build ([`complete_record`]).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::table::{Anchor, VendorSpec, spec};
use super::version::{BuildDate, Version};

/// The record's schema.
pub const RECORD_SCHEMA: u32 = 1;

/// The sibling suffix: `store/<program>/<build>.vendor`.
pub const RECORD_SUFFIX: &str = ".vendor";

/// Bound on a record read (it holds a dozen short fields).
const MAX_RECORD_BYTES: usize = 16 * 1024;

/// One verified vendor build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VendorRecord {
    /// [`RECORD_SCHEMA`]; any other value is not read.
    pub schema: u32,
    /// The program; equals the store directory it sits in.
    pub program: String,
    /// The version; its build id equals the build directory's name.
    pub version: Version,
    /// The vendor's display name at verification.
    pub vendor: String,
    /// The URL the payload was fetched from.
    pub source_url: String,
    /// The payload's verified sha256 (lowercase hex).
    pub sha256: String,
    /// The payload's exact size in bytes.
    pub size: u64,
    /// The staged tree's root, folded at stage (lowercase hex).
    pub tree_root: String,
    /// The Apple team every Mach-O was checked against: the program's on darwin, `None`
    /// elsewhere (no codesign check runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apple_team: Option<String>,
    /// What authenticated the digest.
    pub anchor: Anchor,
    /// The signed manifest's `buildDate` (claude only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_date: Option<BuildDate>,
    /// When the build was verified (Unix seconds).
    pub verified_at: i64,
}

impl VendorRecord {
    /// The bytes the stager writes to [`record_path`] (with
    /// [`super::durable::write_durable`]). Refuses a record [`read_record`] would not read
    /// back, apart from the build-directory checks only a path can make.
    ///
    /// # Errors
    /// A field is malformed, or the record does not serialize.
    pub(crate) fn record_bytes(&self) -> io::Result<Vec<u8>> {
        if !self.well_formed() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "vendor record is malformed",
            ));
        }
        super::durable::to_toml(self, MAX_RECORD_BYTES)
    }

    /// The installed-build view [`super::decide`] takes.
    #[must_use]
    pub(crate) fn installed_view(&self) -> super::decide::InstalledView {
        super::decide::InstalledView {
            version: self.version,
            build: self.version.build_id(),
            build_date: self.build_date,
        }
    }

    /// Field shapes, independent of where the record sits: well-formed, and what this
    /// build's compiled row would have written — its vendor, its anchor, a payload URL
    /// under its pinned prefixes, and on darwin its Apple team (so `.ready` beside the
    /// record still means team-checked).
    fn well_formed(&self) -> bool {
        let Some(row) = spec(&self.program) else {
            return false;
        };
        self.schema == RECORD_SCHEMA
            && self.vendor == row.vendor
            && self.anchor == row.anchor
            && crate::vendor::https_host(&self.source_url).is_some()
            && row.url_prefixes.iter().any(|p| {
                self.source_url
                    .strip_prefix(p)
                    .is_some_and(|rest| !rest.is_empty())
            })
            && super::is_hex64(&self.sha256)
            && super::is_hex64(&self.tree_root)
            && self.size > 0
            && self.apple_team.as_deref() == expected_team(row)
    }
}

/// The Apple team a record of `row` names: the program's on darwin, where the stager
/// checks every Mach-O against it; none elsewhere.
fn expected_team(row: &VendorSpec) -> Option<&'static str> {
    cfg!(target_os = "macos").then_some(row.apple_team)
}

/// `store/<program>/<build>.vendor` beside `build_dir`.
#[must_use]
pub fn record_path(build_dir: &Path) -> Option<PathBuf> {
    let name = build_dir.file_name()?.to_str()?;
    let mut sidecar = String::from(name);
    sidecar.push_str(RECORD_SUFFIX);
    Some(build_dir.with_file_name(sidecar))
}

/// The record beside `build_dir`, or `None` when it is absent, unreadable, of another
/// schema, malformed, or names another program or build than the directory it sits
/// beside. It says nothing about the tree; see [`complete_record`].
#[must_use]
pub fn read_record(build_dir: &Path) -> Option<VendorRecord> {
    let record: VendorRecord =
        super::durable::read_toml(&record_path(build_dir)?, MAX_RECORD_BYTES)?;
    let build = crate::store::parse_build_name(build_dir.file_name()?.to_str()?)?;
    let program = build_dir.parent()?.file_name()?.to_str()?;
    let consistent =
        record.well_formed() && record.program == program && record.version.build_id() == build;
    consistent.then_some(record)
}

/// [`read_record`] of a build the store holds as complete — the only record a reader
/// should act on.
#[must_use]
pub fn complete_record(build_dir: &Path) -> Option<VendorRecord> {
    if !crate::store::build_is_complete(build_dir) {
        return None;
    }
    read_record(build_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Layout;

    fn layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!(
            "atpkg-vendor-record-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Layout { prefix: p }
    }

    fn record() -> VendorRecord {
        VendorRecord {
            schema: RECORD_SCHEMA,
            program: "claude".into(),
            version: Version::parse("2.1.280").unwrap(),
            vendor: "Anthropic".into(),
            source_url:
                "https://downloads.claude.ai/claude-code-releases/2.1.280/darwin-arm64/claude"
                    .into(),
            sha256: "387a5c5dcdbb815085edf0baf79591f9d8894efe922bceaf3d75b1b08055229d".into(),
            size: 217_254_576,
            tree_root: "0".repeat(64),
            apple_team: team("claude"),
            anchor: Anchor::AnthropicOpenPgp,
            build_date: BuildDate::parse("2026-09-21T20:55:27Z"),
            verified_at: 1_790_000_000,
        }
    }

    /// The team a record of `program` carries on this platform.
    fn team(program: &str) -> Option<String> {
        expected_team(spec(program).unwrap()).map(str::to_string)
    }

    /// Write `rec` beside `store/<program>/<build>` the way the stager does.
    fn place(l: &Layout, program: &str, build: u64, rec: &VendorRecord) -> PathBuf {
        let dir = l.build_dir(program, build);
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = rec.record_bytes().unwrap();
        super::super::durable::write_durable(&record_path(&dir).unwrap(), &bytes).unwrap();
        dir
    }

    #[test]
    fn a_record_round_trips_beside_its_build() {
        let l = layout("roundtrip");
        let rec = record();
        let dir = place(&l, "claude", rec.version.build_id(), &rec);
        assert_eq!(
            record_path(&dir).unwrap().file_name().unwrap(),
            "1000002000001000280.vendor"
        );
        assert_eq!(read_record(&dir), Some(rec.clone()));
        let view = rec.installed_view();
        assert_eq!(view.build, 1_000_002_000_001_000_280);
        assert_eq!(view.build_date, rec.build_date);
        // codex: no buildDate.
        let bare = VendorRecord {
            program: "codex".into(),
            version: Version::parse("0.156.0").unwrap(),
            vendor: "OpenAI".into(),
            source_url: "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package-aarch64-apple-darwin.tar.gz".into(),
            anchor: Anchor::OpenAiTwoHost,
            apple_team: team("codex"),
            build_date: None,
            ..record()
        };
        let dir = place(&l, "codex", bare.version.build_id(), &bare);
        assert_eq!(read_record(&dir), Some(bare));
    }

    #[test]
    fn a_record_is_trusted_only_beside_a_complete_build() {
        let l = layout("complete");
        let rec = record();
        let dir = place(&l, "claude", rec.version.build_id(), &rec);
        assert_eq!(complete_record(&dir), None, "no .ready yet");
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(complete_record(&dir), Some(rec));
    }

    #[test]
    fn a_record_that_disagrees_with_its_directory_is_none() {
        let l = layout("mismatch");
        let rec = record();
        // Filed under another build number.
        let other = Version::parse("2.1.281").unwrap().build_id();
        assert_eq!(read_record(&place(&l, "claude", other, &rec)), None);
        // Filed under another program.
        assert_eq!(
            read_record(&place(&l, "codex", rec.version.build_id(), &rec)),
            None
        );
        // An ALab build number.
        assert_eq!(read_record(&place(&l, "claude", 2_026_091_901, &rec)), None);
    }

    #[test]
    fn a_malformed_record_is_none() {
        let l = layout("malformed");
        let rec = record();
        let dir = place(&l, "claude", rec.version.build_id(), &rec);
        let path = record_path(&dir).unwrap();
        let good = std::fs::read_to_string(&path).unwrap();
        for bad in [
            String::from("not toml {{{"),
            String::new(),
            good.replace("schema = 1", "schema = 2"),
            good.replace("2.1.280", "2.1.0280"),
            good.replace(&rec.sha256, "abc"),
            good.replace(&rec.sha256, &rec.sha256.to_uppercase()),
            good.replace("anthropic-openpgp", "nobody"),
            good.replace("2026-09-21T20:55:27Z", "yesterday"),
            good.replace("https://", "http://"),
            good.replace("size = 217254576", "size = 0"),
            good.lines()
                .filter(|l| !l.starts_with("verified_at"))
                .collect::<Vec<_>>()
                .join("\n"),
        ] {
            assert_ne!(bad, good);
            std::fs::write(&path, &bad).unwrap();
            assert_eq!(read_record(&dir), None, "{bad}");
        }
        // Not a regular file.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(read_record(&dir), None);
    }

    #[test]
    fn record_bytes_refuses_what_read_record_would_refuse() {
        for bad in [
            VendorRecord {
                schema: 2,
                ..record()
            },
            VendorRecord {
                program: "trust".into(),
                ..record()
            },
            VendorRecord {
                sha256: "zz".into(),
                ..record()
            },
            VendorRecord {
                tree_root: String::new(),
                ..record()
            },
            VendorRecord {
                size: 0,
                ..record()
            },
            VendorRecord {
                apple_team: Some("short".into()),
                ..record()
            },
            VendorRecord {
                vendor: String::new(),
                ..record()
            },
        ] {
            assert!(bad.record_bytes().is_err(), "{bad:?}");
        }
    }

    /// A record that is not what the program's compiled row would have written is refused
    /// on write and on read: another anchor, vendor, payload host or Apple team — and on
    /// darwin, no team at all (a stager that skipped codesign).
    #[test]
    fn a_record_must_match_the_compiled_row() {
        let other_team = match team("claude") {
            Some(_) => vec![
                None,
                Some("2DC432GLL2".to_string()),
                Some("q6l2sf6ydw".into()),
            ],
            None => vec![Some("Q6L2SF6YDW".to_string())],
        };
        let mut bad: Vec<VendorRecord> = other_team
            .into_iter()
            .map(|apple_team| VendorRecord {
                apple_team,
                ..record()
            })
            .collect();
        bad.extend([
            VendorRecord {
                anchor: Anchor::OpenAiTwoHost,
                ..record()
            },
            VendorRecord {
                vendor: "OpenAI".into(),
                ..record()
            },
            VendorRecord {
                source_url: "https://evil.example/claude-code-releases/2.1.280/claude".into(),
                ..record()
            },
            VendorRecord {
                source_url: "https://downloads.claude.ai/other/2.1.280/claude".into(),
                ..record()
            },
            VendorRecord {
                source_url: "https://downloads.claude.ai/claude-code-releases/".into(),
                ..record()
            },
            VendorRecord {
                source_url: "https://github.com/openai/codex/releases/download/rust-v0.156.0/x"
                    .into(),
                ..record()
            },
        ]);
        let l = layout("row");
        for rec in bad {
            assert!(rec.record_bytes().is_err(), "{rec:?}");
            // Written around `record_bytes`, as a buggy stager or a hand edit would.
            let dir = l.build_dir("claude", rec.version.build_id());
            std::fs::create_dir_all(&dir).unwrap();
            let bytes = super::super::durable::to_toml(&rec, MAX_RECORD_BYTES).unwrap();
            std::fs::write(record_path(&dir).unwrap(), bytes).unwrap();
            crate::store::mark_build_ready(&dir).unwrap();
            assert_eq!(read_record(&dir), None, "{rec:?}");
            assert_eq!(complete_record(&dir), None, "{rec:?}");
        }
    }
}
