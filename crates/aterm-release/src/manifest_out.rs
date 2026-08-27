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

use crate::ledger::{Error, Result};

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
    /// Exact zip asset name in the same release, e.g. "aterm-0.2.0-mac.zip" —
    /// the container the in-app updater stages from (`ditto`, no `hdiutil`; see
    /// `aterm_update_core::Manifest::zip`). Always emitted by this cutter; the
    /// wire field stays optional so pre-zip manifests keep parsing.
    pub zip_name: &'a str,
    /// Lowercase-hex SHA-256 of the zip bytes (computed in-process, dmg.rs).
    pub zip_sha256: &'a str,
    /// "owner/repo" for the `url` field — this must be the **public update
    /// channel** (`[workspace.metadata.aterm] update_channel`) whenever one is
    /// configured, NOT the private publish repo.
    ///
    /// The same manifest bytes are attached to both the private release and the
    /// mirrored public one, so this single string has to name the repository a
    /// reader can actually fetch from. Naming the private repo produced a public
    /// appcast whose `url` 404s for everyone without a credential.
    ///
    /// Nothing in the shipping trust path consumes it: the Rust client's
    /// `Manifest` (crates/aterm-update/src/manifest.rs) has no `url` field at
    /// all and downloads via the release's own asset API URL, and `install.sh`
    /// reads `version`/`dmg`/`sha256`/`team_id`/`min_os` and likewise fetches by
    /// asset id. (An older comment here claimed install.sh grepped this URL; it
    /// does not, and has not for some time.) It is retained because it is part
    /// of the frozen v0.25 manifest surface `emit` round-trips against, and
    /// because it is what a human reading the appcast will click.
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
        zip: Some(i.zip_name.to_string()),
        zip_sha256: Some(i.zip_sha256.to_string()),
        // RETIRED 2026-08-26: the Intel `aterm-<v>-x86_64.dmg` variant. The two
        // wire keys stay in the shared `Manifest` type so already-published
        // manifests keep parsing on every client, but this cutter never emits
        // them again — absent keys are byte-for-byte the pre-pair manifest.
        dmg_x86_64: None,
        dmg_x86_64_sha256: None,
        min_os: Some(i.min_os.to_string()),
        team_id: Some(i.team_id.to_string()),
        pub_date: Some(i.pub_date.to_string()),
        min_build: i.min_build,
        // ATTRIBUTION is stamped AFTER assembly, by `machines::attribute`, not here.
        //
        // Not because it is an afterthought, but because it is a different KIND of fact:
        // every field above is derived from the build, while these two are derived from
        // the machine's own minted identity and the master-signed roster that authorizes
        // it — inputs this pure, fixture-testable assembler deliberately does not resolve.
        // With the paper master unpinned (the shipped state) they stay `None`, and the
        // emitted bytes are byte-identical to what this cutter has always produced.
        machine_id: None,
        roster_seq: None,
        changelog: Some(body),
    }
}

/// Serialize + prove the bytes BEFORE they can ship: the emitted text must
/// round-trip through the shared `Manifest::parse` back to the exact value.
/// Returns the proven bytes.
///
/// This used to ALSO parse the bytes under a struct vendored verbatim from
/// v0.25, standing in for binaries already on users' machines. That bridge is
/// gone: the two-component lineage it protected (v0.25-v0.61) is retired and
/// cannot elect a `vMAJOR.MINOR.PATCH` release at all, so the check constrained
/// the emitter on behalf of clients that could never install the result. The
/// client-side round-trip above is the real gate.
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
