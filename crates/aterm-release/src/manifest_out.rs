// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manifest emission (release spec §4, §6 `manifest_out.rs`): emit
//! `aterm-appcast.toml` (`schema = 1`, permanently) through the shared serde
//! `Manifest` type from aterm-update-core, so publisher and client agree by
//! compilation. Every emission is byte round-tripped through the client's
//! `Manifest::parse` AND the vendored copy of the exact v0.25 struct — the
//! frozen fleet parser is what must accept these bytes, not our rewrite.

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;

use crate::ledger::{Error, LEDGER_FLOOR, Result};

/// The manifest's exact asset name. Load-bearing on the CLIENT side: the
/// updater's release-selection rule is "newest non-draft release carrying
/// `aterm-appcast.toml`" (github.rs), so a renamed asset makes the release
/// invisible to the whole fleet.
pub const MANIFEST_ASSET: &str = "aterm-appcast.toml";

/// Detached signature paired with [`MANIFEST_ASSET`] when Tier SIG is active.
pub const MANIFEST_SIG_ASSET: &str = "aterm-appcast.toml.sig";

/// Deterministic historical name used by the single-head channel migration.
/// Renaming metadata preserves the exact bytes while removing old manifests
/// from the client's exact-name discovery surface.
pub fn archived_manifest_asset(tag: &str) -> String {
    format!("aterm-appcast-{tag}.toml")
}

/// Historical signature name paired with [`archived_manifest_asset`].
pub fn archived_manifest_signature_asset(tag: &str) -> String {
    format!("{}.sig", archived_manifest_asset(tag))
}

/// Everything the §4 field set derives from — all resolved by the caller
/// (publish.rs), so this module stays pure and fixture-testable.
pub struct ManifestInputs<'a> {
    /// The release version, canonical `MAJOR.MINOR.PATCH`, e.g. "0.2.0" —
    /// load-bearing. It must equal the canonical `vMAJOR.MINOR.PATCH` release tag and
    /// must be the `<version>` in the `aterm-<version>.dmg` asset name, because
    /// the deployed client binds all three (see the doc on
    /// `aterm_update_core::Manifest::version`). Not the ordering key:
    /// `build_number` is the apply gate.
    pub version: &'a str,
    /// The verified ledger claim `n` — must equal the binary's
    /// ATERM_BUILD_NUMBER and the plist's CFBundleVersion (self-checked).
    pub build_number: u64,
    /// Full 40-hex release commit (the claim commit for a real cut).
    pub commit: &'a str,
    /// Exact DMG asset name in the same release, e.g. "aterm-0.2.0.dmg".
    pub dmg_name: &'a str,
    /// Lowercase-hex SHA-256 of the DMG bytes (computed in-process, dmg.rs).
    pub dmg_sha256: &'a str,
    /// "owner/repo" — feeds the informational-but-install.sh-load-bearing
    /// `url` field (that script greps the URL straight out of the manifest).
    pub repo_slug: &'a str,
    /// LSMinimumSystemVersion from the STAMPED bundle plist (spec §4).
    pub min_os: &'a str,
    /// Apple Team ID; `""` = the ad-hoc tier (the shipped default) — emitted
    /// as an empty string, never omitted, per the §4 example.
    pub team_id: &'a str,
    /// RFC3339 UTC publish time.
    pub pub_date: &'a str,
    /// Effective apply floor: max(operator request, newest channel manifest).
    /// `None` remains omitted until the channel first raises a floor.
    pub min_build: Option<u64>,
    /// The hand-written changelog section body, verbatim (already gated for
    /// `'''` long before this point).
    pub changelog: &'a str,
}

/// Assemble the §4 manifest value. `changelog` gains a trailing newline (when
/// missing) so the closing `'''` lands on its own line — the exact shape every
/// v0.25 manifest shipped with (gen-appcast.sh's `printf '%s\n'`).
pub fn build(i: &ManifestInputs<'_>) -> Manifest {
    let mut body = i.changelog.to_string();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    Manifest {
        schema: 1,
        version: i.version.to_string(),
        build_number: i.build_number,
        commit: Some(i.commit.to_string()),
        dmg: i.dmg_name.to_string(),
        sha256: i.dmg_sha256.to_string(),
        url: Some(format!(
            "https://github.com/{}/releases/download/v{}/{}",
            i.repo_slug, i.version, i.dmg_name
        )),
        min_os: Some(i.min_os.to_string()),
        team_id: Some(i.team_id.to_string()),
        pub_date: Some(i.pub_date.to_string()),
        min_build: i.min_build,
        changelog: Some(body),
    }
}

/// Serialize + prove the bytes BEFORE they can ship: the emitted text must
/// round-trip through the shared `Manifest::parse` back to the exact value,
/// AND parse under the vendored v0.25 struct (the frozen fleet parser) with
/// every v0.25-load-bearing field intact. Returns the proven bytes.
pub fn emit(m: &Manifest) -> Result<String> {
    let text = m.to_toml().map_err(Error::new)?;
    // Round-trip through the CLIENT's parser (the same type the v0.26 updater
    // compiles): an emission bug can never ship bytes the client reads back
    // differently (spec decision 7).
    let back = Manifest::parse(&text).map_err(|e| {
        Error::new(format!(
            "emitted manifest does not parse under the shared type: {e}"
        ))
    })?;
    if back != *m {
        return Err(Error::new(
            "emitted manifest does not round-trip byte-faithfully through \
             Manifest::parse — refusing to publish"
                .to_string(),
        ));
    }
    v025_check(&text)?;
    Ok(text)
}

/// Emit + write `dist/aterm-appcast.toml`. The bytes on disk are the proven
/// bytes — publish uploads this file verbatim and the post-publish verify
/// byte-compares the download against it.
pub fn write(out_dir: &Path, m: &Manifest) -> Result<PathBuf> {
    let text = emit(m)?;
    let path = out_dir.join(MANIFEST_ASSET);
    std::fs::write(&path, &text)
        .map_err(|e| Error::new(format!("write {}: {e}", path.display())))?;
    Ok(path)
}

/// The v0.25 bridge proof, run at publish time on the REAL bytes (spec §7
/// step 4): parse under the vendored v0.25 struct and assert every hard gate
/// the deployed fleet enforces — schema ≤ 1, the required field set non-empty,
/// and `build_number` strictly above the last v0.25-published build. The same
/// vendored struct is independently exercised in tests/bridge_v025.rs.
pub fn v025_check(text: &str) -> Result<()> {
    let m = v025::Manifest::parse(text).map_err(|e| {
        Error::new(format!(
            "emitted manifest is REJECTED by the frozen v0.25 parser — the deployed \
             fleet could not stage this release: {e}"
        ))
    })?;
    if m.version.is_empty() || m.dmg.is_empty() || m.sha256.is_empty() {
        return Err(Error::new(
            "v0.25 bridge check: a required field (version/dmg/sha256) is empty".to_string(),
        ));
    }
    if m.build_number <= LEDGER_FLOOR {
        return Err(Error::new(format!(
            "v0.25 bridge check: build_number {} is not above the v0.25 floor \
             {LEDGER_FLOOR} — deployed clients would refuse to stage it",
            m.build_number
        )));
    }
    // Field-for-field agreement between the two parsers on every key BOTH
    // understand: a divergence here means the same bytes read differently on
    // an updated client vs the deployed fleet — exactly the bridge bug this
    // release must be structurally unable to ship.
    let shared = Manifest::parse(text)
        .map_err(|e| Error::new(format!("v0.25 bridge check: shared parse failed: {e}")))?;
    let agree = m.schema == shared.schema
        && m.version == shared.version
        && m.build_number == shared.build_number
        && m.commit == shared.commit
        && m.sha256 == shared.sha256
        && m.dmg == shared.dmg
        && m.min_build == shared.min_build
        && m.changelog == shared.changelog;
    if !agree {
        return Err(Error::new(
            "v0.25 bridge check: the frozen v0.25 parser and the shared type read \
             the SAME bytes differently — refusing to publish"
                .to_string(),
        ));
    }
    Ok(())
}

/// Read one `<key>K</key><string>V</string>` value out of plist XML — the
/// read-only sibling of bundle.rs's stamp (same guard: the `<string>` must
/// belong to THIS key). Feeds `min_os` (LSMinimumSystemVersion) and the
/// self-check's sealed CFBundleVersion read.
pub fn plist_string(plist: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let after = plist.find(&key_tag)? + key_tag.len();
    let sstart = after + plist[after..].find("<string>")? + "<string>".len();
    let send = sstart + plist[sstart..].find("</string>")?;
    // Guard: no other <key> between ours and the <string> — otherwise the
    // string belongs to a later key (ours holds e.g. <true/>).
    if plist[after..sstart - "<string>".len()].contains("<key>") {
        return None;
    }
    // Reverse of bundle.rs's xml_escape (lt/gt first, amp LAST — the inverse
    // order of escaping, so "&amp;lt;" decodes correctly).
    Some(
        plist[sstart..send]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// The EXACT `Manifest` the deployed v0.25 fleet parses releases with —
/// vendored verbatim from v0.25's `crates/aterm-update/src/manifest.rs`
/// (struct + `SUPPORTED_SCHEMA` + `parse` only; the local-marker types are
/// irrelevant to the wire format). DO NOT "sync" this with the shared type:
/// its whole value is being FROZEN — it stands in for binaries already on
/// users' machines, which no commit can update.
pub mod v025 {
    use serde::Deserialize;

    pub const SUPPORTED_SCHEMA: u32 = 1;

    #[derive(Debug, Clone, Deserialize)]
    pub struct Manifest {
        #[serde(default)]
        pub schema: u32,
        pub version: String,
        pub build_number: u64,
        #[serde(default)]
        pub commit: Option<String>,
        pub sha256: String,
        pub dmg: String,
        #[serde(default)]
        pub min_build: Option<u64>,
        #[serde(default)]
        pub changelog: Option<String>,
    }

    impl Manifest {
        pub fn parse(text: &str) -> Result<Self, String> {
            let m: Manifest = toml::from_str(text).map_err(|e| format!("parse manifest: {e}"))?;
            if m.schema > SUPPORTED_SCHEMA {
                return Err(format!(
                    "manifest schema {} is newer than supported ({SUPPORTED_SCHEMA}); upgrade aterm",
                    m.schema
                ));
            }
            Ok(m)
        }
    }
}
