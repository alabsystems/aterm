// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Emit and round-trip verify `aterm-appcast.toml` (`schema = 1`) via [`aterm_update_core::Manifest`].

use std::path::{Path, PathBuf};

use aterm_update_core::Manifest;

use crate::ledger::{Error, Result};

/// Exact manifest asset name fetched by the updater from the channel's latest release.
pub const MANIFEST_ASSET: &str = "aterm-appcast.toml";

/// Detached signature paired with [`MANIFEST_ASSET`] when Tier SIG is active.
pub const MANIFEST_SIG_ASSET: &str = "aterm-appcast.toml.sig";

/// Inputs for assembling a release [`Manifest`].
pub struct ManifestInputs<'a> {
    /// Canonical `MAJOR.MINOR.PATCH` release version (must match tag `v<version>` and DMG name).
    pub version: &'a str,
    /// Verified ledger claim `n` (equals the identity record's `build` and `CFBundleVersion`).
    pub build_number: u64,
    /// Full 40-hex release commit.
    pub commit: &'a str,
    /// Exact DMG asset name in the same release (e.g. `"aterm-0.2.0.dmg"`).
    pub dmg_name: &'a str,
    /// Lowercase-hex SHA-256 of the DMG bytes.
    pub dmg_sha256: &'a str,
    /// Exact zip asset name in the same release (e.g. `"aterm-0.2.0-mac.zip"`).
    pub zip_name: &'a str,
    /// Lowercase-hex SHA-256 of the zip bytes.
    pub zip_sha256: &'a str,
    /// Public update channel `"owner/repo"` used in the manifest `url` field.
    pub repo_slug: &'a str,
    /// `LSMinimumSystemVersion` from the stamped bundle plist.
    pub min_os: &'a str,
    /// Apple Team ID (`""` for the ad-hoc tier, never omitted).
    pub team_id: &'a str,
    /// RFC3339 UTC publish time.
    pub pub_date: &'a str,
    /// Effective apply floor (`max(operator request, newest channel manifest)`).
    pub min_build: Option<u64>,
    /// Hand-written changelog section body (already gated for `'''`).
    pub changelog: &'a str,
}

/// Assemble the [`Manifest`] value, ensuring `changelog` ends with a newline when non-empty.
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
        linux_verifier_x86_64: None,
        linux_verifier_x86_64_sha256: None,
        linux_verifier_x86_64_size: None,
        linux_verifier_aarch64: None,
        linux_verifier_aarch64_sha256: None,
        linux_verifier_aarch64_size: None,
        linux_verifier_interface: None,
        // Stamped after assembly, as a Linux handoff is, by `Manifest::set_windows_artifact`
        // when the cut publishes a Windows zip; `emit` then writes them.
        windows_x86_64: None,
        windows_x86_64_sha256: None,
        windows_x86_64_size: None,
        // Stamped beside the zip by `crate::windows_pin::stamp` when the cut publishes the
        // Windows bootstrap verifier.
        windows_verifier_x86_64: None,
        windows_verifier_x86_64_sha256: None,
        windows_verifier_x86_64_size: None,
        windows_verifier_interface: None,
        dmg_x86_64: None,
        dmg_x86_64_sha256: None,
        min_os: Some(i.min_os.to_string()),
        team_id: Some(i.team_id.to_string()),
        pub_date: Some(i.pub_date.to_string()),
        min_build: i.min_build,
        // Stamped after assembly by `machines::attribute` when the paper master is pinned.
        machine_id: None,
        roster_seq: None,
        changelog: Some(body),
    }
}

/// Serialize `m` to TOML and verify that [`Manifest::parse`] round-trips to `m`.
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

/// Emit and atomically write `<out_dir>/aterm-appcast.toml`, refusing symlink or directory targets.
pub fn write(out_dir: &Path, m: &Manifest) -> Result<PathBuf> {
    let text = emit(m)?;
    let path = out_dir.join(MANIFEST_ASSET);
    if let Ok(meta) = std::fs::symlink_metadata(&path)
        && (meta.file_type().is_symlink() || meta.is_dir())
    {
        return Err(Error::new(format!(
            "write {}: refusing symlink or directory destination",
            path.display()
        )));
    }
    crate::durable::write_atomic_durable(&path, text.as_bytes(), 0o644)
        .map_err(|e| Error::new(format!("write {}: {e}", path.display())))?;
    Ok(path)
}

fn strip_xml_comments(xml: &str) -> Option<std::borrow::Cow<'_, str>> {
    let Some(first) = xml.find("<!--") else {
        return Some(std::borrow::Cow::Borrowed(xml));
    };
    let mut out = String::with_capacity(xml.len());
    out.push_str(&xml[..first]);
    let after = &xml[first + 4..];
    let end = after.find("-->")?;
    let mut rest = &after[end + 3..];
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let end = after.find("-->")?;
        rest = &after[end + 3..];
    }
    out.push_str(rest);
    Some(std::borrow::Cow::Owned(out))
}

fn decode_xml_entities(raw: &str) -> Option<String> {
    if raw.contains('<') {
        return None;
    }
    let Some(first) = raw.find('&') else {
        return Some(raw.to_owned());
    };
    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..first]);
    let mut tail = &raw[first..];
    loop {
        let (entity, ch) = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find(|(entity, _)| tail.starts_with(entity))?;
        out.push(ch);
        tail = &tail[entity.len()..];
        let Some(at) = tail.find('&') else {
            out.push_str(tail);
            break;
        };
        out.push_str(&tail[..at]);
        tail = &tail[at..];
    }
    Some(out)
}

/// Read one `<key>K</key><string>V</string>` (or `<string/>`) value out of plist XML.
pub fn plist_string(plist: &str, key: &str) -> Option<String> {
    let stripped = strip_xml_comments(plist)?;
    let mut search = &*stripped;
    let rest = loop {
        let at = search.find("<key>")?;
        let after_open = &search[at + 5..];
        if let Some(after_key) = after_open
            .strip_prefix(key)
            .and_then(|s| s.strip_prefix("</key>"))
        {
            break after_key.trim_start();
        }
        search = after_open;
    };
    if rest.starts_with("<string/>") {
        return Some(String::new());
    }
    let body = rest.strip_prefix("<string>")?;
    let (raw, _) = body.split_once("</string>")?;
    decode_xml_entities(raw)
}

#[cfg(test)]
#[path = "manifest_out_tests.rs"]
mod tests;
