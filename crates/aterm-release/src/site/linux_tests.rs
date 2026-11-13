// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::site::releases::{Asset, LINUX_ARCHES, elect};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use std::collections::BTreeMap;

const SLUG: &str = "alabsystems/aterm";
const NOW: i64 = 1_785_801_600;
const MASTER: [u8; 32] = [0x61; 32];
const MACHINE: [u8; 32] = [0x62; 32];
const OTHER: [u8; 32] = [0x63; 32];

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}

fn pk(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(kp(seed).public_key().as_ref()).unwrap()
}

fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
    kp(seed).sign(bytes).as_ref().to_vec()
}

fn roster() -> Vec<u8> {
    format!(
        "schema = 1\nroster_seq = 3\nvalid_until = \"9999-12-31T23:59:59Z\"\nrevoked = []\n\n\
         [[machine]]\nid = \"m3\"\npubkey = \"{}\"\nadded_at = \"2026-08-01T00:00:00Z\"\n",
        pk(&MACHINE)
    )
    .into_bytes()
}

fn appcast(v: &str, build: u64, min: u64, size: u64) -> Vec<u8> {
    format!(
        "schema = 1\nversion = \"{v}\"\nbuild_number = {build}\ncommit = \"{c}\"\n\
         dmg = \"aterm-{v}.dmg\"\nsha256 = \"{h}\"\n\
         url = \"https://github.com/{SLUG}/releases/download/v{v}/aterm-{v}.dmg\"\n\
         linux_aarch64 = \"aterm-{v}-linux-aarch64\"\nlinux_aarch64_sha256 = \"{h}\"\n\
         linux_aarch64_size = {size}\nmin_build = {min}\nmachine_id = \"m3\"\nroster_seq = 3\n",
        c = "a".repeat(40),
        h = "b".repeat(64),
    )
    .into_bytes()
}

fn release(
    served: &mut BTreeMap<String, Vec<u8>>,
    v: &str,
    cast: &[u8],
    signer: &[u8; 32],
    listed: u64,
    current: bool,
) -> Release {
    let tag = format!("v{v}");
    let proof = if current {
        "aterm-appcast.toml".to_string()
    } else {
        format!("aterm-appcast-{tag}.toml")
    };
    let mut names = vec![
        (format!("aterm-{v}.dmg"), 1),
        (format!("aterm-{v}-mac.zip"), 1),
        (format!("aterm-{v}-linux-aarch64"), listed),
        (proof.clone(), cast.len() as u64),
        (format!("{proof}.sig"), 64),
    ];
    let url = |n: &str| format!("https://github.com/{SLUG}/releases/download/{tag}/{n}");
    served.insert(url(&proof), cast.to_vec());
    served.insert(url(&format!("{proof}.sig")), sign(signer, cast));
    if current {
        let r = roster();
        served.insert(url("aterm-machines.toml.sig"), sign(&MASTER, &r));
        served.insert(url("aterm-machines.toml"), r);
        names.push(("aterm-machines.toml".into(), 1));
        names.push(("aterm-machines.toml.sig".into(), 64));
    }
    Release {
        tag,
        prerelease: false,
        draft: false,
        date: String::new(),
        body: String::new(),
        assets: names
            .into_iter()
            .map(|(name, size)| Asset {
                name,
                size: Some(size),
            })
            .collect(),
    }
}

fn run(
    served: &BTreeMap<String, Vec<u8>>,
    releases: &[Release],
) -> Result<Option<LinuxDownload>, String> {
    elected(served, releases).map(|f| f.linux(LINUX_ARCHES[1]).cloned())
}

fn elected(
    served: &BTreeMap<String, Vec<u8>>,
    releases: &[Release],
) -> Result<crate::site::releases::Facts, String> {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let mut get = |url: &str, _limit: u64| {
        served
            .get(url)
            .cloned()
            .ok_or_else(|| format!("not served: {url}"))
    };
    let mut chain = Chain {
        slug: SLUG,
        masters: &masters,
        now_unix: NOW,
        artifact_layout: None,
        get: &mut get,
    };
    elect(releases, SLUG, &mut chain, &mut Vec::new()).map_err(|e| e.to_string())
}

fn prove_cached(
    layout: &Layout,
    served: &BTreeMap<String, Vec<u8>>,
    release: &Release,
) -> (Result<LinuxDownload, String>, usize) {
    let master = pk(&MASTER);
    let masters = [master.as_str()];
    let mut reads = 0;
    let result = {
        let mut get = |url: &str, _limit: u64| {
            reads += 1;
            served
                .get(url)
                .cloned()
                .ok_or_else(|| format!("blocked transport: {url}"))
        };
        let mut chain = Chain {
            slug: SLUG,
            masters: &masters,
            now_unix: NOW,
            artifact_layout: Some(layout),
            get: &mut get,
        };
        chain.prove(release, "aarch64", release)
    };
    (result, reads)
}

fn corrupt_cached_objects(layout: &Layout) {
    let objects = layout.prefix.join("signed-artifacts/v2/objects/sha256");
    for shard in std::fs::read_dir(objects).unwrap() {
        for object in std::fs::read_dir(shard.unwrap().path()).unwrap() {
            std::fs::write(object.unwrap().path(), b"partial object").unwrap();
        }
    }
}

#[test]
fn a_signed_executable_of_the_head_is_linked_with_its_digest_and_size() {
    let mut served = BTreeMap::new();
    let head = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9000,
        true,
    );
    let d = run(&served, std::slice::from_ref(&head)).unwrap().unwrap();
    assert_eq!(d.label, "aterm-0.64.0-linux-aarch64");
    assert_eq!(
        d.kind,
        LinuxKind::SignedRaw {
            sha256: "b".repeat(64),
            size: 9000
        }
    );
    let facts = elected(&served, &[head]).unwrap();
    assert_eq!(
        crate::site::pages::linux_install_command(&facts, "aarch64").as_deref(),
        Some("curl -fsSL https://alab.systems/install.sh | bash")
    );
    assert_eq!(
        crate::site::pages::linux_install_command(&facts, "x86_64"),
        None
    );
}

#[test]
fn an_older_executable_is_authorized_under_the_current_policy() {
    let mut served = BTreeMap::new();
    let mut head_cast = String::from_utf8(appcast("0.65.0", 21, 0, 9000)).unwrap();
    head_cast = head_cast
        .lines()
        .filter(|l| !l.starts_with("linux_"))
        .map(|l| format!("{l}\n"))
        .collect();
    let mut head = release(
        &mut served,
        "0.65.0",
        head_cast.as_bytes(),
        &MACHINE,
        1,
        true,
    );
    head.assets
        .retain(|a| a.name != "aterm-0.65.0-linux-aarch64");
    let old = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9000,
        false,
    );
    let d = run(&served, &[head.clone(), old.clone()]).unwrap().unwrap();
    assert_eq!(d.tag, "v0.64.0");
    let facts = elected(&served, &[head.clone(), old]).unwrap();
    assert_eq!(facts.head, "v0.65.0");
    assert_eq!(
        crate::site::pages::linux_install_command(&facts, "aarch64").as_deref(),
        Some("curl -fsSL https://alab.systems/install.sh | bash -s -- --version 0.64.0")
    );
    let floor_cast = head_cast.replace("min_build = 0", "min_build = 21");
    let mut served2 = BTreeMap::new();
    let mut head = release(
        &mut served2,
        "0.65.0",
        floor_cast.as_bytes(),
        &MACHINE,
        1,
        true,
    );
    head.assets
        .retain(|a| a.name != "aterm-0.65.0-linux-aarch64");
    let old = release(
        &mut served2,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9000,
        false,
    );
    let err = run(&served2, &[head, old]).unwrap_err();
    assert!(err.contains("below apply floor"), "{err}");
}

#[test]
fn a_signature_by_no_rostered_machine_refuses_the_sync() {
    let mut served = BTreeMap::new();
    let head = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &OTHER,
        9000,
        true,
    );
    let err = run(&served, &[head]).unwrap_err();
    assert!(
        err.contains("v0.64.0 Linux aarch64 signature/asset verification refused"),
        "{err}"
    );
    assert!(
        err.contains("no authorized live machine signature"),
        "{err}"
    );
}

#[test]
fn a_listed_size_the_appcast_does_not_sign_refuses_the_sync() {
    let mut served = BTreeMap::new();
    let head = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9001,
        true,
    );
    let err = run(&served, &[head]).unwrap_err();
    assert!(err.contains("does not match release inventory"), "{err}");
}

#[test]
fn a_roster_under_another_master_refuses_the_sync() {
    let mut served = BTreeMap::new();
    let head = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9000,
        true,
    );
    let url =
        format!("https://github.com/{SLUG}/releases/download/v0.64.0/aterm-machines.toml.sig");
    served.insert(url, sign(&OTHER, &roster()));
    let err = run(&served, &[head]).unwrap_err();
    assert!(
        err.contains("not signed by the pinned paper master"),
        "{err}"
    );
}

#[test]
fn a_second_proof_with_blocked_transport_reverifies_exact_cached_pairs() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = Layout {
        prefix: scratch.path().join("pkg"),
    };
    let mut served = BTreeMap::new();
    let head = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9000,
        true,
    );
    let (first, reads) = prove_cached(&layout, &served, &head);
    first.unwrap();
    assert_eq!(reads, 4, "one document/signature pair for each proof");

    let (second, reads) = prove_cached(&layout, &BTreeMap::new(), &head);
    second.expect("content-addressed pairs are reverified without transport");
    assert_eq!(reads, 0, "an exact verified pair makes no network read");
}

#[test]
fn a_corrupt_cached_pair_reads_transport_and_refuses_a_mismatch() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let layout = Layout {
        prefix: scratch.path().join("pkg"),
    };
    let mut served = BTreeMap::new();
    let head = release(
        &mut served,
        "0.64.0",
        &appcast("0.64.0", 20, 0, 9000),
        &MACHINE,
        9000,
        true,
    );
    prove_cached(&layout, &served, &head).0.unwrap();
    corrupt_cached_objects(&layout);

    let appcast_url =
        format!("https://github.com/{SLUG}/releases/download/v0.64.0/aterm-appcast.toml");
    served.insert(appcast_url, appcast("0.64.0", 21, 0, 9000));
    let (result, reads) = prove_cached(&layout, &served, &head);
    let error = result.unwrap_err();
    assert!(reads >= 4, "both corrupt pairs must fall back to transport");
    assert!(
        error.contains("no authorized live machine signature"),
        "{error}"
    );
}
