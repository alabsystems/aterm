// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Manifest emission: `aterm-appcast.toml` (`schema = 1`) through the shared serde
//! `Manifest` type from aterm-update-core, so publisher and client agree by compilation.
//! Every emission is round-tripped through the client's `Manifest::parse` before it ships.

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;

use crate::ledger::{Error, Result};

/// The manifest's exact asset name. The updater fetches it by this name from the channel's
/// latest release, so a renamed asset makes the release invisible to the fleet.
pub const MANIFEST_ASSET: &str = "aterm-appcast.toml";

/// Detached signature paired with [`MANIFEST_ASSET`] when Tier SIG is active.
pub const MANIFEST_SIG_ASSET: &str = "aterm-appcast.toml.sig";

/// Everything the manifest derives from, resolved by the caller (publish.rs) so this
/// module stays pure and fixture-testable.
pub struct ManifestInputs<'a> {
    /// The release version, canonical `MAJOR.MINOR.PATCH`, e.g. "0.2.0". It must equal the
    /// `v<version>` release tag and the `<version>` in the `aterm-<version>.dmg` asset name,
    /// because the client binds all three (`aterm_update_core::Manifest::version`). Not the
    /// ordering key: `build_number` is the apply gate.
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
    /// Exact zip asset name in the same release, e.g. "aterm-0.2.0-mac.zip": the container
    /// the in-app updater stages from (`ditto`, no `hdiutil`). Always emitted; the wire
    /// field stays optional so pre-zip manifests keep parsing.
    pub zip_name: &'a str,
    /// Lowercase-hex SHA-256 of the zip bytes (computed in-process, dmg.rs).
    pub zip_sha256: &'a str,
    /// "owner/repo" for the `url` field: the public update channel
    /// (`[workspace.metadata.aterm] update_channel`) when one is configured, never the
    /// private publish repo, whose `url` 404s for everyone without a credential.
    ///
    /// The credential-less updater (`aterm_update::github::web_container_url_agrees`)
    /// refuses a manifest whose `url` is not exactly
    /// `https://github.com/<slug>/releases/download/v<version>/<dmg_name>` for the tag its
    /// pointer chose; `tools/install.sh` binds it too. The URL is never followed: it binds
    /// the signed manifest to its tag and container. The cut checks the served appcast's
    /// `url` against `aterm_update_core::cdn::release_download_url` (publish.rs), so a
    /// mis-set `update_channel` fails the cut rather than the fleet.
    pub repo_slug: &'a str,
    /// LSMinimumSystemVersion from the stamped bundle plist.
    pub min_os: &'a str,
    /// Apple Team ID (a cut passes `aterm_update_core::pins::APPLE_TEAM_ID`); `""` is the
    /// ad-hoc tier, emitted as an empty string, never omitted.
    pub team_id: &'a str,
    /// RFC3339 UTC publish time.
    pub pub_date: &'a str,
    /// Effective apply floor: max(operator request, newest channel manifest); `None`
    /// (omitted) until the channel first raises a floor.
    pub min_build: Option<u64>,
    /// The hand-written changelog section body, verbatim (already gated for `'''`).
    pub changelog: &'a str,
}

/// Assemble the manifest value. `changelog` gains a trailing newline when missing, so the
/// closing `'''` lands on its own line.
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
        linux_x86_64: None,
        linux_x86_64_sha256: None,
        linux_x86_64_size: None,
        linux_aarch64: None,
        linux_aarch64_sha256: None,
        linux_aarch64_size: None,
        // The retired Intel `aterm-<v>-x86_64.dmg`: the wire keys stay in `Manifest` so
        // published manifests keep parsing, but this cutter never emits them.
        dmg_x86_64: None,
        dmg_x86_64_sha256: None,
        min_os: Some(i.min_os.to_string()),
        team_id: Some(i.team_id.to_string()),
        pub_date: Some(i.pub_date.to_string()),
        min_build: i.min_build,
        // Stamped after assembly by `machines::attribute`: they come from the machine's
        // minted identity and the master-signed roster, inputs this pure assembler does not
        // resolve. With the paper master unpinned (a fork) they stay `None`.
        machine_id: None,
        roster_seq: None,
        changelog: Some(body),
    }
}

/// Serialize and prove the bytes before they can ship: the text must parse back through
/// the client's own `Manifest::parse` to the exact value. Returns the proven bytes.
pub fn emit(m: &Manifest) -> Result<String> {
    let text = m.to_toml().map_err(Error::new)?;
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

/// Emit and write `<out_dir>/aterm-appcast.toml`. Publish uploads these proven bytes
/// verbatim and the post-publish verify byte-compares the download against them.
pub fn write(out_dir: &Path, m: &Manifest) -> Result<PathBuf> {
    let text = emit(m)?;
    let path = out_dir.join(MANIFEST_ASSET);
    std::fs::write(&path, &text)
        .map_err(|e| Error::new(format!("write {}: {e}", path.display())))?;
    Ok(path)
}

/// Read one `<key>K</key><string>V</string>` value out of plist XML: the read-only sibling
/// of bundle.rs's stamp, with the same guard that the `<string>` belongs to this key.
pub fn plist_string(plist: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    let after = plist.find(&key_tag)? + key_tag.len();
    let sstart = after + plist[after..].find("<string>")? + "<string>".len();
    let send = sstart + plist[sstart..].find("</string>")?;
    // No other <key> between ours and the <string>, else the string belongs to a later
    // key (ours holds e.g. <true/>).
    if plist[after..sstart - "<string>".len()].contains("<key>") {
        return None;
    }
    // Inverse of bundle.rs's xml_escape: `&amp;` last, so "&amp;lt;" decodes to "&lt;".
    Some(
        plist[sstart..send]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}
