// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded release discovery when the channel head has no native artifact.
//!
//! A catalog is a list of locations, NEVER signing authority. Callers must first
//! authenticate the newest policy/roster, retain its revocations and minimum-build
//! floor, then verify each candidate's appcast under that authority. A missing
//! platform can be skipped; a bad signature cannot. All artifact URLs are derived
//! independently with `cdn::release_download_url`, never taken from this listing.
//!
//! The normal head check stays on GitHub's anonymous web host. Like the existing
//! source-only-head fallback, platform fallback may use a paginated API listing;
//! it is bounded and requires exhaustion before returning any candidate.

use crate::{HttpError, Source, api_get_classified};
use serde::Deserialize;
use std::collections::BTreeMap;

const PER_PAGE: usize = 100;
const MAX_PAGES: usize = 30;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

/// Untrusted availability hints, not authenticated artifact claims. Filtering
/// by exact asset name prevents historical Mac-only releases (including their
/// retired signers) from becoming Linux candidates. Every selected native
/// appcast and payload must still pass the complete current-authority chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCandidate {
    pub tag: String,
    /// Archived manifests are native locations, never the current APP head.
    pub current_appcast: bool,
    pub linux_x86_64: bool,
    pub linux_aarch64: bool,
}

impl ReleaseCandidate {
    pub fn appcast_name(&self) -> String {
        if self.current_appcast {
            "aterm-appcast.toml".into()
        } else {
            format!("aterm-appcast-{}.toml", self.tag)
        }
    }

    pub fn has_linux(&self, target: crate::linux::LinuxTarget) -> bool {
        match target {
            crate::linux::LinuxTarget::X86_64 => self.linux_x86_64,
            crate::linux::LinuxTarget::Aarch64 => self.linux_aarch64,
        }
    }
}

/// One fully exhausted listing with APP and per-architecture availability.
pub fn candidates(source: &Source, token: Option<&str>) -> Result<Vec<ReleaseCandidate>, String> {
    candidates_with(source, token, api_get_classified)
}

/// Discover canonical APP release tags in descending numeric order. Credentials,
/// when supplied, go only to the existing API transport's protected stdin lane.
/// No assets are downloaded or authenticated by discovery.
pub fn candidate_tags(source: &Source, token: Option<&str>) -> Result<Vec<String>, String> {
    candidates(source, token).map(|rows| {
        rows.into_iter()
            .filter(|row| row.current_appcast)
            .map(|row| row.tag)
            .collect()
    })
}

fn candidates_with(
    source: &Source,
    token: Option<&str>,
    mut fetch: impl FnMut(&str, Option<&str>) -> Result<Vec<u8>, HttpError>,
) -> Result<Vec<ReleaseCandidate>, String> {
    if !crate::is_valid_slug(&source.owner) || !crate::is_valid_slug(&source.repo) {
        return Err("release catalog source contains an unsafe owner/repo".into());
    }
    let mut candidates = BTreeMap::new();
    for page in 1..=MAX_PAGES {
        let url = format!(
            "https://api.github.com/repos/{}/{}/releases?per_page={PER_PAGE}&page={page}",
            source.owner, source.repo
        );
        let bytes = fetch(&url, token).map_err(|e| format!("release catalog: {e}"))?;
        if bytes.len() > MAX_PAGE_BYTES {
            return Err("release catalog page exceeds the 8 MiB parsing bound".into());
        }
        let releases: Vec<Release> = aterm_json::from_slice(&bytes)
            .map_err(|e| format!("release catalog did not parse: {e}"))?;
        let count = releases.len();
        if count > PER_PAGE {
            return Err("release catalog returned more records than requested".into());
        }
        for release in releases {
            // Draft/prerelease tags must not poison stable discovery.
            if release.draft || release.prerelease {
                continue;
            }
            let has_name = |name: &str| release.assets.iter().any(|a| a.name == name);
            let current_appcast = has_name("aterm-appcast.toml");
            let archive = format!("aterm-appcast-{}.toml", release.tag_name);
            let archived_pair = has_name(&archive) && has_name(&format!("{archive}.sig"));
            let has_asset = |target: crate::linux::LinuxTarget| {
                let name =
                    target.asset_name(release.tag_name.strip_prefix('v').unwrap_or_default());
                has_name(&name)
            };
            let linux_x86_64 = has_asset(crate::linux::LinuxTarget::X86_64);
            let linux_aarch64 = has_asset(crate::linux::LinuxTarget::Aarch64);
            // The cutter archives previous exact-name appcasts. Keep native
            // locations discoverable, without reviving old Mac APP authority.
            if !current_appcast && !(archived_pair && (linux_x86_64 || linux_aarch64)) {
                continue;
            }
            let key = match crate::tag::parse_release_tag(&release.tag_name) {
                Ok(crate::tag::TagKind::Legacy) => continue,
                Ok(crate::tag::TagKind::Candidate(key)) => key,
                Err(e) => {
                    return Err(format!(
                        "malformed APP release tag {:?}: {e:?}",
                        release.tag_name
                    ));
                }
            };
            let candidate = ReleaseCandidate {
                tag: release.tag_name.clone(),
                current_appcast,
                linux_x86_64,
                linux_aarch64,
            };
            if candidates.insert(key, candidate).is_some() {
                return Err(format!(
                    "duplicate APP release tag {} in catalog",
                    release.tag_name
                ));
            }
        }
        if count < PER_PAGE {
            return Ok(candidates.into_values().rev().collect());
        }
    }
    Err(format!(
        "release catalog reached the {MAX_PAGES}-page bound before exhaustion"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_tags_with(
        source: &Source,
        token: Option<&str>,
        fetch: impl FnMut(&str, Option<&str>) -> Result<Vec<u8>, HttpError>,
    ) -> Result<Vec<String>, String> {
        candidates_with(source, token, fetch).map(|rows| {
            rows.into_iter()
                .filter(|row| row.current_appcast)
                .map(|row| row.tag)
                .collect()
        })
    }

    #[test]
    fn archived_native_pair_is_discoverable_but_never_elects_the_current_app_head() {
        let body = br#"[
          {"tag_name":"v0.100.0","assets":[{"name":"aterm-appcast.toml"}]},
          {"tag_name":"v0.101.0","assets":[{"name":"aterm-appcast-v0.101.0.toml"},{"name":"aterm-appcast-v0.101.0.toml.sig"},{"name":"aterm-0.101.0-linux-aarch64"}]},
          {"tag_name":"v0.99.0","assets":[{"name":"aterm-appcast-v0.99.0.toml"},{"name":"aterm-appcast-v0.99.0.toml.sig"},{"name":"aterm-0.99.0-linux-aarch64"}]},
          {"tag_name":"v0.98.0","assets":[{"name":"aterm-appcast-v0.98.0.toml"},{"name":"aterm-0.98.0-linux-aarch64"}]},
          {"tag_name":"v0.97.0","assets":[{"name":"aterm-appcast-v0.97.0.toml"},{"name":"aterm-appcast-v0.97.0.toml.sig"}]}
        ]"#;
        let rows = candidates_with(&source(), None, |_, _| Ok(body.to_vec())).unwrap();
        assert_eq!(
            rows.len(),
            3,
            "incomplete pair and Mac-only archives are not candidates"
        );
        assert_eq!(rows[0].tag, "v0.101.0");
        assert_eq!(
            rows.iter().find(|row| row.current_appcast).unwrap().tag,
            "v0.100.0"
        );
        assert_eq!(rows[2].appcast_name(), "aterm-appcast-v0.99.0.toml");
        assert!(rows[2].has_linux(crate::linux::LinuxTarget::Aarch64));
        assert_eq!(
            candidate_tags_with(&source(), None, |_, _| Ok(body.to_vec())).unwrap(),
            ["v0.100.0"]
        );
    }

    #[test]
    fn target_inventory_excludes_legacy_tar_and_wrong_tag_without_authenticating_mac_history() {
        let body = br#"[
          {"tag_name":"v0.100.0","assets":[{"name":"aterm-appcast.toml"}]},
          {"tag_name":"v0.99.0","assets":[{"name":"aterm-appcast.toml"},{"name":"aterm-0.99.0-linux-aarch64"}]},
          {"tag_name":"v0.98.0","assets":[{"name":"aterm-appcast.toml"},{"name":"aterm-0.98.0-linux-x86_64"}]},
          {"tag_name":"v0.68.0","assets":[{"name":"aterm-appcast.toml"},{"name":"aterm-0.68.0-linux-x86_64.tar.gz"},{"name":"aterm-0.67.0-linux-aarch64"}]}
        ]"#;
        let rows = candidates_with(&source(), None, |_, _| Ok(body.to_vec())).unwrap();
        assert_eq!(
            rows[0].tag, "v0.100.0",
            "Mac-only head still supplies current policy"
        );
        for target in [
            crate::linux::LinuxTarget::Aarch64,
            crate::linux::LinuxTarget::X86_64,
        ] {
            let native: Vec<_> = rows
                .iter()
                .filter(|row| row.has_linux(target))
                .map(|row| row.tag.as_str())
                .collect();
            assert_eq!(
                native,
                if target == crate::linux::LinuxTarget::Aarch64 {
                    vec!["v0.99.0"]
                } else {
                    vec!["v0.98.0"]
                }
            );
        }
    }

    fn source() -> Source {
        Source {
            owner: "alabsystems".into(),
            repo: "aterm".into(),
        }
    }

    fn discover(bytes: &[u8]) -> Result<Vec<String>, String> {
        candidate_tags_with(&source(), None, |url, token| {
            assert_eq!(
                url,
                "https://api.github.com/repos/alabsystems/aterm/releases?per_page=100&page=1"
            );
            assert_eq!(token, None);
            Ok(bytes.to_vec())
        })
    }

    #[test]
    fn numeric_candidates_skip_source_cuts_prereleases_drafts_and_legacy() {
        let tags = discover(br#"[
          {"tag_name":"v0.9.0","assets":[{"name":"aterm-appcast.toml","url":"https://evil.invalid/ignored"}]},
          {"tag_name":"v0.100.0"},
          {"tag_name":"atpkg-index-1"},
          {"tag_name":"v0.30.0-rc1","prerelease":true,"assets":[{"name":"aterm-appcast.toml"}]},
          {"tag_name":"v0.90.0","draft":true,"assets":[{"name":"aterm-appcast.toml"}]},
          {"tag_name":"v0.61","assets":[{"name":"aterm-appcast.toml"}]},
          {"tag_name":"v0.10.0","assets":[{"name":"aterm-appcast.toml"}]}
        ]"#).unwrap();
        assert_eq!(tags, ["v0.10.0", "v0.9.0"]);
    }

    #[test]
    fn malformed_app_tags_duplicates_and_invalid_json_fail_closed() {
        for bytes in [
            br#"[{"tag_name":"v0.01.0","assets":[{"name":"aterm-appcast.toml"}]}]"#.as_slice(),
            br#"[{"tag_name":"v0.1.0","assets":[{"name":"aterm-appcast.toml"}]},{"tag_name":"v0.1.0","assets":[{"name":"aterm-appcast.toml"}]}]"#,
            br#"{}"#, br#"not json"#,
        ] {
            assert!(discover(bytes).is_err());
        }
        assert!(discover(&vec![b' '; MAX_PAGE_BYTES + 1]).is_err());
    }

    #[test]
    fn pagination_requires_exhaustion_and_preserves_credentials() {
        let page = format!(
            "[{}]",
            vec![r#"{"tag_name":"source-only"}"#; PER_PAGE].join(",")
        )
        .into_bytes();
        let mut calls = 0;
        let result = candidate_tags_with(&source(), Some("test-token"), |url, token| {
            calls += 1;
            assert!(url.ends_with(&format!("&page={calls}")));
            assert_eq!(token, Some("test-token"));
            Ok(if calls == 2 {
                b"[]".to_vec()
            } else {
                page.clone()
            })
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert!(result.is_empty());
        calls = 0;
        let error = candidate_tags_with(&source(), None, |_, _| {
            calls += 1;
            Ok(page.clone())
        })
        .unwrap_err();
        assert_eq!(calls, MAX_PAGES);
        assert!(error.contains("before exhaustion"));
    }

    #[test]
    fn network_failure_never_becomes_an_empty_catalog() {
        let error = candidate_tags_with(&source(), None, |_, _| {
            Err(HttpError::RateLimited {
                code: 403,
                url: "https://api.github.com/".into(),
                authenticated: false,
            })
        })
        .unwrap_err();
        assert!(error.contains("rate limit"));
        let mut unsafe_source = source();
        unsafe_source.repo = "aterm/../../elsewhere".into();
        assert!(
            candidate_tags_with(&unsafe_source, None, |_, _| panic!("must not fetch")).is_err()
        );
    }
}
