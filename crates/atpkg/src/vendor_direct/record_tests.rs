// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::store::Layout;

fn layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-vendor-record-{label}-"))
}

fn record() -> VendorRecord {
    VendorRecord {
        schema: RECORD_SCHEMA,
        program: "claude".into(),
        version: Version::parse("2.1.280").unwrap(),
        vendor: "Anthropic".into(),
        source_url: "https://downloads.claude.ai/claude-code-releases/2.1.280/darwin-arm64/claude"
            .into(),
        sha256: "387a5c5dcdbb815085edf0baf79591f9d8894efe922bceaf3d75b1b08055229d".into(),
        sha512: String::new(),
        size: 217_254_576,
        tree_root: "0".repeat(64),
        apple_team: team("claude"),
        anchor: Anchor::AnthropicOpenPgp,
        build_date: BuildDate::parse("2026-09-21T20:55:27Z"),
        verified_at: 1_790_000_000,
        measured_sha256: None,
        agreed_with: None,
        tree_from: None,
    }
}

/// The team a record of `program` carries on this platform.
fn team(program: &str) -> Option<String> {
    expected_team(spec(program).unwrap()).map(str::to_string)
}

/// Write `rec` beside `store/<program>/<build>` the way the stager does.
fn place(l: &Layout, program: &str, build: u64, rec: &VendorRecord) -> PathBuf {
    let dir = l.build_dir(program, build);
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = rec.record_bytes().unwrap();
    super::super::durable::write_durable(&record_path(&dir).unwrap(), &bytes).unwrap();
    dir
}

#[test]
fn a_record_round_trips_beside_its_build() {
    let l = layout("roundtrip");
    let rec = record();
    let dir = place(&l, "claude", rec.version.build_id(), &rec);
    assert_eq!(
        record_path(&dir).unwrap().file_name().unwrap(),
        "1000002000001000280.vendor"
    );
    assert_eq!(read_record(&dir), Some(rec.clone()));
    let view = rec.installed_view();
    assert_eq!(view.build, 1_000_002_000_001_000_280);
    assert_eq!(view.build_date, rec.build_date);
    let bare = VendorRecord {
        program: "codex".into(),
        version: Version::parse("0.156.0").unwrap(),
        vendor: "OpenAI".into(),
        source_url: "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package-aarch64-apple-darwin.tar.gz".into(),
        anchor: Anchor::OpenAiTwoHost,
        apple_team: team("codex"),
        build_date: None,
        ..record()
    };
    let dir = place(&l, "codex", bare.version.build_id(), &bare);
    assert_eq!(read_record(&dir), Some(bare));
}

/// Verify that a record names one payload digest under its algorithm's key.
#[test]
fn a_record_names_its_payload_digest_by_algorithm() {
    let l = layout("digest");
    let rec = record();
    let team = rec
        .apple_team
        .as_ref()
        .map(|t| format!("apple_team = \"{t}\"\n"))
        .unwrap_or_default();
    let earlier = format!(
        "schema = 1\nprogram = \"claude\"\nversion = \"2.1.280\"\nvendor = \"Anthropic\"\n\
         source_url = \"{url}\"\nsha256 = \"{sha}\"\nsize = 217254576\ntree_root = \"{root}\"\n\
         {team}anchor = \"anthropic-openpgp\"\nbuild_date = \"2026-09-21T20:55:27Z\"\n\
         verified_at = 1790000000\n",
        url = rec.source_url,
        sha = rec.sha256,
        root = rec.tree_root,
    );
    assert_eq!(
        String::from_utf8(rec.record_bytes().unwrap()).unwrap(),
        earlier
    );
    let dir = l.build_dir("claude", rec.version.build_id());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(record_path(&dir).unwrap(), &earlier).unwrap();
    assert_eq!(read_record(&dir), Some(rec.clone()));

    let by_512 = VendorRecord {
        sha256: String::new(),
        sha512: "5".repeat(128),
        ..rec.clone()
    };
    let bytes = String::from_utf8(by_512.record_bytes().unwrap()).unwrap();
    assert!(
        bytes.contains(&format!("\nsha512 = \"{}\"\n", by_512.sha512)),
        "{bytes}"
    );
    assert!(!bytes.contains("sha256"), "{bytes}");
    place(&l, "claude", by_512.version.build_id(), &by_512);
    assert_eq!(read_record(&dir), Some(by_512.clone()));

    for (sha256, sha512) in [
        (rec.sha256.clone(), by_512.sha512.clone()),
        (String::new(), String::new()),
        (String::new(), "5".repeat(64)),
        (String::new(), "A".repeat(128)),
        ("5".repeat(128), String::new()),
    ] {
        let bad = VendorRecord {
            sha256,
            sha512,
            ..rec.clone()
        };
        assert!(bad.record_bytes().is_err(), "{bad:?}");
        let bytes = super::super::durable::to_toml(&bad, MAX_RECORD_BYTES).unwrap();
        std::fs::write(record_path(&dir).unwrap(), bytes).unwrap();
        assert_eq!(read_record(&dir), None, "{bad:?}");
    }
}

#[test]
fn a_record_is_trusted_only_beside_a_complete_build() {
    let l = layout("complete");
    let rec = record();
    let dir = place(&l, "claude", rec.version.build_id(), &rec);
    assert_eq!(complete_record(&dir), None, "no .ready yet");
    crate::store::mark_build_ready(&dir).unwrap();
    assert_eq!(complete_record(&dir), Some(rec));
}

#[test]
fn a_record_that_disagrees_with_its_directory_is_none() {
    let l = layout("mismatch");
    let rec = record();
    let other = Version::parse("2.1.281").unwrap().build_id();
    assert_eq!(read_record(&place(&l, "claude", other, &rec)), None);
    assert_eq!(
        read_record(&place(&l, "codex", rec.version.build_id(), &rec)),
        None
    );
    assert_eq!(read_record(&place(&l, "claude", 2_026_091_901, &rec)), None);
}

#[test]
fn a_malformed_record_is_none() {
    let l = layout("malformed");
    let rec = record();
    let dir = place(&l, "claude", rec.version.build_id(), &rec);
    let path = record_path(&dir).unwrap();
    let good = std::fs::read_to_string(&path).unwrap();
    for bad in [
        String::from("not toml {{{"),
        String::new(),
        good.replace("schema = 1", "schema = 2"),
        good.replace("2.1.280", "2.1.0280"),
        good.replace(&rec.sha256, "abc"),
        good.replace(&rec.sha256, &rec.sha256.to_uppercase()),
        good.replace("anthropic-openpgp", "nobody"),
        good.replace("2026-09-21T20:55:27Z", "yesterday"),
        good.replace("https://", "http://"),
        good.replace("size = 217254576", "size = 0"),
        good.lines()
            .filter(|l| !l.starts_with("verified_at"))
            .collect::<Vec<_>>()
            .join("\n"),
    ] {
        assert_ne!(bad, good);
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(read_record(&dir), None, "{bad}");
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(read_record(&dir), None);
}

#[test]
fn record_bytes_refuses_what_read_record_would_refuse() {
    for bad in [
        VendorRecord {
            schema: 2,
            ..record()
        },
        VendorRecord {
            program: "trust".into(),
            ..record()
        },
        VendorRecord {
            sha256: "zz".into(),
            ..record()
        },
        VendorRecord {
            tree_root: String::new(),
            ..record()
        },
        VendorRecord {
            size: 0,
            ..record()
        },
        VendorRecord {
            apple_team: Some("short".into()),
            ..record()
        },
        VendorRecord {
            vendor: String::new(),
            ..record()
        },
    ] {
        assert!(bad.record_bytes().is_err(), "{bad:?}");
    }
}

/// Verify that a record not matching the program's compiled row is refused on write and read.
#[test]
fn a_record_must_match_the_compiled_row() {
    let other_team = match team("claude") {
        Some(_) => vec![
            None,
            Some("2DC432GLL2".to_string()),
            Some("q6l2sf6ydw".into()),
        ],
        None => vec![Some("Q6L2SF6YDW".to_string())],
    };
    let mut bad: Vec<VendorRecord> = other_team
        .into_iter()
        .map(|apple_team| VendorRecord {
            apple_team,
            ..record()
        })
        .collect();
    bad.extend([
        VendorRecord {
            anchor: Anchor::OpenAiTwoHost,
            ..record()
        },
        VendorRecord {
            vendor: "OpenAI".into(),
            ..record()
        },
        VendorRecord {
            source_url: "https://evil.example/claude-code-releases/2.1.280/claude".into(),
            ..record()
        },
        VendorRecord {
            source_url: "https://downloads.claude.ai/other/2.1.280/claude".into(),
            ..record()
        },
        VendorRecord {
            source_url: "https://downloads.claude.ai/claude-code-releases/".into(),
            ..record()
        },
        VendorRecord {
            source_url: "https://github.com/openai/codex/releases/download/rust-v0.156.0/x".into(),
            ..record()
        },
    ]);
    let l = layout("row");
    for rec in bad {
        assert!(rec.record_bytes().is_err(), "{rec:?}");
        let dir = l.build_dir("claude", rec.version.build_id());
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = super::super::durable::to_toml(&rec, MAX_RECORD_BYTES).unwrap();
        std::fs::write(record_path(&dir).unwrap(), bytes).unwrap();
        crate::store::mark_build_ready(&dir).unwrap();
        assert_eq!(read_record(&dir), None, "{rec:?}");
        assert_eq!(complete_record(&dir), None, "{rec:?}");
    }
}

/// A record names as the tree's lender only a row that lends its tree to its own: codex for
/// codex-old, never itself, a row with an alias of its own, another head's row or no row.
#[test]
fn a_record_names_only_a_row_that_lends_it_its_tree() {
    let l = layout("lender");
    let version = Version::parse("0.163.0").unwrap();
    let codex_old = VendorRecord {
        program: "codex-old".into(),
        version,
        vendor: "OpenAI".into(),
        source_url: "https://github.com/openai/codex/releases/download/rust-v0.163.0/codex-package-aarch64-apple-darwin.tar.gz".into(),
        anchor: Anchor::OpenAiTwoHost,
        apple_team: team("codex-old"),
        build_date: None,
        tree_from: Some("codex".into()),
        ..record()
    };
    let dir = place(&l, "codex-old", version.build_id(), &codex_old);
    assert_eq!(read_record(&dir), Some(codex_old.clone()));
    let text = std::fs::read_to_string(record_path(&dir).unwrap()).unwrap();
    assert!(text.ends_with("tree_from = \"codex\"\n"), "{text}");
    for lender in ["codex-old", "claude", "grok", "", "codex2"] {
        let bad = VendorRecord {
            tree_from: Some(lender.into()),
            ..codex_old.clone()
        };
        assert!(bad.record_bytes().is_err(), "{lender:?}");
    }
    let codex = VendorRecord {
        program: "codex".into(),
        tree_from: Some("codex-old".into()),
        ..codex_old.clone()
    };
    assert!(
        codex.record_bytes().is_err(),
        "codex-old's tree carries its alias"
    );
    assert!(
        VendorRecord {
            tree_from: None,
            ..codex
        }
        .record_bytes()
        .is_ok()
    );
}

/// A grok record, from either lane: the one an earlier aterm wrote when xAI's Developer ID
/// vouched for the bucket's raw binary still reads, word for word, and one this lane
/// writes names npm's tarball beside the binary's agreed sha256 and the bucket copy it
/// agreed with; half an agreement, or one off the row's hosts, is not a record.
#[test]
fn a_grok_record_reads_from_either_lane() {
    let l = layout("grok");
    let version = Version::parse("1.0.68").unwrap();
    let team = team("grok")
        .map(|t| format!("apple_team = \"{t}\"\n"))
        .unwrap_or_default();
    let earlier = format!(
        "schema = 1\nprogram = \"grok\"\nversion = \"1.0.68\"\nvendor = \"xAI\"\n\
         source_url = \"https://storage.googleapis.com/grok-build-public-artifacts/cli/\
         grok-1.0.68-macos-aarch64\"\nsha256 = \"{sha}\"\nsize = 179063008\n\
         tree_root = \"{root}\"\n{team}anchor = \"xai-developer-id\"\n\
         verified_at = 1790000000\n",
        sha = "c".repeat(64),
        root = "0".repeat(64),
    );
    let dir = l.build_dir("grok", version.build_id());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(record_path(&dir).unwrap(), &earlier).unwrap();
    let old = read_record(&dir).expect("the earlier lane's record reads");
    assert_eq!(
        (old.anchor, old.measured_sha256.as_deref()),
        (Anchor::XaiDeveloperId, None)
    );
    assert_eq!(
        String::from_utf8(old.record_bytes().unwrap()).unwrap(),
        earlier,
        "and writes back as it was"
    );
    let agreed = VendorRecord {
        source_url: "https://registry.npmjs.org/@xai-official/grok-darwin-arm64/-/\
                     grok-darwin-arm64-1.0.68.tgz"
            .into(),
        sha256: String::new(),
        sha512: "d".repeat(128),
        anchor: Anchor::XaiNpmRegistry,
        measured_sha256: Some("e".repeat(64)),
        agreed_with: Some(
            "https://storage.googleapis.com/grok-build-public-artifacts/cli/\
             grok-1.0.68-macos-aarch64.zst"
                .into(),
        ),
        ..old.clone()
    };
    let dir = place(&l, "grok", version.build_id(), &agreed);
    assert_eq!(read_record(&dir), Some(agreed.clone()));
    let text = std::fs::read_to_string(record_path(&dir).unwrap()).unwrap();
    assert!(
        text.contains(&format!("measured_sha256 = \"{}\"\n", "e".repeat(64)))
            && text.contains("agreed_with = \"https://storage.googleapis.com/"),
        "{text}"
    );
    for bad in [
        VendorRecord {
            agreed_with: None,
            ..agreed.clone()
        },
        VendorRecord {
            measured_sha256: None,
            ..agreed.clone()
        },
        VendorRecord {
            measured_sha256: Some("E".repeat(64)),
            ..agreed.clone()
        },
        VendorRecord {
            agreed_with: Some("https://storage.googleapis.com/other/grok.zst".into()),
            ..agreed.clone()
        },
        VendorRecord {
            source_url: "https://registry.npmjs.org/@xai-official/grok/-/grok-1.0.68.tgz".into(),
            ..agreed.clone()
        },
        VendorRecord {
            anchor: Anchor::MetaDeveloperId,
            ..agreed.clone()
        },
    ] {
        assert!(bad.record_bytes().is_err(), "{bad:?}");
    }
    let claude_as_xai = VendorRecord {
        anchor: Anchor::XaiDeveloperId,
        ..record()
    };
    assert!(
        claude_as_xai.record_bytes().is_err(),
        "only grok's row reads the anchor it succeeded"
    );
}
