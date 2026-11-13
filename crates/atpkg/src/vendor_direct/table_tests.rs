// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The warm never starts a turn: claude's is print mode over a stdin it never writes —
/// never `--bare` or `--init-only`, which skip or race the fetches it exists for — with
/// no transcript and no hooks; no warm argv carries a free operand, which print mode
/// would send as a prompt.
#[test]
fn the_warm_argv_refreshes_caches_and_never_prompts() {
    let claude = spec("claude").unwrap().warm.unwrap().argv;
    for present in ["-p", "--no-session-persistence", "--strict-mcp-config"] {
        assert!(claude.contains(&present), "{present}");
    }
    assert!(claude.iter().any(|a| a.contains("disableAllHooks")));
    for absent in ["--bare", "--init-only"] {
        assert!(!claude.contains(&absent), "{absent}");
    }
    let takes_value = ["--input-format", "--output-format", "--settings"];
    for (i, arg) in claude.iter().enumerate() {
        assert!(
            arg.starts_with('-') || (i > 0 && takes_value.contains(&claude[i - 1])),
            "{arg:?} would be a prompt"
        );
    }
}

/// EVERY ROW'S PINS NEST: each URL prefix is https, ends at a `/` and has an admissible
/// host; the head lies under one; each digest-document pin lies under one, no two pins
/// are prefixes of each other (so the first match is the only match), and every redirect
/// host is admissible. grok's npm pins name the four platform packages, never the scope,
/// so the umbrella `@xai-official/grok` is outside every pin.
#[test]
fn every_rows_pins_nest_under_its_url_prefixes() {
    for row in VENDORS {
        let under = |url: &str| {
            row.url_prefixes
                .iter()
                .any(|p| url.starts_with(p) && url.len() > p.len())
        };
        for prefix in row.url_prefixes {
            assert!(
                prefix.starts_with("https://") && prefix.ends_with('/'),
                "{}: {prefix}",
                row.program
            );
            assert!(crate::vendor::https_host(prefix).is_some(), "{prefix}");
        }
        assert!(under(row.head_url), "{}: {}", row.program, row.head_url);
        for (i, pin) in row.digest_docs.iter().enumerate() {
            assert!(
                row.url_prefixes.iter().any(|p| pin.prefix.starts_with(p)),
                "{}: {} is under no url prefix",
                row.program,
                pin.prefix
            );
            for other in &row.digest_docs[i + 1..] {
                assert!(
                    !pin.prefix.starts_with(other.prefix) && !other.prefix.starts_with(pin.prefix),
                    "{}: {} and {}",
                    row.program,
                    pin.prefix,
                    other.prefix
                );
            }
            for host in pin.redirect_hosts {
                assert!(
                    crate::vendor::https_host(&format!("https://{host}/")).is_some(),
                    "{host}"
                );
            }
        }
    }
    let grok = spec("grok").unwrap();
    for umbrella in [
        "https://registry.npmjs.org/@xai-official/grok/1.0.70",
        "https://registry.npmjs.org/@xai-official/grok-cli/1.0.70",
    ] {
        assert!(!grok.digest_doc_admitted(umbrella, umbrella), "{umbrella}");
        assert!(!crate::vendor::vendor_direct_url_allowed("grok", umbrella));
    }
}

/// THE ALIASES ARE THE ROW'S OWN: each `(name, target)` names one of the row's exposed
/// tools and a target it does not expose — a clone inside the program's own build, never
/// a shim into another program's — both admissible tool names; and no tool is exposed by
/// two rows, so every `bin/` name is one program's.
#[test]
fn every_alias_is_a_clone_inside_its_own_rows_build() {
    let mut seen = std::collections::BTreeSet::new();
    for row in VENDORS {
        for tool in row.exposes {
            assert!(seen.insert(*tool), "{tool} exposed by two rows");
            assert!(crate::store::ToolName::new(tool).is_some(), "{tool}");
        }
        for (name, target) in row.aliases {
            assert!(row.exposes.contains(name), "{}: {name}", row.program);
            assert!(!row.exposes.contains(target), "{}: {target}", row.program);
            assert!(crate::store::ToolName::new(target).is_some(), "{target}");
        }
    }
    let aliased: Vec<&str> = VENDORS
        .iter()
        .filter(|row| !row.aliases.is_empty())
        .map(|row| row.program)
        .collect();
    assert_eq!(aliased, ["codex-old"]);
    let (old, codex) = (spec("codex-old").unwrap(), spec("codex").unwrap());
    assert_eq!(old.exposes, &["codex-old"]);
    assert_eq!(old.aliases, &[("codex-old", "codex")]);
    let lane = |s: &VendorSpec| {
        (
            (s.vendor, s.product, s.head_url, s.anchor, s.apple_team),
            (s.url_prefixes, s.url_query, s.digest_docs, s.shim_env),
            (s.self_update_verbs, s.yolo_flags, s.yolo_markers),
            (s.yolo_after, s.yolo_skip, s.revision_versions),
        )
    };
    assert_eq!(lane(old), lane(codex));
    assert!(old.warm.is_none() && !old.live_upgrade);
    assert_eq!(old.legacy_ceiling, NEVER_INDEX_PINNED);
    assert!(old.floor >= codex.floor);
    assert!(old.description.starts_with("OpenAI Codex CLI"));
    assert!(crate::alab::agent("codex-old").is_none());
    assert!(codex.warm.is_some() && codex.live_upgrade);
    assert!(crate::alab::agent("codex").is_some());
}

/// The `anchor` key of a `.vendor` record an earlier client wrote: each spelling is
/// an on-disk contract, and a record whose anchor no longer parses is refused.
#[test]
fn anchors_spell_as_their_record_strings() {
    #[derive(Serialize, Deserialize)]
    struct Row {
        a: Anchor,
    }
    for (a, spelled) in [
        (Anchor::AnthropicOpenPgp, "anthropic-openpgp"),
        (Anchor::OpenAiTwoHost, "openai-two-host"),
        (Anchor::GoogleTwoHost, "google-two-host"),
        (Anchor::MetaDeveloperId, "meta-developer-id"),
        (Anchor::MoonshotTwoHost, "moonshot-two-host"),
        (Anchor::XaiDeveloperId, "xai-developer-id"),
        (Anchor::XaiNpmRegistry, "xai-npm-registry"),
    ] {
        let text = aterm_toml::to_string(&Row { a }).unwrap();
        assert_eq!(text.trim(), format!("a = \"{spelled}\""));
        assert_eq!(aterm_toml::from_str::<Row>(&text).unwrap().a, a);
    }
}

/// Each digest document keeps its own host pin: the codex release JSON is never read
/// from GitHub's storage, and only the SUMS may arrive through a redirect.
#[test]
fn digest_documents_are_pinned_per_document() {
    let codex = spec("codex").unwrap();
    let json = codex.head_url;
    let sums =
        "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS";
    let storage = "https://release-assets.githubusercontent.com/github-production-release-asset/1";
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
    let agy = spec("agy").unwrap();
    let beside = "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.3.6-1/darwin-arm/manifest.json";
    let muse = spec("muse").unwrap();
    let channel = muse.head_url;
    let release = "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse&version=1.4.9-R6297.1&file=manifest.json";
    for (row, doc, other) in [
        (agy, agy.head_url, beside),
        (agy, beside, agy.head_url),
        (muse, channel, release),
        (muse, release, channel),
    ] {
        assert!(row.digest_doc_admitted(doc, doc), "{doc}");
        assert!(!row.digest_doc_admitted(doc, other), "{doc} from {other}");
    }
    let kimi = spec("kimi").unwrap();
    let cdn = "https://cdn.kimi.com/kimi-code/binaries/2.3.1/manifest.json";
    let github = "https://github.com/MoonshotAI/kimi-code/releases/download/\
                  @moonshot-ai/kimi-code@2.3.1/manifest.json";
    for doc in [kimi.head_url, cdn, github] {
        assert!(kimi.digest_doc_admitted(doc, doc), "{doc}");
    }
    assert!(kimi.digest_doc_admitted(github, storage));
    assert!(kimi.digest_doc_admitted(github, "https://objects.githubusercontent.com/x"));
    for effective in [
        storage,
        "https://objects.githubusercontent.com/x",
        "https://cdn.kimi.com/kimi-code/binaries/2.3.2/manifest.json",
        github,
    ] {
        assert!(
            !kimi.digest_doc_admitted(cdn, effective),
            "{cdn} from {effective}"
        );
        assert!(
            !kimi.digest_doc_admitted(kimi.head_url, effective),
            "the head from {effective}"
        );
    }
    for effective in [
        cdn,
        "https://code.kimi.com/kimi-code/latest",
        "https://evil.example/x",
        "https://release-assets.githubusercontent.com.evil.com/x",
        "http://release-assets.githubusercontent.com/x",
    ] {
        assert!(!kimi.digest_doc_admitted(github, effective), "{effective}");
    }
    let grok = spec("grok").unwrap();
    let npm_doc = "https://registry.npmjs.org/@xai-official/grok-linux-x64/1.0.70";
    for doc in [grok.head_url, npm_doc] {
        assert!(grok.digest_doc_admitted(doc, doc), "{doc}");
    }
    for effective in [
        "https://x.ai/cli/stable",
        "https://storage.googleapis.com/grok-build-public-artifacts/cli/alpha",
        "https://storage.googleapis.com/antigravity-public/antigravity-cli/stable",
        npm_doc,
    ] {
        assert!(
            !grok.digest_doc_admitted(grok.head_url, effective),
            "grok's pointer from {effective}"
        );
    }
    for effective in [
        "https://registry.npmjs.org/@xai-official/grok-linux-x64/1.0.69",
        "https://registry.npmjs.com/@xai-official/grok-linux-x64/1.0.70",
        "https://www.npmjs.com/package/@xai-official/grok-linux-x64",
        grok.head_url,
    ] {
        assert!(
            !grok.digest_doc_admitted(npm_doc, effective),
            "npm's version document from {effective}"
        );
    }
    assert_eq!(
        grok.anchor.digest_alg(),
        DigestAlg::Sha512,
        "npm signs a sha512"
    );
    for row in VENDORS {
        assert_ne!(row.anchor, Anchor::XaiDeveloperId, "{}", row.program);
        assert!(row.anchor.reads_record_of(row.anchor), "{}", row.program);
    }
    assert!(Anchor::XaiNpmRegistry.reads_record_of(Anchor::XaiDeveloperId));
    assert!(!Anchor::XaiDeveloperId.reads_record_of(Anchor::XaiNpmRegistry));
    assert!(!Anchor::MetaDeveloperId.reads_record_of(Anchor::XaiDeveloperId));
}
