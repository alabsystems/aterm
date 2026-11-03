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
    /// Verified ledger claim `n` (equals `ATERM_BUILD_NUMBER` and `CFBundleVersion`).
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

fn strip_xml_comments(xml: &str) -> Option<String> {
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let end = after.find("-->")?;
        rest = &after[end + 3..];
    }
    out.push_str(rest);
    Some(out)
}

fn decode_xml_entities(raw: &str) -> Option<String> {
    if raw.contains('<') {
        return None;
    }
    let mut out = String::with_capacity(raw.len());
    let mut tail = raw;
    while let Some(at) = tail.find('&') {
        out.push_str(&tail[..at]);
        tail = &tail[at..];
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
    }
    out.push_str(tail);
    Some(out)
}

/// Read one `<key>K</key><string>V</string>` (or `<string/>`) value out of plist XML.
pub fn plist_string(plist: &str, key: &str) -> Option<String> {
    let stripped = strip_xml_comments(plist)?;
    let key_tag = format!("<key>{key}</key>");
    let after = stripped.find(&key_tag)? + key_tag.len();
    let rest = stripped[after..].trim_start();
    if rest.starts_with("<string/>") {
        return Some(String::new());
    }
    let body = rest.strip_prefix("<string>")?;
    let (raw, _) = body.split_once("</string>")?;
    decode_xml_entities(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_string_requires_adjacent_string_tag_after_key() {
        let plist = "<plist><dict>\n\
            \t<key>Flag</key>\n\t<false/>\n\
            \t<key>Items</key>\n\t<array><string>item-zero</string></array>\n\
            \t<key>Name</key>\n\t<string>a &amp;lt; b</string>\n\
            </dict></plist>";
        assert_eq!(plist_string(plist, "Name").as_deref(), Some("a &lt; b"));
        assert_eq!(plist_string(plist, "Flag"), None);
        assert_eq!(plist_string(plist, "Items"), None);
    }

    /// NEGATIVE CONTROL: `plist_string` must ignore XML comments, decode `<string/>` and
    /// `&quot;`/`&apos;`, and reject unescaped `<` or unknown XML entities; and `write` must
    /// refuse a symlink destination instead of overwriting its target.
    #[test]
    fn plist_string_handles_comments_empty_tags_and_entities_and_write_refuses_symlinks() {
        let plist = "<plist><dict>\n\
            <!-- <key>Team</key><string>shadowed</string> -->\n\
            <key>Team</key>\n\t<string/>\n\
            <key>Quoted</key>\n\t<string>&quot;hi&apos;</string>\n\
            <key>BrokenTag</key>\n\t<string>a < b</string>\n\
            <key>BrokenEnt</key>\n\t<string>a &bogus; b</string>\n\
            </dict></plist>";
        assert_eq!(plist_string(plist, "Team").as_deref(), Some(""));
        assert_eq!(plist_string(plist, "Quoted").as_deref(), Some("\"hi'"));
        assert_eq!(plist_string(plist, "BrokenTag"), None);
        assert_eq!(plist_string(plist, "BrokenEnt"), None);
        assert_eq!(
            plist_string(
                "<!-- unterminated <key>Team</key><string>x</string>",
                "Team"
            ),
            None
        );

        #[cfg(unix)]
        {
            let dir = aterm_tempfile::tempdir().unwrap();
            let victim = dir.path().join("victim.toml");
            std::fs::write(&victim, "keep = true\n").unwrap();
            let link_out = dir.path().join("out");
            std::fs::create_dir_all(&link_out).unwrap();
            std::os::unix::fs::symlink(&victim, link_out.join(MANIFEST_ASSET)).unwrap();
            let m = build(&ManifestInputs {
                version: "0.2.0",
                build_number: 7,
                commit: "0123456789abcdef0123456789abcdef01234567",
                dmg_name: "aterm-0.2.0.dmg",
                dmg_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                zip_name: "aterm-0.2.0-mac.zip",
                zip_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                repo_slug: "alabsystems/aterm",
                min_os: "14.0",
                team_id: "",
                pub_date: "2026-11-03T00:00:00Z",
                min_build: None,
                changelog: "- Fixed manifest write.\n",
            });
            assert!(write(&link_out, &m).is_err());
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep = true\n");

            let clean_out = dir.path().join("clean");
            std::fs::create_dir_all(&clean_out).unwrap();
            let written = write(&clean_out, &m).unwrap();
            let parsed = Manifest::parse(&std::fs::read_to_string(&written).unwrap()).unwrap();
            assert_eq!(parsed, m);
        }
    }
}
