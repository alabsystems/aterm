// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The compiled vendor table (design §1.1): one row per vendor-direct program. Everything
//! that decides where bytes may come from and who vouches for them is here, in source, so
//! no index publish can move it or stall it.

use serde::{Deserialize, Serialize};

use super::version::Version;

/// What authenticates a program's digest document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Anchor {
    /// `<v>/manifest.json` under Anthropic's compiled OpenPGP key.
    #[serde(rename = "anthropic-openpgp")]
    AnthropicOpenPgp,
    /// The release JSON on `releases.openai.com` agreeing with the GitHub release's
    /// `SHA256SUMS` — two independent hosts, no signature.
    #[serde(rename = "openai-two-host")]
    OpenAiTwoHost,
}

impl Anchor {
    /// The spelling a `.vendor` record carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicOpenPgp => "anthropic-openpgp",
            Self::OpenAiTwoHost => "openai-two-host",
        }
    }
}

/// Where one kind of digest document is fetched, and where its answer may come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocPin {
    /// The document's URL starts with this (https, trailing `/`); a program's pins are
    /// disjoint, so a URL names exactly one.
    pub prefix: &'static str,
    /// Hosts a redirect may end on. Empty: the document must be answered at the URL
    /// requested, with no redirect at all.
    pub redirect_hosts: &'static [&'static str],
}

/// One vendor-direct program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorSpec {
    /// The program, as [`crate::stub::AGENT_PROGRAMS`] spells it.
    pub program: &'static str,
    /// Who publishes it, for user-visible lines (`Anthropic latest`).
    pub vendor: &'static str,
    /// The product's own name.
    pub product: &'static str,
    /// The vendor's `latest` pointer — a hint only; everything it names is re-verified.
    pub head_url: &'static str,
    /// What authenticates the digest.
    pub anchor: Anchor,
    /// The Apple Developer ID team every darwin Mach-O must be signed by.
    pub apple_team: &'static str,
    /// The tools the build exposes; never taken from the index.
    pub exposes: &'static [&'static str],
    /// Every first-hop fetch URL must start with one of these (https, trailing `/`).
    pub url_prefixes: &'static [&'static str],
    /// Per digest document, where it is fetched and where its answer may come from.
    pub digest_docs: &'static [DocPin],
    /// `NAME=VALUE` entries the shims export; never taken from the index.
    pub shim_env: &'static [&'static str],
    /// The vendor version current when this row was written: nothing older is installed,
    /// so a frozen or replayed CDN cannot hand a new machine an old build. It is also at
    /// or above the version of every legacy pin up to [`Self::legacy_ceiling`].
    pub floor: Version,
    /// The newest legacy ALab index build of the program published before this row was
    /// written — build 2026092201 of both, pinned by indexes 43–44 (read from the index
    /// cache, 2026-09-22), whose signed pkg manifests name claude 2.1.280 and codex
    /// 0.156.0: the floors. So a legacy build at or below it whose version cannot be read
    /// is replaced by any admissible head with no index and no downgrade. A legacy build
    /// above it was pinned after this code and may carry a version above the floor: unread,
    /// only a person asking replaces it.
    pub legacy_ceiling: u64,
}

impl VendorSpec {
    /// Whether a digest document requested at `requested` may be read, having been
    /// answered from `effective` (curl's effective URL): `requested` is https under one of
    /// [`Self::digest_docs`], and `effective` is `requested` itself or an admissible https
    /// URL on one of that pin's redirect hosts.
    #[must_use]
    pub(crate) fn digest_doc_admitted(&self, requested: &str, effective: &str) -> bool {
        let Some(pin) = self
            .digest_docs
            .iter()
            .find(|pin| requested.starts_with(pin.prefix))
        else {
            return false;
        };
        crate::vendor::https_host(requested).is_some()
            && (effective == requested
                || crate::vendor::https_host(effective)
                    .is_some_and(|h| pin.redirect_hosts.contains(&h)))
    }

    /// [`Self::shim_env`] admitted under [`crate::shim_env::ShimEnv::admit`]'s rule
    /// (pinned by a test, so the fallback never fires).
    #[must_use]
    pub fn shim_env(&self) -> crate::shim_env::ShimEnv {
        let raw: Vec<String> = self.shim_env.iter().map(|s| (*s).to_string()).collect();
        crate::shim_env::ShimEnv::admit(&raw).unwrap_or(crate::shim_env::ShimEnv::NONE)
    }
}

/// A compile-time version; an out-of-range literal fails the build.
const fn floor(major: u32, minor: u32, patch: u32) -> Version {
    match Version::new(major, minor, patch) {
        Some(v) => v,
        None => panic!("vendor floor out of range"),
    }
}

/// The newest legacy pin published when this was written ([`VendorSpec::legacy_ceiling`]):
/// indexes 43–44 pin claude and codex at build 2026092201, the compiled floors. The vendor
/// lane has pinned past it since (index 45: codex 2026092301; index 46: claude 2026092401),
/// so the release carrying this code re-derives it, and the floors, from the live index once
/// that lane is booted out (docs/RUNBOOK-atpkg-republish.md, cutover steps 1–2);
/// `spec_coherence` refuses a committed legacy pin above it.
const LEGACY_CEILING: u64 = 2_026_092_201;

/// The vendor-direct programs, in [`crate::stub::AGENT_PROGRAMS`] order.
pub const VENDORS: &[VendorSpec] = &[
    VendorSpec {
        program: "claude",
        vendor: "Anthropic",
        product: "Claude Code",
        head_url: "https://downloads.claude.ai/claude-code-releases/latest",
        anchor: Anchor::AnthropicOpenPgp,
        apple_team: "Q6L2SF6YDW",
        exposes: &["claude"],
        url_prefixes: &["https://downloads.claude.ai/claude-code-releases/"],
        // `<v>/manifest.json` and its `.sig`, answered in place (measured 2026-09-22).
        digest_docs: &[DocPin {
            prefix: "https://downloads.claude.ai/claude-code-releases/",
            redirect_hosts: &[],
        }],
        // Claude's own updater writes a copy under `~/.local` that this name never runs.
        shim_env: &["DISABLE_AUTOUPDATER=1"],
        floor: floor(2, 1, 280),
        legacy_ceiling: LEGACY_CEILING,
    },
    VendorSpec {
        program: "codex",
        vendor: "OpenAI",
        product: "Codex CLI",
        head_url: "https://releases.openai.com/codex/channels/latest",
        anchor: Anchor::OpenAiTwoHost,
        apple_team: "2DC432GLL2",
        exposes: &["codex"],
        url_prefixes: &[
            "https://releases.openai.com/codex/",
            "https://github.com/openai/codex/releases/download/",
        ],
        digest_docs: &[
            // The release JSON: releases.openai.com itself, never redirected — else both
            // halves of the two-host agreement could come from GitHub.
            DocPin {
                prefix: "https://releases.openai.com/codex/",
                redirect_hosts: &[],
            },
            // `SHA256SUMS`: github.com answers with a redirect to its release-asset
            // storage (measured for rust-v0.156.0).
            DocPin {
                prefix: "https://github.com/openai/codex/releases/download/",
                redirect_hosts: &[
                    "release-assets.githubusercontent.com",
                    "objects.githubusercontent.com",
                ],
            },
        ],
        shim_env: &[],
        floor: floor(0, 156, 0),
        legacy_ceiling: LEGACY_CEILING,
    },
];

/// The row for `program`, or `None` for a program the index manages.
#[must_use]
pub fn spec(program: &str) -> Option<&'static VendorSpec> {
    VENDORS.iter().find(|s| s.program == program)
}

/// Whether `program` is updated vendor-direct rather than through the ALab index.
#[must_use]
pub fn is_vendor(program: &str) -> bool {
    spec(program).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The authority host of an `https://host/...` URL.
    fn host(url: &str) -> &str {
        let rest = url.strip_prefix("https://").expect("https");
        rest.split('/').next().unwrap()
    }

    #[test]
    fn the_table_is_exactly_the_agent_programs() {
        let programs: Vec<&str> = VENDORS.iter().map(|s| s.program).collect();
        assert_eq!(programs, crate::stub::AGENT_PROGRAMS);
        for name in crate::stub::AGENT_PROGRAMS {
            assert!(is_vendor(name));
            assert_eq!(spec(name).map(|s| s.program), Some(*name));
        }
        for other in ["trust", "ay", "gh", "", "Claude", "claude "] {
            assert!(!is_vendor(other), "{other:?}");
        }
    }

    #[test]
    fn the_rows_are_the_measured_facts() {
        let claude = spec("claude").unwrap();
        assert_eq!(
            (claude.vendor, claude.anchor, claude.apple_team),
            ("Anthropic", Anchor::AnthropicOpenPgp, "Q6L2SF6YDW")
        );
        assert_eq!(claude.exposes, &["claude"]);
        assert_eq!(claude.shim_env, &["DISABLE_AUTOUPDATER=1"]);
        assert_eq!(claude.floor.to_string(), "2.1.280");
        let codex = spec("codex").unwrap();
        assert_eq!(
            (codex.vendor, codex.anchor, codex.apple_team),
            ("OpenAI", Anchor::OpenAiTwoHost, "2DC432GLL2")
        );
        assert_eq!(codex.exposes, &["codex"]);
        assert!(codex.shim_env.is_empty());
        assert_eq!(codex.floor.to_string(), "0.156.0");
        // Build 2026092201 of both, the newest legacy pins (indexes 43–44), at the floors.
        for s in VENDORS {
            assert_eq!(s.legacy_ceiling, 2_026_092_201, "{}", s.program);
            assert!(!super::super::is_vendor_build(s.legacy_ceiling));
        }
    }

    /// The floors are the versions of the committed fixtures, which were fetched from the
    /// vendors the day this table was written.
    #[test]
    fn the_floors_are_the_fixture_releases() {
        let manifest: aterm_json::Value =
            aterm_json::from_str(include_str!("fixtures/claude-2.1.280-manifest.json")).unwrap();
        let version = manifest["version"].as_str().unwrap();
        assert_eq!(Version::parse(version), Some(spec("claude").unwrap().floor));
        let build_date = manifest["buildDate"].as_str().unwrap();
        assert!(super::super::version::BuildDate::parse(build_date).is_some());
        let release: aterm_json::Value =
            aterm_json::from_str(include_str!("fixtures/codex-channel-latest.json")).unwrap();
        let tag = release["tag_name"].as_str().unwrap();
        let codex_floor = spec("codex").unwrap().floor.to_string();
        assert_eq!(tag.strip_prefix("rust-v"), Some(codex_floor.as_str()));
    }

    #[test]
    fn every_pin_is_https_and_an_allowed_vendor_host() {
        for s in VENDORS {
            assert!(
                s.url_prefixes.iter().any(|p| s.head_url.starts_with(p)),
                "{}: the head is fetched under a pinned prefix",
                s.program
            );
            for p in s.url_prefixes {
                assert!(p.starts_with("https://") && p.ends_with('/'), "{p}");
                assert!(crate::vendor::VENDOR_HOSTS.contains(&host(p)), "{p}");
            }
            for (i, pin) in s.digest_docs.iter().enumerate() {
                assert!(
                    s.url_prefixes.iter().any(|p| pin.prefix.starts_with(p)),
                    "{}: a digest document is fetched under a pinned prefix",
                    pin.prefix
                );
                for other in &s.digest_docs[i + 1..] {
                    assert!(
                        !pin.prefix.starts_with(other.prefix)
                            && !other.prefix.starts_with(pin.prefix),
                        "{} and {} overlap",
                        pin.prefix,
                        other.prefix
                    );
                }
                for h in pin.redirect_hosts {
                    assert!(crate::vendor::VENDOR_HOSTS.contains(h), "{h}");
                }
            }
            let team = s.apple_team;
            assert_eq!(team.len(), 10);
            assert!(
                team.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            );
            for tool in s.exposes {
                assert!(crate::store::shim_allowed(tool), "{tool}");
            }
            assert_eq!(
                s.shim_env().entries().len(),
                s.shim_env.len(),
                "{}",
                s.program
            );
            assert_ne!(
                s.program, "digests",
                "collides with the digest log's file name"
            );
        }
    }

    /// Each digest document keeps its own host pin: the codex release JSON is never read
    /// from GitHub's storage, and only the SUMS may arrive through a redirect.
    #[test]
    fn digest_documents_are_pinned_per_document() {
        let codex = spec("codex").unwrap();
        let json = codex.head_url;
        let sums = "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS";
        let storage =
            "https://release-assets.githubusercontent.com/github-production-release-asset/1";
        assert!(codex.digest_doc_admitted(json, json));
        for effective in [
            storage,
            "https://objects.githubusercontent.com/x",
            "https://releases.openai.com/codex/elsewhere",
            "https://github.com/openai/codex/releases/download/x",
        ] {
            assert!(!codex.digest_doc_admitted(json, effective), "{effective}");
        }
        assert!(codex.digest_doc_admitted(sums, storage));
        assert!(codex.digest_doc_admitted(sums, "https://objects.githubusercontent.com/x"));
        assert!(codex.digest_doc_admitted(sums, sums));
        for effective in [
            "https://releases.openai.com/codex/channels/latest",
            "https://evil.example/x",
            "http://release-assets.githubusercontent.com/x",
            "https://release-assets.githubusercontent.com:8443/x",
            "https://user@release-assets.githubusercontent.com/x",
        ] {
            assert!(!codex.digest_doc_admitted(sums, effective), "{effective}");
        }
        // A document outside every pin is never admitted, even answered in place.
        for requested in [
            "https://github.com/other/codex/releases/download/x",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json",
            "http://releases.openai.com/codex/channels/latest",
        ] {
            assert!(
                !codex.digest_doc_admitted(requested, requested),
                "{requested}"
            );
        }
        let claude = spec("claude").unwrap();
        let manifest = "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json";
        assert!(claude.digest_doc_admitted(manifest, manifest));
        assert!(!claude.digest_doc_admitted(manifest, "https://downloads.claude.ai/other"));
        assert!(!claude.digest_doc_admitted(manifest, storage));
    }

    #[test]
    fn anchors_spell_as_their_record_strings() {
        #[derive(Serialize, Deserialize)]
        struct Row {
            a: Anchor,
        }
        for a in [Anchor::AnthropicOpenPgp, Anchor::OpenAiTwoHost] {
            let text = aterm_toml::to_string(&Row { a }).unwrap();
            assert_eq!(text.trim(), format!("a = \"{}\"", a.as_str()));
            assert_eq!(aterm_toml::from_str::<Row>(&text).unwrap().a, a);
        }
    }
}
