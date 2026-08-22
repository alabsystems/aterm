// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The **shared release manifest** — the `aterm-appcast.toml` asset attached to
//! every GitHub release. ONE serde type both sides of the wire compile against:
//! the publisher (`crates/aterm-release`) emits it via [`Manifest::to_toml`] and
//! the updater client parses it via [`Manifest::parse`], so publisher/client field
//! agreement is enforced by the compiler instead of being hand-synced with a shell
//! script (the old `tools/gen-appcast.sh` contract this replaces).
//!
//! The wire format is `schema = 1` **permanently**, and parse semantics are frozen
//! to what the already-deployed v0.25 fleet accepts:
//! * absent `schema` ⇒ `0` (pre-schema manifests still parse);
//! * unknown keys are tolerated — deliberately NO `deny_unknown_fields`, so an
//!   older client can read a newer same-schema manifest that grew a key;
//! * every key beyond the load-bearing core (`version`, `build_number`, `sha256`,
//!   `dmg`) is optional and defaults to `None`.

use serde::{Deserialize, Serialize};

/// The highest manifest `schema` version this build understands. A manifest
/// declaring a higher schema is from a newer format we can't safely interpret, so
/// we reject it (the client stays on its current build) rather than misread it.
pub const SUPPORTED_SCHEMA: u32 = 1;

/// The release manifest attached to a GitHub Release as `aterm-appcast.toml`.
///
/// Field ORDER here is meaningful for emission only: serde's TOML serializer
/// writes keys in declaration order, and this order matches the published
/// artifact layout (docs/RELEASING.md §manifest) — `min_build` and `changelog`
/// last, since one is usually absent and the other is the lone multiline value.
/// Parsing is order-insensitive as always.
///
/// `PartialEq`/`Eq` exist for the publish-time round-trip self-check: the
/// publisher asserts `Manifest::parse(&m.to_toml()?)? == m` before any byte is
/// uploaded, so an emission bug can never ship a manifest the client (this very
/// type) would read back differently.
// Skip (propagates to the DERIVE-generated impls — the checker consults the
// impl subject for macro-generated items): the serde expansion dispatches
// into pinned toml 0.8's generic `Deserializer::deserialize_struct`, a
// cross-crate body the verifier cannot bundle (rung-3 instantiation
// verification is the eventual proper discharge). Audited via the publish
// round-trip self-check documented above + the frozen v0.25 fixture parse on
// every cut. Verify-only; behavior unchanged.
#[cfg_attr(trust_verify, trust::skip)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest format version (the publisher emits `schema = 1`). Absent ⇒ 0.
    #[serde(default)]
    pub schema: u32,
    /// The release version — the workspace `MAJOR.MINOR.0` version with any nonzero patch
    /// reset to 0, e.g. `"0.2.0"` —
    /// **load-bearing, an identity bind on both sides.** The client derives the
    /// expected value from the release's canonical `vMAJOR.MINOR.PATCH` tag and
    /// refuses any manifest whose `version` differs, then requires the DMG asset
    /// to be exactly `aterm-<version>.dmg` (`aterm-update/src/github.rs`,
    /// `fetch_authoritative_release` / `authoritative_dmg_index`). The publisher
    /// self-checks the same triple at cut time (`aterm-release/src/verify.rs`).
    ///
    /// It is NOT the ordering key and never was: release *selection* orders by
    /// the numeric tag and [`build_number`](Self::build_number) is the *apply*
    /// gate. Do not treat this string as display-only — a scheme change that
    /// alters the tag shape must change this field in lockstep. There is exactly
    /// ONE version lineage: this string, the tag, the DMG name and the source
    /// snapshot are all the same number (see `VERSIONING.md`). Retired
    /// two-component releases (`"0.25"`..`"0.61"`) stay published as archive
    /// history and are skipped by every client.
    pub version: String,
    /// Monotonic build number — THE downgrade gate. Claimed from the append-only
    /// `RELEASES.ledger` (`n = max(tail + 1, unix_now)`, epoch-scale) and stamped
    /// identically into the binary (`ATERM_BUILD_NUMBER`), the bundle's
    /// `CFBundleVersion`, and here; the publisher's self-check asserts all three
    /// agree. Clients rely only on strict monotonicity, never on the scale.
    pub build_number: u64,
    /// Full git commit hash of the source the release was built from. Binds the
    /// build number to an exact source commit, so a staged (and later running)
    /// build is checkable against the repo. Absent ⇒ None (a hand-written or
    /// pre-field manifest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The DMG asset's file name within the same release, e.g. `"aterm-0.26.dmg"`.
    /// The in-app client resolves the download through the releases API by this
    /// name (works for private repos where browser URLs need auth).
    pub dmg: String,
    /// SHA-256 (lowercase hex) of the DMG asset's bytes.
    pub sha256: String,
    /// Absolute browser download URL of the DMG. Informational for the API-driven
    /// in-app client (which resolves by `dmg` name), but load-bearing for
    /// `tools/install.sh`, which greps it straight out of the manifest — keep
    /// emitting it so that script stays byte-compatible. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The ZIP asset's file name within the same release, e.g.
    /// `"aterm-0.10.0-mac.zip"` — a `ditto -c -k --sequesterRsrc --keepParent`
    /// archive of the very same signed `aterm.app` the DMG carries. Absent ⇒ None
    /// (a manifest cut before zip staging existed); such a release still updates
    /// through the DMG.
    ///
    /// WHY A SECOND CONTAINER FOR THE SAME BUNDLE: `hdiutil attach` needs a live
    /// bootstrap context — DiskImages registers with the `com.apple.hdiejectd` XPC
    /// service — and the survivor of a seamless overlap update is a fork-child
    /// whose launchd job has exited, i.e. an orphan holding a bootstrap context
    /// for a dead job. Every attach from there fails ENXIO ("Device not
    /// configured"), so the whole fleet stops updating one handoff after it starts
    /// using handoffs. `ditto -x -k` speaks to no XPC service and therefore works
    /// from ANY process context. The DMG stays the human download; the zip is what
    /// the in-app updater stages from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::zip`]'s bytes. Absent ⇒ None. A zip name
    /// without a digest is never staged from: there would be nothing to check the
    /// downloaded bytes against, so the client falls back to the DMG.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_sha256: Option<String>,
    /// The INTEL batteries-included DMG's asset name within the same release,
    /// e.g. `"aterm-0.47.0-x86_64.dmg"` — the same signed, notarized universal
    /// app with the toolchain seed filtered to `x86_64-apple-darwin` artifacts.
    /// Present only on releases whose seed covers that triple (atpkg index ≥ 14).
    /// Absent ⇒ None: the release predates the per-arch DMG pair (or was cut
    /// deliberately arm64-only), and Intel installs take the lean zip as before.
    ///
    /// WHY A SECOND DMG: the dual-arch fat DMG measured 2,090,384,004 bytes on
    /// v0.46.0 — 97.3% of [`crate::RELEASE_ASSET_DOWNLOAD_BOUND`] — and every
    /// download carried ~0.9–1.1 GB of seed tarballs the receiving CPU can never
    /// execute. Splitting per arch is the only durable headroom under that bound.
    ///
    /// The bare `aterm-<version>.dmg` stays the canonical (arm64-seeded) asset,
    /// because the deployed fleet binds that exact spelling
    /// (`aterm-update/src/github.rs` `authoritative_dmg_index`, and
    /// `tools/install.sh`'s identity bind) — this field is ADDITIVE, riding the
    /// frozen parser's deliberate unknown-key tolerance (see the module doc).
    /// NO updater ever downloads it: updates stage from the lean zip; only
    /// `tools/install.sh`'s Intel first-install lane elects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64: Option<String>,
    /// SHA-256 (lowercase hex) of [`Self::dmg_x86_64`]'s bytes. Absent ⇒ None.
    /// A name without a digest is never installed from (nothing to check the
    /// download against) — the same doctrine as [`Self::zip_sha256`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmg_x86_64_sha256: Option<String>,
    /// Minimum macOS version the bundle declares (`LSMinimumSystemVersion`),
    /// e.g. `"11.0"`. Display/tooling only. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_os: Option<String>,
    /// Apple Team ID the release is signed under; `""` = the ad-hoc tier (the
    /// shipped default). The Dev-ID signing hook fills the real Team ID when the
    /// owner has a certificate. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// RFC3339 UTC publish time, e.g. `"2026-07-06T21:29:44Z"`. Display only.
    /// Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pub_date: Option<String>,
    /// Optional operator **apply floor**: clients refuse to stage/apply ANY build
    /// whose `build_number` is below this, even a genuine signed one. Lets the
    /// owner retire a bad-but-genuine release after the fact (`ship yank`) — a
    /// yank a silent updater can honor without a signed channel. Ratcheted
    /// monotonically client-side (`Floor` in aterm-update). Absent ⇒ None. Once
    /// raised, the release cutter carries the channel's newest floor forward into
    /// every successor, even when `cut --min-build N` is omitted. (F5)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_build: Option<u64>,
    /// ATTRIBUTION: the id of the MACHINE that signed this release, e.g. `"m3"`.
    ///
    /// The owner's requirement is "I can track which computer does what", and this is
    /// where a verifier reads the answer. It is deliberately INSIDE the signed bytes: a
    /// genuine signature by one machine therefore cannot be relabelled as another, because
    /// changing this string changes the bytes the signature covers. The converse — a
    /// stolen key claiming somebody else's id — is refused by the roster, which maps id to
    /// public key (`roster::Attribution::bind`).
    ///
    /// Absent ⇒ None, which is what every release cut before the roster existed carries.
    /// A client with an ARMED paper master refuses an unattributed release; a client with
    /// an unpinned one ignores this field entirely, which is what makes adding it
    /// backward-compatible.
    ///
    /// It is NOT recorded in `RELEASES.ledger`: that file's parser hard-fails on any
    /// non-comment line that is not exactly two whitespace-separated fields, and it is the
    /// append-only ordering root of the entire update fleet. Attribution lands here, in
    /// the cut journal, and in the roster — never there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// The `roster_seq` of the machine roster that authorized [`Self::machine_id`] at cut
    /// time — the cross-check that stops an old roster being paired with a new release (or
    /// a new roster with an old one) after a machine has been revoked. Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_seq: Option<u64>,
    /// Human-readable "what changed" notes — the hand-written CHANGELOG.md
    /// section body, verbatim. Surfaced by the in-app updater's status query +
    /// the Software Update window so the user sees what a staged update brings.
    /// Absent ⇒ None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
}

impl Manifest {
    /// Parse a manifest from TOML text, rejecting a schema this build is too old
    /// to understand.
    // Skip: the serde derive dispatches into pinned `toml 0.8`'s
    // `Deserializer::deserialize_struct` — a cross-crate generic body the
    // verifier cannot bundle (rung-3 instantiation verification is the
    // eventual proper discharge). Audited: the parser is exercised on every
    // release cut against both the shared type AND the frozen v0.25 fixture,
    // and every error path returns `Err`, never panics. Verify-only.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
        m.validate()?;
        Ok(m)
    }

    /// Wire invariants shared by parsing and publisher emission. In particular,
    /// a manifest may retire older builds, but it cannot demand a build newer
    /// than itself: accepting that impossible state would let one bad appcast
    /// wedge the ratcheted client floor beyond every published successor.
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
        Ok(())
    }

    /// Serialize to the published TOML shape.
    ///
    /// Everything except `changelog` goes through serde's TOML serializer (basic
    /// `"…"` strings, declaration-order keys, `None` keys omitted). The changelog
    /// is rendered BY HAND as a `'''` multiline literal: serde would emit it as a
    /// single basic string with `\n`/`\"` escapes, but the appcast has always
    /// carried it as a literal block — human-diffable on the releases page and
    /// byte-faithful for hand-written markdown (no escaping layer to get wrong).
    ///
    /// Fails closed on a body a multiline literal cannot represent: a `'''`
    /// inside the body (no escape exists inside a literal), or a control
    /// character other than `\n`/`\t` (TOML forbids them raw). The release
    /// changelog gate already aborts on `'''` with a line number long before
    /// emission; this is the emitter's own last line of defense so a bad body can
    /// never silently produce a manifest that parses back differently.
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
        // Serialize the scalar head without the changelog, then append the
        // changelog as the final key. A leading newline right after the opening
        // `'''` is trimmed by TOML, so the emitted block parses back to the body
        // byte-for-byte — including a body with no trailing newline (the closing
        // delimiter then sits directly after the last character, which is valid).
        let mut head = self.clone();
        head.changelog = None;
        let mut out = toml::to_string(&head).map_err(|e| format!("serialize manifest: {e}"))?;
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

    /// A fully populated v0.26-shape manifest for the round-trip tests.
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

    /// Frozen v0.25 parse semantics: absent `schema` ⇒ 0, unknown keys tolerated
    /// (no deny_unknown_fields), optional keys default to None.
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

    /// A manifest from a newer format than this build understands is rejected
    /// (the client stays put) rather than silently misread.
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

    /// The publish-time contract: emit → parse must reproduce the value exactly,
    /// and the changelog must be rendered as a `'''` multiline literal (raw `#`,
    /// `"` and `\` inside — no escaping layer).
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

    /// Absent optional keys are OMITTED from the emission (the v0.25 fleet never
    /// sees a `min_build` unless the operator ratchets one), and the minimal
    /// shape still round-trips.
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

    /// `min_build` present (a `cut --min-build` / `yank` cut) is emitted and read
    /// back — the operator yank must survive the round trip.
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

    /// The zip container is OPTIONAL on the wire in both directions: a manifest
    /// that carries one must round-trip it, and one that does not must still be a
    /// perfectly valid manifest (that is what keeps every already-published
    /// release installable after zip staging shipped).
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

        // The same manifest without the pair is valid, and its DMG is untouched.
        let mut dmg_only = full();
        dmg_only.zip = None;
        dmg_only.zip_sha256 = None;
        let text = dmg_only.to_toml().unwrap();
        assert!(!text.contains("zip"), "got:\n{text}");
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back, dmg_only);
        assert_eq!(back.dmg, "aterm-0.26.dmg");
    }

    /// The Intel DMG pair is OPTIONAL on the wire in both directions, exactly
    /// like the zip: a manifest that names one must round-trip the name/digest
    /// pair byte-exactly, and a manifest without it must stay valid and emit
    /// NEITHER key — that absence is what keeps every already-published release
    /// (and any deliberately arm64-only cut) parsing unchanged on the frozen
    /// v0.25 fleet, whose unknown-key tolerance is the entire compatibility
    /// story for these fields.
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

        // Absent pair: valid manifest, neither key emitted, canonical DMG untouched.
        let plain = full();
        let text = plain.to_toml().unwrap();
        assert!(!text.contains("dmg_x86_64"), "got:\n{text}");
        let back = Manifest::parse(&text).unwrap();
        assert_eq!(back, plain);
        assert_eq!(back.dmg, "aterm-0.26.dmg");
    }

    /// A floor above the manifest's own build is not merely unusable: clients
    /// persist floors monotonically, so accepting it can poison every later
    /// selection. Both read and write sides must reject the impossible state.
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

        // Negative control: the boundary itself is valid. `min_build == build`
        // can retire every predecessor while still leaving this successor usable.
        m.min_build = Some(m.build_number);
        assert_eq!(
            Manifest::parse(&m.to_toml().unwrap()).unwrap().min_build,
            Some(m.build_number)
        );
    }

    /// Literal-block edge cases: a body with no trailing newline (closing `'''`
    /// lands right after the text) and a body ending in a single quote (four
    /// quotes in a row — TOML allows up to two content quotes at the delimiter).
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

    /// Bodies a multiline literal cannot represent are refused at emission —
    /// never silently mangled (decision: hard abort beats silent collapse).
    #[test]
    fn to_toml_refuses_unrepresentable_changelog() {
        let mut m = full();
        m.changelog = Some("evil ''' body".into());
        assert!(m.to_toml().is_err(), "''' in the body must refuse to emit");
        m.changelog = Some("cr\r\nlf".into());
        assert!(m.to_toml().is_err(), "raw \\r must refuse to emit");
    }
}
