// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release manifest, the `aterm-appcast.toml` asset on every GitHub release.

use serde::{Deserialize, Serialize};

/// The highest manifest `schema` this build understands.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The release manifest attached to a GitHub Release as `aterm-appcast.toml`.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version (the publisher emits `schema = 1`).
    #[serde(default)]
    pub schema: u32,
    /// The release version.
    pub version: String,
    /// Monotonic build number: the downgrade gate.
    pub build_number: u64,
    /// Full git commit hash of the source the release was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The DMG asset's file name within the same release.
    pub dmg: String,
    /// SHA-256 (lowercase hex) of the DMG asset's bytes.
    pub sha256: String,
    /// Absolute browser download URL of the DMG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The ZIP asset's file name, e.g. `"aterm-0.10.0-mac.zip"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::zip`]'s bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_sha256: Option<String>,
    /// Native Linux raw ELF assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_x86_64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_x86_64_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_x86_64_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_aarch64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_aarch64_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linux_aarch64_size: Option<u64>,
    /// The Intel batteries-included DMG's asset name, e.g. `"aterm-0.47.0-x86_64.dmg"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::dmg_x86_64`]'s bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64_sha256: Option<String>,
    /// Minimum macOS version the bundle declares (`LSMinimumSystemVersion`), e.g. `"11.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_os: Option<String>,
    /// Apple Team ID the release is signed under; `""` is the ad hoc tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// RFC3339 UTC publish time, e.g. `"2026-07-06T21:29:44Z"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,
    /// Optional operator apply floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_build: Option<u64>,
    /// The id of the machine that signed this release, e.g. `"m3"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// The `roster_seq` of the roster that authorized [`Self::machine_id`] at cut time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_seq: Option<u64>,
    /// The release's CHANGELOG.md section body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

impl Manifest {
    /// Select a native target only when all its signed identity fields agree.
    pub fn linux_artifact(
        &self,
        target: crate::linux::LinuxTarget,
    ) -> Result<Option<crate::linux::LinuxArtifact<'_>>, String> {
        let (name, digest, size) = match target {
            crate::linux::LinuxTarget::X86_64 => (
                self.linux_x86_64.as_deref(),
                self.linux_x86_64_sha256.as_deref(),
                self.linux_x86_64_size,
            ),
            crate::linux::LinuxTarget::Aarch64 => (
                self.linux_aarch64.as_deref(),
                self.linux_aarch64_sha256.as_deref(),
                self.linux_aarch64_size,
            ),
        };
        crate::linux::artifact(target, &self.version, name, digest, size)
    }

    /// Parse a manifest from TOML text, rejecting a schema this build is too old to understand.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = aterm_toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema > SUPPORTED_SCHEMA {
            return Err(format!(
                "manifest schema {} is newer than supported ({SUPPORTED_SCHEMA}); upgrade aterm",
                self.schema
            ));
        }
        if let Some(min_build) = self.min_build
            && min_build > self.build_number
        {
            return Err(format!(
                "manifest min_build {min_build} exceeds its build_number {}; refusing an \
                 impossible update floor",
                self.build_number
            ));
        }
        self.linux_artifact(crate::linux::LinuxTarget::X86_64)?;
        self.linux_artifact(crate::linux::LinuxTarget::Aarch64)?;
        Ok(())
    }

    /// Serialize to the published TOML shape.
    pub fn to_toml(&self) -> Result<String, String> {
        self.validate()?;
        if let Some(body) = &self.changelog {
            if body.contains("'''") {
                return Err(
                    "changelog contains ''' — unrepresentable inside a TOML multiline \
                     literal string; rewrite the offending CHANGELOG.md line"
                        .into(),
                );
            }
            if let Some(bad) = body
                .chars()
                .find(|c| c.is_control() && *c != '\n' && *c != '\t')
            {
                return Err(format!(
                    "changelog contains control character {bad:?} — TOML forbids it raw \
                     inside a literal string; rewrite the offending CHANGELOG.md line"
                ));
            }
        }
        let mut head = self.clone();
        head.changelog = None;
        let mut out =
            aterm_toml::to_string(&head).map_err(|e| format!("serialize manifest: {e}"))?;
        if let Some(body) = &self.changelog {
            out.push_str("changelog = '''\n");
            out.push_str(body);
            out.push_str("'''\n");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> Manifest {
        Manifest {
            schema: 1,
            version: "0.26".into(),
            build_number: 1_783_918_101,
            commit: Some("aed5a06caed5a06caed5a06caed5a06caed5a06c".into()),
            dmg: "aterm-0.26.dmg".into(),
            sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            url: Some(
                "https://github.com/alabsystems/aterm/releases/download/v0.26/aterm-0.26.dmg"
                    .into(),
            ),
            zip: Some("aterm-0.26-mac.zip".into()),
            zip_sha256: Some(
                "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
            ),
            dmg_x86_64: Some("aterm-0.26-x86_64.dmg".into()),
            dmg_x86_64_sha256: Some(
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
            ),
            linux_x86_64: None,
            linux_x86_64_sha256: None,
            linux_x86_64_size: None,
            linux_aarch64: None,
            linux_aarch64_sha256: None,
            linux_aarch64_size: None,
            min_os: Some("11.0".into()),
            team_id: Some(String::new()),
            pub_date: Some("2026-07-06T21:29:44Z".into()),
            min_build: None,
            machine_id: None,
            roster_seq: None,
            changelog: Some(
                "### Added\n- a `thing` with \"quotes\", # hashes and a \\ backslash\n".into(),
            ),
        }
    }

    fn linux_manifest() -> Manifest {
        let mut m = full();
        m.version = "0.86.0".into();
        m.dmg = "aterm-0.86.0.dmg".into();
        m.linux_aarch64 = Some("aterm-0.86.0-linux-aarch64".into());
        m.linux_aarch64_sha256 = Some("ab".repeat(32));
        m.linux_aarch64_size = Some(33_000_000);
        m
    }

    #[test]
    fn linux_entries_round_trip_and_select_only_the_declared_target() {
        use crate::linux::LinuxTarget;
        let mut m = linux_manifest();
        let aarch64 = m.linux_artifact(LinuxTarget::Aarch64).unwrap().unwrap();
        assert_eq!(aarch64.name, "aterm-0.86.0-linux-aarch64");
        assert_eq!(aarch64.size, 33_000_000);
        assert_eq!(aarch64.sha256, "ab".repeat(32));
        assert_eq!(m.linux_artifact(LinuxTarget::X86_64).unwrap(), None);
        assert_eq!(Manifest::parse(&m.to_toml().unwrap()).unwrap(), m);
        m.linux_x86_64 = Some("aterm-0.86.0-linux-x86_64".into());
        m.linux_x86_64_sha256 = Some("cd".repeat(32));
        m.linux_x86_64_size = Some(34_000_000);
        let text = m.to_toml().unwrap();
        assert_eq!(Manifest::parse(&text).unwrap(), m);
        assert_eq!(
            m.linux_artifact(LinuxTarget::X86_64).unwrap().unwrap().size,
            34_000_000
        );
        assert!(text.contains("changelog = '''\n"));
    }

    #[test]
    fn every_partial_linux_entry_fails_closed_on_parse_and_emit() {
        for fields in 1_u8..7 {
            let mut m = linux_manifest();
            if fields & 1 == 0 {
                m.linux_aarch64 = None;
            }
            if fields & 2 == 0 {
                m.linux_aarch64_sha256 = None;
            }
            if fields & 4 == 0 {
                m.linux_aarch64_size = None;
            }
            assert!(m.to_toml().is_err(), "partial mask {fields}");
            let wire = aterm_toml::to_string(&m).unwrap();
            assert!(Manifest::parse(&wire).is_err(), "partial mask {fields}");
        }
    }

    #[test]
    fn linux_identity_digest_size_and_legacy_version_mutants_are_rejected() {
        for name in [
            "aterm-0.85.0-linux-aarch64",
            "aterm-0.86.0-linux-x86_64",
            "aterm-0.86.0-linux-aarch64.tar.gz",
            "../aterm-0.86.0-linux-aarch64",
            "https://example.invalid/aterm",
        ] {
            let mut m = linux_manifest();
            m.linux_aarch64 = Some(name.into());
            assert!(m.to_toml().is_err(), "{name}");
        }
        for digest in ["".into(), "AB".repeat(32), "xy".repeat(32), "ab".repeat(31)] {
            let mut m = linux_manifest();
            m.linux_aarch64_sha256 = Some(digest);
            assert!(m.to_toml().is_err());
        }
        for size in [0, 63, crate::linux::LINUX_BINARY_MAX_BYTES + 1, u64::MAX] {
            let mut m = linux_manifest();
            m.linux_aarch64_size = Some(size);
            assert!(m.to_toml().is_err(), "size {size}");
        }
        for version in ["0.86", "0.086.0", "0.86.0+gabc", "0.86.0/../1"] {
            let mut m = linux_manifest();
            m.version = version.into();
            m.linux_aarch64 = Some(crate::linux::LinuxTarget::Aarch64.asset_name(version));
            assert!(m.to_toml().is_err(), "version {version}");
        }
    }

    #[test]
    fn absent_schema_is_zero_and_unknown_keys_tolerated() {
        let m = Manifest::parse(
            r#"version = "1.0.0"
               build_number = 10
               sha256 = "x"
               dmg = "a.dmg"
               some_future_key = "ignored""#,
        )
        .unwrap();
        assert_eq!(
            m.schema, 0,
            "absent schema must default to 0 (v0.25 semantics)"
        );
        assert_eq!(m.commit, None);
        assert_eq!(m.url, None);
        assert_eq!(
            (m.zip.as_deref(), m.zip_sha256.as_deref()),
            (None, None),
            "a manifest cut before zip staging must still parse, with no zip"
        );
        assert_eq!(m.min_os, None);
        assert_eq!(m.team_id, None);
        assert_eq!(m.pub_date, None);
        assert_eq!(m.min_build, None);
        assert_eq!(m.changelog, None);
    }

    #[test]
    fn rejects_newer_schema() {
        let r = Manifest::parse(
            r#"schema = 99
               version = "9.0.0"
               build_number = 999999
               sha256 = "x"
               dmg = "aterm-9.0.0.dmg""#,
        );
        assert!(r.is_err(), "a future schema must be rejected");
    }

    #[test]
    fn full_manifest_round_trips_and_changelog_is_a_literal_block() {
        let m = full();
        let text = m.to_toml().unwrap();
        assert!(
            text.contains("changelog = '''\n### Added\n"),
            "changelog must be a ''' multiline literal, got:\n{text}"
        );
        assert!(
            !text.contains("with \\\"quotes\\\"") && text.contains("with \"quotes\""),
            "literal block must carry the body unescaped, got:\n{text}"
        );
        assert_eq!(
            Manifest::parse(&text).unwrap(),
            m,
            "byte round-trip must be exact"
        );
    }

    #[test]
    fn absent_optionals_are_omitted_from_emission() {
        let m = Manifest {
            schema: 1,
            version: "0.26".into(),
            build_number: 7,
            commit: None,
            dmg: "a.dmg".into(),
            sha256: "x".into(),
            url: None,
            zip: None,
            zip_sha256: None,
            dmg_x86_64: None,
            dmg_x86_64_sha256: None,
            linux_x86_64: None,
            linux_x86_64_sha256: None,
            linux_x86_64_size: None,
            linux_aarch64: None,
            linux_aarch64_sha256: None,
            linux_aarch64_size: None,
            min_os: None,
            team_id: None,
            pub_date: None,
            min_build: None,
            machine_id: None,
            roster_seq: None,
            changelog: None,
        };
        let text = m.to_toml().unwrap();
        for absent in [
            "min_build",
            "commit",
            "url",
            "zip",
            "zip_sha256",
            "linux_x86_64",
            "linux_aarch64",
            "dmg_x86_64",
            "dmg_x86_64_sha256",
            "min_os",
            "team_id",
            "pub_date",
            "changelog",
        ] {
            assert!(
                !text.contains(absent),
                "absent key `{absent}` leaked into:\n{text}"
            );
        }
        assert_eq!(Manifest::parse(&text).unwrap(), m);
    }

    #[test]
    fn rejects_min_build_above_build_number_on_parse_and_emit() {
        let text = r#"schema = 1
version = "0.55"
build_number = 550
dmg = "aterm-0.55.dmg"
sha256 = "x"
min_build = 551
"#;
        let err = Manifest::parse(text).unwrap_err();
        assert!(
            err.contains("min_build 551") && err.contains("build_number 550"),
            "{err}"
        );

        let mut m = full();
        m.min_build = Some(m.build_number + 1);
        let err = m.to_toml().unwrap_err();
        assert!(err.contains("exceeds its build_number"), "{err}");

        m.min_build = Some(m.build_number);
        assert_eq!(
            Manifest::parse(&m.to_toml().unwrap()).unwrap().min_build,
            Some(m.build_number)
        );
    }

    #[test]
    fn changelog_literal_edge_cases_round_trip_exactly() {
        for body in [
            "no trailing newline",
            "ends with a quote'",
            "",
            "\nleading blank\n",
        ] {
            let mut m = full();
            m.changelog = Some(body.into());
            let text = m.to_toml().unwrap();
            assert_eq!(
                Manifest::parse(&text).unwrap().changelog.as_deref(),
                Some(body),
                "body {body:?} must survive emit→parse byte-exactly; emitted:\n{text}"
            );
        }
    }

    #[test]
    fn to_toml_refuses_unrepresentable_changelog() {
        let mut m = full();
        m.changelog = Some("evil ''' body".into());
        assert!(m.to_toml().is_err(), "''' in the body must refuse to emit");
        m.changelog = Some("cr\r\nlf".into());
        assert!(m.to_toml().is_err(), "raw \\r must refuse to emit");
    }
}
