// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The release manifest, the `aterm-appcast.toml` asset on every GitHub release: one serde
//! type the publisher (`aterm-release`, [`Manifest::to_toml`]) and the updater
//! ([`Manifest::parse`]) both compile against.
//!
//! The wire format is `schema = 1` permanently, with parse semantics frozen to what deployed
//! clients accept: absent `schema` ⇒ `0`; unknown keys tolerated (no `deny_unknown_fields`),
//! so an older client reads a manifest that grew a key; every key beyond `version`,
//! `build_number`, `sha256` and `dmg` optional.

use serde::{Deserialize, Serialize};

/// The highest manifest `schema` this build understands; a higher one is rejected (the client
/// stays on its build) rather than misread.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The release manifest attached to a GitHub Release as `aterm-appcast.toml`.
///
/// Field order matters for emission only: serde writes keys in declaration order, and
/// `min_build` and `changelog` come last (one usually absent, the other the lone multiline
/// value). `PartialEq` serves the publisher's round-trip self-check
/// (`Manifest::parse(&m.to_toml()?)? == m`) before any byte is uploaded.
// Skip (propagates to the derived impls): the serde expansion dispatches into toml's generic
// `Deserializer::deserialize_struct`, a cross-crate body the verifier cannot bundle; the
// publish round-trip self-check covers it.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version (the publisher emits `schema = 1`). Absent ⇒ 0.
    #[serde(default)]
    pub schema: u32,
    /// The release version, the workspace `MAJOR.MINOR.0` (e.g. `"0.2.0"`), and an identity
    /// bind on both sides: the client derives it from the release's canonical
    /// `vMAJOR.MINOR.PATCH` tag, refuses a manifest whose `version` differs, and requires the
    /// DMG to be `aterm-<version>.dmg` (`aterm-update/src/github.rs`); the publisher
    /// self-checks the same triple (`aterm-release/src/verify.rs`). Not the ordering key:
    /// selection orders by tag and [`build_number`](Self::build_number) gates the apply. One
    /// version lineage (`VERSIONING.md`): this string, the tag, the DMG name and the source
    /// snapshot are the same number.
    pub version: String,
    /// Monotonic build number: the downgrade gate. Claimed from the append-only
    /// `RELEASES.ledger` (`n = max(tail + 1, unix_now)`) and stamped identically into the
    /// binary (`ATERM_BUILD_NUMBER`), `CFBundleVersion` and here; the publisher's self-check
    /// asserts all three agree. Clients rely only on strict monotonicity, never the scale.
    pub build_number: u64,
    /// Full git commit hash of the source the release was built from, binding the build
    /// number to an exact commit. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The DMG asset's file name within the same release; clients require exactly
    /// `aterm-<version>.dmg`.
    pub dmg: String,
    /// SHA-256 (lowercase hex) of the DMG asset's bytes.
    pub sha256: String,
    /// Absolute browser download URL of the DMG. Never followed, but required: the in-app
    /// updater checks it names the pointer's tag and [`Self::dmg`]
    /// (`aterm-update`'s `web_container_url_agrees`), and `tools/install.sh` binds it too.
    /// Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The ZIP asset's file name, e.g. `"aterm-0.10.0-mac.zip"`: a `ditto -c -k --keepParent`
    /// archive (bytes only, no extended attributes) of the same signed `aterm.app` the DMG
    /// carries. Absent ⇒ None, and the release updates through the DMG.
    ///
    /// The in-app updater stages from the zip because `hdiutil attach` needs a live bootstrap
    /// context (it talks to the `com.apple.hdiejectd` XPC service), and the survivor of a
    /// seamless overlap update is an orphan whose launchd job has exited: every attach there
    /// fails ENXIO. `ditto -x -k` uses no XPC service. The DMG stays the human download.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::zip`]'s bytes. Absent ⇒ None; a zip without a digest
    /// is never staged from, and the client falls back to the DMG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_sha256: Option<String>,
    /// Native Linux raw ELF assets. Each target's name, digest and size are one declaration:
    /// a partial one is refused, not treated as absent. Names are bound to this manifest's
    /// version; URLs are derived from the authenticated source and tag, never read from here.
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
    /// The Intel batteries-included DMG's asset name, e.g. `"aterm-0.47.0-x86_64.dmg"`: the
    /// same signed universal app with the toolchain seed filtered to `x86_64-apple-darwin`.
    /// Absent ⇒ None (an arm64-only cut). A dual-arch DMG came to 97% of
    /// [`crate::RELEASE_ASSET_DOWNLOAD_BOUND`], ~1 GB of it seed the receiving CPU cannot run.
    ///
    /// The bare `aterm-<version>.dmg` stays the canonical arm64 asset, since clients bind that
    /// spelling. No updater downloads this one (updates stage from the zip); only
    /// `tools/install.sh`'s Intel first-install lane does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::dmg_x86_64`]'s bytes. Absent ⇒ None; a name without
    /// a digest is never installed from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64_sha256: Option<String>,
    /// Minimum macOS version the bundle declares (`LSMinimumSystemVersion`),
    /// e.g. `"11.0"`. Display/tooling only. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_os: Option<String>,
    /// Apple Team ID the release is signed under; `""` is the ad hoc tier. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// RFC3339 UTC publish time, e.g. `"2026-07-06T21:29:44Z"`. Display only.
    /// Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,
    /// Optional operator apply floor: clients refuse to stage or apply any build below it,
    /// even a genuine signed one, so the owner can retire a bad release after the fact
    /// (`ship yank`). Ratcheted monotonically client-side (`Floor` in aterm-update); once
    /// raised, the cutter carries the channel's newest floor into every successor. Absent ⇒
    /// None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_build: Option<u64>,
    /// The id of the machine that signed this release, e.g. `"m3"`. Inside the signed bytes,
    /// so a signature by one machine cannot be relabelled as another; a stolen key claiming
    /// another id is refused by the roster's id-to-key map (`roster::Attribution::bind`).
    ///
    /// Absent ⇒ None. A client with an armed paper master refuses an unattributed release;
    /// one with an unpinned master ignores the field. It is never written to
    /// `RELEASES.ledger`, whose parser hard-fails on any line that is not exactly two fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// The `roster_seq` of the roster that authorized [`Self::machine_id`] at cut time, so an
    /// old roster cannot be paired with a new release (or the reverse) after a revocation.
    /// Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_seq: Option<u64>,
    /// The release's CHANGELOG.md section body, verbatim, shown by the updater's status query
    /// and the Software Update window. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

impl Manifest {
    /// Select a native target only when all its signed identity fields agree. `None` means
    /// this release does not ship that target, not "up to date".
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
    // Skip: as `Manifest`; every error path returns `Err`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = aterm_toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Wire invariants shared by parsing and emission. A manifest may retire older builds but
    /// cannot demand one newer than itself, which would wedge the ratcheted client floor above
    /// every published successor.
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

    /// Serialize to the published TOML shape: serde's serializer for everything but
    /// `changelog`, which is rendered by hand as a `'''` multiline literal (diffable on the
    /// releases page, no escaping layer). Refuses a body a literal cannot represent: one
    /// containing `'''`, or a control character other than `\n`/`\t`.
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
        // The changelog goes last. TOML trims a newline right after the opening `'''`, so the
        // block parses back to the body byte for byte, with or without a trailing newline.
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

    /// A fully populated manifest for the round-trip tests.
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
            dmg_x86_64: None,
            dmg_x86_64_sha256: None,
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
            // Bypass the validating emitter to build an invalid wire input.
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

    /// Absent `schema` ⇒ 0, unknown keys tolerated, optional keys default to None.
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

    /// A newer schema is rejected rather than misread.
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

    /// Emit then parse reproduces the value exactly, and the changelog is a `'''` literal
    /// block carrying raw `#`, `"` and `\`.
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

    /// Absent optional keys are omitted from the emission, and the minimal shape round-trips.
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

    /// A `min_build` (a `cut --min-build` or `yank` cut) survives the round trip.
    #[test]
    fn min_build_round_trips_when_set() {
        let mut m = full();
        m.min_build = Some(1_783_354_740);
        let text = m.to_toml().unwrap();
        assert!(text.contains("min_build = 1783354740"), "got:\n{text}");
        assert_eq!(
            Manifest::parse(&text).unwrap().min_build,
            Some(1_783_354_740)
        );
    }

    /// The zip pair round-trips, and a manifest without it stays valid.
    #[test]
    fn zip_container_round_trips_and_stays_optional() {
        let m = full();
        let text = m.to_toml().unwrap();
        assert!(
            text.contains("zip = \"aterm-0.26-mac.zip\"") && text.contains("zip_sha256 = "),
            "the zip name/digest pair must be emitted next to the DMG's, got:\n{text}"
        );
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back.zip, m.zip);
        assert_eq!(back.zip_sha256, m.zip_sha256);

        // Without the pair: valid, DMG untouched.
        let mut dmg_only = full();
        dmg_only.zip = None;
        dmg_only.zip_sha256 = None;
        let text = dmg_only.to_toml().unwrap();
        assert!(!text.contains("zip"), "got:\n{text}");
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back, dmg_only);
        assert_eq!(back.dmg, "aterm-0.26.dmg");
    }

    /// The Intel DMG name/digest pair round-trips, and a manifest without it stays valid and
    /// emits neither key.
    #[test]
    fn x86_64_dmg_pair_round_trips_and_stays_optional() {
        let mut m = full();
        m.dmg_x86_64 = Some("aterm-0.47.0-x86_64.dmg".into());
        m.dmg_x86_64_sha256 =
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into());
        let text = m.to_toml().unwrap();
        assert!(
            text.contains("dmg_x86_64 = \"aterm-0.47.0-x86_64.dmg\"")
                && text.contains("dmg_x86_64_sha256 = "),
            "the Intel DMG name/digest pair must be emitted, got:\n{text}"
        );
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back.dmg_x86_64, m.dmg_x86_64);
        assert_eq!(back.dmg_x86_64_sha256, m.dmg_x86_64_sha256);

        // Without the pair: valid, neither key emitted, canonical DMG untouched.
        let plain = full();
        let text = plain.to_toml().unwrap();
        assert!(!text.contains("dmg_x86_64"), "got:\n{text}");
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back, plain);
        assert_eq!(back.dmg, "aterm-0.26.dmg");
    }

    /// A floor above the manifest's own build is refused on parse and emit: clients persist
    /// floors monotonically, so accepting it could poison every later selection.
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

        // Negative control: `min_build == build` is valid.
        m.min_build = Some(m.build_number);
        assert_eq!(
            Manifest::parse(&m.to_toml().unwrap()).unwrap().min_build,
            Some(m.build_number)
        );
    }

    /// A body with no trailing newline (closing `'''` right after the text) and one ending in
    /// a quote (TOML allows up to two content quotes at the delimiter) round-trip exactly.
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

    /// Bodies a multiline literal cannot represent are refused at emission, never mangled.
    #[test]
    fn to_toml_refuses_unrepresentable_changelog() {
        let mut m = full();
        m.changelog = Some("evil ''' body".into());
        assert!(m.to_toml().is_err(), "''' in the body must refuse to emit");
        m.changelog = Some("cr\r\nlf".into());
        assert!(m.to_toml().is_err(), "raw \\r must refuse to emit");
    }
}
