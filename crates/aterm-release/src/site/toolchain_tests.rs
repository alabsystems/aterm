// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::site::releases::parse_releases;
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use std::cell::Cell;
use std::path::{Path, PathBuf};

const NOW: i64 = 1_785_801_600;
const MASTER: [u8; 32] = [0x71; 32];
const MACHINE: [u8; 32] = [0x72; 32];

fn fixture() -> PathBuf {
    aterm_tempfile::manifest_dir().join("tests/fixtures/site/toolchain")
}

fn read_dir(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

fn levels_of(files: &BTreeMap<String, String>) -> Result<Levels, Unknown> {
    let index = aterm_toml::from_str(&files["index.toml"]).unwrap();
    levels(&index, &|name| files.get(name).cloned())
}

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}

fn public_key(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(kp(seed).public_key().as_ref()).unwrap()
}

fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
    kp(seed).sign(bytes).as_ref().to_vec()
}

fn roster() -> Vec<u8> {
    format!(
        "schema = 1\nroster_seq = 3\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         revoked = []\n\n[[machine]]\nid = \"m3\"\npubkey = \"{}\"\n\
         added_at = \"2026-08-01T00:00:00Z\"\n",
        public_key(&MACHINE)
    )
    .into_bytes()
}

fn signed_fixture() -> (
    Vec<crate::site::releases::Release>,
    BTreeMap<String, Vec<u8>>,
) {
    let files = read_dir(&fixture());
    let index = files["index.toml"]
        .replace("machine_id = \"m0\"", "machine_id = \"m3\"")
        .replace("roster_seq = 1", "roster_seq = 3")
        .into_bytes();
    let roster = roster();
    let mut served = BTreeMap::new();
    let url = |repo: &str, tag: &str, name: &str| {
        format!("file:///dl/alabsystems/{repo}/releases/download/{tag}/{name}")
    };
    served.insert(
        url("aterm", "atpkg-index-50", "index.toml.sig"),
        sign(&MACHINE, &index),
    );
    served.insert(
        url("aterm", "atpkg-index-50", "aterm-machines.toml.sig"),
        sign(&MASTER, &roster),
    );
    served.insert(
        url("aterm", "atpkg-index-50", "aterm-machines.toml"),
        roster,
    );
    served.insert(url("aterm", "atpkg-index-50", "index.toml"), index);
    for (repo, program, build) in [
        ("ay", "ay", 8256),
        ("aterm", "claude", 2_026_092_601),
        ("trust", "trust", 9192),
    ] {
        let name = manifest_name(program, build);
        let tag = format!("atpkg-{program}-{build}");
        let raw = files[&name]
            .replace(
                "kind = \"binary\"",
                "kind = \"binary\"\nasset = \"fixture\"",
            )
            .into_bytes();
        served.insert(
            url(repo, &tag, &format!("{name}.sig")),
            sign(&MACHINE, &raw),
        );
        served.insert(url(repo, &tag, &name), raw);
    }
    let releases = parse_releases(
        r#"[{"tag_name":"atpkg-index-51","prerelease":true,"draft":true,"assets":[{"name":"index.toml","size":1}]},
                {"tag_name":"atpkg-index-50","prerelease":true,"draft":false,"assets":[
                    {"name":"index.toml","size":1},{"name":"index.toml.sig","size":64},
                    {"name":"aterm-machines.toml","size":1},{"name":"aterm-machines.toml.sig","size":64}]},
                {"tag_name":"atpkg-index-49","prerelease":true,"draft":false,"assets":[{"name":"index.toml","size":1}]}]"#,
    )
    .unwrap();
    (releases, served)
}

fn cache_layout(root: &aterm_tempfile::TempDir) -> Layout {
    Layout {
        prefix: root.path().join("pkg"),
    }
}

fn corrupt_objects(path: &Path) {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                corrupt_objects(&path);
            } else {
                std::fs::write(path, b"corrupt").unwrap();
            }
        }
    }
}

#[test]
fn the_fixture_index_is_full_on_arm_and_partial_on_intel() {
    let files = read_dir(&fixture());
    assert_eq!(
        levels_of(&files).unwrap(),
        [Coverage::Partial, Coverage::Full]
    );
}

#[test]
fn a_per_target_overlay_is_what_that_triple_sees() {
    let mut files = read_dir(&fixture());
    files
        .get_mut("index.toml")
        .unwrap()
        .push_str("\n[channels.pin_by_target.\"x86_64-apple-darwin\"]\ntrust = 6808\n");
    assert_eq!(levels_of(&files).unwrap(), [Coverage::Full, Coverage::Full]);
}

#[test]
fn no_pinned_program_for_an_architecture_is_none() {
    let mut files = read_dir(&fixture());
    let index = files["index.toml"]
        .lines()
        .map(|l| {
            if l.starts_with("pin = ") {
                "pin = { trust = 9192 }"
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    files.insert("index.toml".into(), index);
    assert_eq!(
        levels_of(&files).unwrap(),
        [Coverage::Absent, Coverage::Full]
    );
}

#[test]
fn a_missing_pinned_manifest_is_a_refusal_never_a_guess() {
    let mut files = read_dir(&fixture());
    files.remove("pkg-claude-2026092601.toml");
    assert_eq!(
        levels_of(&files).unwrap_err(),
        Unknown::Missing {
            manifest: "pkg-claude-2026092601.toml".into(),
            triple: "x86_64-apple-darwin".into()
        }
    );
}

#[test]
fn a_second_fetch_reads_no_exact_body_after_the_first_verified_them() {
    let (releases, served) = signed_fixture();
    let root = aterm_tempfile::tempdir().unwrap();
    let layout = cache_layout(&root);
    let master = public_key(&MASTER);
    let reads = Cell::new(0usize);
    let mut get = |url: &str, _: u64| {
        reads.set(reads.get() + 1);
        served
            .get(url)
            .cloned()
            .ok_or_else(|| format!("not served: {url}"))
    };
    let f = fetch(
        &releases,
        "alabsystems/aterm",
        "file:///dl/",
        Some(&layout),
        &[master.as_str()],
        NOW,
        &mut get,
    )
    .unwrap();
    assert_eq!(reads.get(), 10, "one index quad and three manifest pairs");
    assert_eq!(f.tag, "atpkg-index-50");
    assert_eq!(
        f.manifests.keys().collect::<Vec<_>>(),
        [
            "pkg-ay-8256.toml",
            "pkg-claude-2026092601.toml",
            "pkg-trust-9192.toml"
        ]
    );
    assert_eq!(
        fetched_levels(&f).unwrap(),
        [Coverage::Partial, Coverage::Full]
    );
    let blocked_reads = Cell::new(0usize);
    let mut blocked = |url: &str, _: u64| {
        blocked_reads.set(blocked_reads.get() + 1);
        Err(format!("network blocked: {url}"))
    };
    let warm = fetch(
        &releases,
        "alabsystems/aterm",
        "file:///dl/",
        Some(&layout),
        &[master.as_str()],
        NOW,
        &mut blocked,
    )
    .unwrap();
    assert_eq!(blocked_reads.get(), 0);
    assert_eq!(
        fetched_levels(&warm).unwrap(),
        [Coverage::Partial, Coverage::Full]
    );
}

#[test]
fn a_corrupt_cached_object_returns_to_the_network_and_refuses_a_mismatch() {
    let (releases, served) = signed_fixture();
    let root = aterm_tempfile::tempdir().unwrap();
    let layout = cache_layout(&root);
    let master = public_key(&MASTER);
    let mut get = |url: &str, _: u64| {
        served
            .get(url)
            .cloned()
            .ok_or_else(|| format!("not served: {url}"))
    };
    fetch(
        &releases,
        "alabsystems/aterm",
        "file:///dl/",
        Some(&layout),
        &[master.as_str()],
        NOW,
        &mut get,
    )
    .unwrap();
    corrupt_objects(&layout.prefix.join("signed-artifacts/v2/objects"));
    let mut mismatched = served;
    let index_url = "file:///dl/alabsystems/aterm/releases/download/atpkg-index-50/index.toml";
    mismatched.insert(
        index_url.into(),
        b"signed bytes changed in transit".to_vec(),
    );
    let reads = Cell::new(0usize);
    let mut get_mismatch = |url: &str, _: u64| {
        reads.set(reads.get() + 1);
        mismatched
            .get(url)
            .cloned()
            .ok_or_else(|| format!("not served: {url}"))
    };
    let refused = fetch(
        &releases,
        "alabsystems/aterm",
        "file:///dl/",
        Some(&layout),
        &[master.as_str()],
        NOW,
        &mut get_mismatch,
    )
    .unwrap_err();
    assert_eq!(reads.get(), 4, "the invalid index quad was reacquired");
    assert!(refused.to_string().contains("signed index chain refused"));
}

#[test]
fn no_complete_signed_index_is_no_index() {
    let root = aterm_tempfile::tempdir().unwrap();
    let layout = cache_layout(&root);
    let master = public_key(&MASTER);
    let none = parse_releases(r#"[{"tag_name":"v0.63.0","assets":[]}]"#).unwrap();
    let mut blocked = |url: &str, _: u64| Err(format!("unexpected network read: {url}"));
    assert_eq!(
        fetch(
            &none,
            "alabsystems/aterm",
            "file:///dl",
            Some(&layout),
            &[master.as_str()],
            NOW,
            &mut blocked,
        )
        .err(),
        Some(Unknown::NoIndex {
            slug: "alabsystems/aterm".into()
        })
    );
}
