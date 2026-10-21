// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One master, one roster, both products: a paper master mints a machine key, and that one
//! machine on one roster signs both an aterm release manifest and a complete atpkg
//! toolchain registry (`index.toml`, `pkg-*.toml` and a real `.tar.zst`). The actual client
//! verifiers (`aterm_update_core::roster`, `atpkg::flow::install`) accept both, attributed to
//! the same machine, and one revocation stops both. Shipping types on both sides; no network.

// Both sides are unix-gated crates, so this proof is too.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aterm_update_core::roster::{Roster, RosterReject, verify_roster};
use atpkg::Candidate;
use atpkg::flow::{Fetcher, InstallRequest};
use atpkg_keys::roster_ops::{add, empty, revoke};

const TRIPLE: &str = "aarch64-apple-darwin";

// The master is generated per run, not parsed from a phrase: the phrase-to-seed leg is
// proved in `master.rs` and `paper_master_to_client.rs`, and a generated key keeps a
// phrase-shaped literal off what the census `key-material` guard reads.

/// 2026-08-04T00:00:00Z.
const NOW: i64 = 1_785_801_600;

/// A test's own directory, removed when the test ends, failed or not.
fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atpkg-o2c-{label}-"))
        .tempdir()
        .unwrap()
}

/// A raw USTAR + zstd archive holding `bin/ay`.
fn make_archive(dir: &Path) -> PathBuf {
    let content = b"#!/bin/true\nthe ay binary";
    let mut h = [0u8; 512];
    h[..6].copy_from_slice(b"bin/ay");
    h[100..108].copy_from_slice(b"0000755\0");
    h[108..116].copy_from_slice(b"0000000\0");
    h[116..124].copy_from_slice(b"0000000\0");
    h[124..136].copy_from_slice(format!("{:011o}\0", content.len()).as_bytes());
    h[136..148].copy_from_slice(b"00000000000\0");
    h[148..156].copy_from_slice(b"        ");
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    let mut tar = h.to_vec();
    tar.extend_from_slice(content);
    tar.resize(tar.len() + (512 - content.len() % 512) % 512, 0);
    tar.resize(tar.len() + 1024, 0);
    let path = dir.join("ay-18.tar.zst");
    let mut f = std::fs::File::create(&path).unwrap();
    aterm_zstd::frame::write_raw(&mut f, &tar, aterm_zstd::frame::Checksum::Xxh64).unwrap();
    path
}

/// The fake network: a published atpkg release (the signed index, the master-signed roster
/// that authorized its signer, the per-build manifests and the artifacts).
struct Registry {
    index: (Vec<u8>, Vec<u8>),
    roster: (Vec<u8>, Vec<u8>),
    pkg: HashMap<(String, u64), (Vec<u8>, Vec<u8>)>,
    archives: HashMap<String, PathBuf>,
}

impl Fetcher for Registry {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        Ok(vec![Candidate {
            label: "atpkg-index-1".into(),
            index_bytes: self.index.0.clone(),
            sig: self.index.1.clone(),
            roster_bytes: self.roster.0.clone(),
            roster_sig: self.roster.1.clone(),
        }])
    }
    fn pkg_manifest(
        &self,
        _repo: &str,
        program: &str,
        build: u64,
    ) -> Result<(Vec<u8>, Vec<u8>), String> {
        self.pkg
            .get(&(program.to_string(), build))
            .cloned()
            .ok_or_else(|| "no manifest".into())
    }
    fn download(&self, _repo: &str, asset: &str, dest: &Path) -> Result<(), String> {
        let src = self.archives.get(asset).ok_or("no asset")?;
        std::fs::copy(src, dest)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// The bytes of an aterm release manifest as the cutter emits them, carrying the two
/// attribution keys. Signed as raw bytes, exactly as the client verifies them.
fn appcast(machine_id: &str, roster_seq: u64) -> Vec<u8> {
    let mut s = String::from("schema = 1\nversion = \"0.99.0\"\nbuild_number = 990\n");
    s.push_str("dmg = \"aterm-0.99.0.dmg\"\nsha256 = \"");
    s.push_str(&"ab".repeat(32));
    s.push_str("\"\nmachine_id = \"");
    s.push_str(machine_id);
    s.push_str("\"\nroster_seq = ");
    s.push_str(&roster_seq.to_string());
    s.push('\n');
    s.into_bytes()
}

/// Everything the owner produces in one publishing session.
struct Published {
    master_pub: String,
    machine_id: String,
    machine_pub: String,
    roster: Roster,
    roster_bytes: Vec<u8>,
    roster_sig: Vec<u8>,
    machine_key: Vec<u8>,
    registry: Registry,
    appcast: Vec<u8>,
    appcast_sig: Vec<u8>,
}

/// Mint a machine from a master, roster it, and publish both products under it. `revoked`
/// names machines the roster withdraws.
fn publish(dir: &Path, machine_id: &str, revoked: &[&str]) -> Published {
    // --- the owner side, through the shipping tool ----------------------------------
    // The paper master (here a per-run keypair) and the machine key it authorizes.
    let (master_key, master_pub) = atpkg_keys::generate().expect("paper master keypair");
    let (machine_key, machine_pub) = atpkg_keys::generate().expect("machine keypair");
    let mut roster = add(empty(NOW as u64), machine_id, &machine_pub, NOW as u64)
        .expect("the machine joins the roster");
    for id in revoked {
        roster = revoke(roster, id, NOW as u64).expect("revocation applies");
    }
    let roster_bytes = roster.to_toml().expect("a valid roster emits").into_bytes();
    let roster_sig =
        atpkg_keys::sign(&master_key, &roster_bytes).expect("the master signs the roster");

    // --- product 1: an aterm release manifest, signed by the machine ------------------
    let appcast = appcast(machine_id, roster.roster_seq);
    let appcast_sig = atpkg_keys::sign(&machine_key, &appcast).expect("the machine signs it");

    // --- product 2: a complete atpkg registry, signed by the same machine -------------
    let archive = make_archive(dir);
    let sha = atpkg::sha256_file(&archive).unwrap();
    let probe = dir.join("probe");
    let _ = std::fs::remove_dir_all(&probe);
    atpkg::extract_tar_zst(&archive, &probe, 10_000_000, 10_000).unwrap();
    let tree = atpkg::tree_root(&probe).unwrap();

    let pkg_body = format!(
        "schema = 2\nprogram = \"ay\"\nversion = \"0.1\"\nbuild_number = 18\nexposes = [\"ay\"]\n\
         [[artifact]]\ntarget = \"{TRIPLE}\"\nkind = \"binary\"\nasset = \"ay-18.tar.zst\"\n\
         sha256 = \"{sha}\"\ntree_root = \"{tree}\"\nsize = 100\n\
         [artifact.cost]\ndisk_installed = 1048576\n"
    );
    let pkg_sig = atpkg_keys::sign(&machine_key, pkg_body.as_bytes()).expect("machine signs pkg");

    // No `[keys]` table: the index names which machine cut it and which roster generation
    // authorized that machine, and the roster says who may sign.
    let index_body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{machine_id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\npin = {{ ay = 18 }}\n",
        seq = roster.roster_seq
    );
    let index_sig =
        atpkg_keys::sign(&machine_key, index_body.as_bytes()).expect("machine signs index");

    let mut pkg = HashMap::new();
    pkg.insert(("ay".to_string(), 18u64), (pkg_body.into_bytes(), pkg_sig));
    let mut archives = HashMap::new();
    archives.insert("ay-18.tar.zst".to_string(), archive);

    Published {
        master_pub,
        machine_id: machine_id.to_string(),
        machine_pub,
        roster,
        roster_bytes: roster_bytes.clone(),
        roster_sig,
        machine_key,
        registry: Registry {
            index: (index_body.into_bytes(), index_sig),
            roster: (roster_bytes, Vec::new()), // filled in by the caller (see below)
            pkg,
            archives,
        },
        appcast,
        appcast_sig,
    }
}

/// `publish` moves the roster signature, so the caller stitches it into the registry here.
fn with_roster_sig(mut p: Published) -> Published {
    p.registry.roster.1 = p.roster_sig.clone();
    p
}

fn store_at(dir: &Path, name: &str) -> atpkg::store::Layout {
    atpkg::store::Layout {
        prefix: dir.join(name),
    }
}

fn request() -> InstallRequest<'static> {
    InstallRequest {
        channel: "stable",
        program: "ay",
        triple: TRIPLE,
        installed: None,
    }
}

/// One master, one roster, one machine: a release and a toolchain index both pass their
/// real client verifiers, attributed to that machine.
#[test]
fn one_master_and_one_roster_authorize_both_a_release_and_a_toolchain_index() {
    let tmp = scratch("both");
    let dir = tmp.path();
    let p = with_roster_sig(publish(dir, "m3", &[]));

    // ---- product 1: the aterm release, through `aterm_update_core::roster` -----------
    let verified = verify_roster(&[&p.master_pub], p.roster_bytes.clone(), &p.roster_sig)
        .expect("the pinned paper master verifies the roster");
    let parsed = Roster::parse(&verified).expect("the roster parses");
    parsed
        .admit(0, NOW)
        .expect("fresh, and above a first-contact floor");
    let who = parsed
        .authorize_appcast(&p.appcast, &p.appcast_sig, NOW)
        .expect("m3 is live and signed the release");
    assert_eq!(who.machine_id, "m3");
    assert_eq!(who.pubkey_b64, p.machine_pub);
    who.bind(Some("m3"), Some(parsed.roster_seq))
        .expect("the manifest's own claim agrees with the key that signed");

    // ---- product 2: the atpkg toolchain, through the real install flow ---------------
    // The anchor is the same master key the release chain used, not a second root.
    let anchor = atpkg::Anchor::of(vec![p.master_pub.clone()], 0);
    let layout = store_at(dir, "prefix");
    let report = atpkg::install(
        &p.registry,
        &layout,
        &anchor,
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect("the toolchain installs under the very same paper master");
    assert_eq!(report.build, 18);
    assert_eq!(report.roster_seq, parsed.roster_seq);
    assert_eq!(report.shimmed, vec!["ay".to_string()]);
    assert_eq!(
        atpkg::which(&layout, "ay").unwrap(),
        layout.build_dir("ay", 18).join("bin/ay")
    );

    // The atpkg anchor is the paper master itself, not a package root.
    assert!(
        anchor.is_armed(),
        "precondition: the atpkg anchor is armed with the master, not a package root"
    );
}

/// Every byte is genuinely signed and only the anchor is missing, as in an unpinned fork.
/// The test passes its own empty anchor and never reads `pins`.
#[test]
fn an_unpinned_client_installs_nothing_although_every_byte_is_genuine() {
    let tmp = scratch("inert");
    let dir = tmp.path();
    let p = with_roster_sig(publish(dir, "m3", &[]));
    let layout = store_at(dir, "prefix");

    let inert = atpkg::Anchor::of(vec![], 0);
    assert!(!inert.is_armed(), "precondition: nothing is pinned");
    let err = atpkg::install(
        &p.registry,
        &layout,
        &inert,
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("an unpinned client must install NOTHING, not everything");
    assert!(
        matches!(err, atpkg::FlowError::NoIndex),
        "the refusal is 'no index I can trust', not a permissive fallthrough: {err:?}"
    );
    assert!(
        atpkg::which(&layout, "ay").is_none(),
        "and nothing landed on disk"
    );

    // Negative control: the same registry installs once the real master is pinned.
    let armed = atpkg::Anchor::of(vec![p.master_pub.clone()], 0);
    assert!(
        atpkg::install(
            &p.registry,
            &layout,
            &armed,
            &request(),
            atpkg::BuildFloor::none(),
            NOW
        )
        .is_ok(),
        "the same bytes install under the pinned master"
    );

    // The shipped anchor is armed; the unpinned case above used a synthetic empty anchor.
    assert!(!atpkg::PKG_TRUST_ANCHORS.is_empty());
    assert!(atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS));
}

/// There is no older root to fall back to, so a wrong master ends the road rather than
/// downgrading.
#[test]
fn a_client_pinned_to_a_different_master_installs_nothing() {
    let tmp = scratch("wrong-master");
    let dir = tmp.path();
    let p = with_roster_sig(publish(dir, "m3", &[]));
    let layout = store_at(dir, "prefix");

    let (_other_key, other_pub) = atpkg_keys::generate().unwrap();
    assert_ne!(other_pub, p.master_pub, "the fixture must actually differ");
    let err = atpkg::install(
        &p.registry,
        &layout,
        &atpkg::Anchor::of(vec![other_pub], 0),
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("a registry not authorized by the pinned master must not install");
    assert!(matches!(err, atpkg::FlowError::NoIndex), "{err:?}");
}

/// The machine's key and signatures are genuine and both products byte-identical to the
/// ones that installed above; only the roster's deny-list differs, and both verifiers
/// refuse, the machine leaving the candidate set before any signature is checked.
#[test]
fn revoking_one_machine_stops_the_release_and_the_toolchain_at_once() {
    let tmp = scratch("revoked");
    let dir = tmp.path();
    let p = with_roster_sig(publish(dir, "m3", &["m3"]));
    let layout = store_at(dir, "prefix");
    let anchor = atpkg::Anchor::of(vec![p.master_pub.clone()], 0);

    // A revocation, not a never-minted machine: `revoke` drops the entry and names the id on
    // the deny-list, which a client with an older roster still refuses on. (The deny beating
    // a still-listed entry is proved in `atpkg::sig`'s tests.)
    assert!(
        p.roster.is_revoked("m3"),
        "precondition: m3 is on the deny-list, so this tests revocation and not absence"
    );
    assert!(
        p.roster.live(NOW).is_empty(),
        "a revoked machine leaves the candidate set BEFORE any crypto"
    );

    // Product 1 — the aterm release: refused.
    let verified = verify_roster(&[&p.master_pub], p.roster_bytes.clone(), &p.roster_sig).unwrap();
    let parsed = Roster::parse(&verified).unwrap();
    parsed.admit(0, NOW).unwrap();
    assert_eq!(
        parsed.machine("m3", NOW).err(),
        Some(RosterReject::Revoked),
        "the direct lookup names the reason"
    );
    assert!(
        parsed
            .authorize_appcast(&p.appcast, &p.appcast_sig, NOW)
            .is_err(),
        "a revoked machine's valid release signature is not accepted"
    );

    // Product 2 — the atpkg toolchain: refused, through the real install flow.
    let err = atpkg::install(
        &p.registry,
        &layout,
        &anchor,
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("a revoked machine's index must not install");
    assert!(matches!(err, atpkg::FlowError::NoIndex), "{err:?}");
    assert!(atpkg::which(&layout, "ay").is_none(), "nothing landed");

    // Negative control: the same machine key under a roster that does not revoke it
    // installs, so the refusals above are the revocation.
    let tmp2 = scratch("revoked-control");
    let dir2 = tmp2.path();
    let ok = with_roster_sig(publish(dir2, "m3", &[]));
    let ok_layout = store_at(dir2, "prefix");
    assert!(
        atpkg::install(
            &ok.registry,
            &ok_layout,
            &atpkg::Anchor::of(vec![ok.master_pub.clone()], 0),
            &request(),
            atpkg::BuildFloor::none(),
            NOW
        )
        .is_ok()
    );
    assert!(!ok.machine_key.is_empty(), "the control used a real key");
}

/// A replayed roster generation is refused once a newer one has been durably seen (the
/// `Anchor` carries the client's floor). A pre-revocation roster's master signature never
/// expires, so the ratchet is what keeps revocation meaningful.
#[test]
fn a_pre_revocation_roster_cannot_be_replayed_past_the_ratchet() {
    let tmp = scratch("replay");
    let dir = tmp.path();
    let p = with_roster_sig(publish(dir, "m3", &[]));
    let layout = store_at(dir, "prefix");

    // Its master signature still verifies — that is exactly why the floor exists.
    assert!(
        verify_roster(&[&p.master_pub], p.roster_bytes.clone(), &p.roster_sig).is_ok(),
        "precondition: the old roster is still cryptographically genuine"
    );
    let seq = p.roster.roster_seq;

    // At or below the floor it installs; one generation past it, never again.
    assert!(
        atpkg::install(
            &p.registry,
            &layout,
            &atpkg::Anchor::of(vec![p.master_pub.clone()], seq),
            &request(),
            atpkg::BuildFloor::none(),
            NOW
        )
        .is_ok(),
        "seq == floor is the current generation, not an attack"
    );
    let fresh = store_at(dir, "prefix2");
    let err = atpkg::install(
        &p.registry,
        &fresh,
        &atpkg::Anchor::of(vec![p.master_pub.clone()], seq + 1),
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("a client that has seen a newer generation refuses this one");
    assert!(matches!(err, atpkg::FlowError::NoIndex), "{err:?}");
    assert!(atpkg::which(&fresh, "ay").is_none());
}

/// An index signed by the rostered machine but claiming another machine's name is refused
/// at the bind, over already-authenticated bytes.
#[test]
fn an_index_cannot_wear_another_machines_name() {
    let tmp = scratch("relabel");
    let dir = tmp.path();
    let mut p = with_roster_sig(publish(dir, "m3", &[]));
    let layout = store_at(dir, "prefix");

    // Re-cut the index claiming `m11`, signed with m3's real key: the signature is genuine,
    // only the claim is false.
    let body = format!(
        "schema = 2\nindex_build = 41\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"m11\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\npin = {{ ay = 18 }}\n",
        seq = p.roster.roster_seq
    );
    let sig = atpkg_keys::sign(&p.machine_key, body.as_bytes()).unwrap();
    p.registry.index = (body.into_bytes(), sig);

    let err = atpkg::install(
        &p.registry,
        &layout,
        &atpkg::Anchor::of(vec![p.master_pub.clone()], 0),
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("a genuine signature must not be wearable under another machine's name");
    assert!(matches!(err, atpkg::FlowError::NoIndex), "{err:?}");
    assert_eq!(p.machine_id, "m3", "the key that signed really is m3's");
}

// Revocation must land: a revoked machine cannot out-publish the generation revoking it, and
// merely seeing that generation is durable even when nothing installs under it.

/// Several candidate releases and no artifacts: enough to drive the real selection through
/// `atpkg::install`, which then fails downstream.
struct Releases(Vec<Candidate>);

impl Fetcher for Releases {
    fn index_candidates(&self) -> Result<Vec<Candidate>, String> {
        Ok(self.0.clone())
    }
    fn pkg_manifest(&self, _: &str, _: &str, _: u64) -> Result<(Vec<u8>, Vec<u8>), String> {
        Err("no manifest published".into())
    }
    fn download(&self, _: &str, _: &str, _: &Path) -> Result<(), String> {
        Err("no asset published".into())
    }
}

/// Two machines minted from one paper master, and the rosters that name them.
struct TwoMachines {
    master_pub: String,
    master_key: Vec<u8>,
    m3: Vec<u8>,
    m3_pub: String,
    m11: Vec<u8>,
    m11_pub: String,
}

fn two_machines() -> TwoMachines {
    let (master_key, master_pub) = atpkg_keys::generate().unwrap();
    let (m3, m3_pub) = atpkg_keys::generate().unwrap();
    let (m11, m11_pub) = atpkg_keys::generate().unwrap();
    TwoMachines {
        master_pub,
        master_key,
        m3,
        m3_pub,
        m11,
        m11_pub,
    }
}

/// A master-signed roster naming both machines, with `revoked` withdrawn. Every
/// `add`/`revoke` bumps `roster_seq`, so a roster with a revocation is the newer generation.
fn both_rostered(o: &TwoMachines, revoked: &[&str]) -> (Roster, Vec<u8>, Vec<u8>) {
    let mut r = add(empty(NOW as u64), "m3", &o.m3_pub, NOW as u64).unwrap();
    r = add(r, "m11", &o.m11_pub, NOW as u64).unwrap();
    for id in revoked {
        r = revoke(r, id, NOW as u64).unwrap();
    }
    let bytes = r.to_toml().expect("a valid roster emits").into_bytes();
    let sig = atpkg_keys::sign(&o.master_key, &bytes).unwrap();
    (r, bytes, sig)
}

/// One published release: an index at `index_build`, signed by `signer` claiming
/// `machine_id`, published beside `roster`.
fn release(
    label: &str,
    roster: &(Roster, Vec<u8>, Vec<u8>),
    signer: &[u8],
    machine_id: &str,
    index_build: u64,
) -> Candidate {
    let body = format!(
        "schema = 2\nindex_build = {index_build}\nvalid_until = \"2099-01-01T00:00:00Z\"\n\
         machine_id = \"{machine_id}\"\nroster_seq = {seq}\n\
         [programs.ay]\nrepo = \"ay\"\n\
         [[channels]]\nname = \"stable\"\nchannel_build = 1\nmin_build = 0\npin = {{ ay = 18 }}\n",
        seq = roster.0.roster_seq
    )
    .into_bytes();
    let sig = atpkg_keys::sign(signer, &body).unwrap();
    Candidate {
        label: label.into(),
        index_bytes: body,
        sig,
        roster_bytes: roster.1.clone(),
        roster_sig: roster.2.clone(),
    }
}

/// The thief holds m11's key but cannot mint a roster, so they publish a huge `index_build`
/// beside the last roster listing them while the owner publishes the revoking generation at
/// a modest build. Both arrive in one fetch (the roster travels per candidate); choosing by
/// generation before `index_build` keeps the thief from outbidding the revocation.
#[test]
fn a_revoked_machine_cannot_outbid_the_generation_that_revokes_it() {
    let tmp = scratch("outbid");
    let dir = tmp.path();
    let o = two_machines();
    let before = both_rostered(&o, &[]);
    let after = both_rostered(&o, &["m11"]);
    assert!(
        after.0.roster_seq > before.0.roster_seq,
        "precondition: revoking produces a strictly NEWER generation ({} > {})",
        after.0.roster_seq,
        before.0.roster_seq
    );
    assert!(
        before.0.machine("m11", NOW).is_ok(),
        "precondition: m11 really is authorized on the older generation, so the thief's \
         index is genuinely signed by a then-live machine"
    );

    // The thief bids 100; the owner's revoking release is a modest 50.
    let fetcher = Releases(vec![
        release("thief-m11", &before, &o.m11, "m11", 100),
        release("owner-revoking", &after, &o.m3, "m3", 50),
    ]);
    let layout = store_at(dir, "prefix");
    let anchor = atpkg::Anchor::of(vec![o.master_pub.clone()], 0);
    let index =
        atpkg::resolve_verified_index(&fetcher, &layout, &anchor, atpkg::BuildFloor::none(), NOW)
            .expect("the owner's release is perfectly valid");
    assert_eq!(
        index.attribution().machine_id,
        "m3",
        "the newest master-signed generation decides who may sign — not the biggest \
         index_build, which is a number the signer picks"
    );
    assert_eq!(index.index_build, 50);
    assert_eq!(index.roster_seq(), after.0.roster_seq);

    // Negative control: served alone to a client that has not seen the revocation, the
    // thief's release is selected, with the higher build.
    let alone = store_at(dir, "alone");
    let solo = atpkg::resolve_verified_index(
        &Releases(vec![release("thief-m11", &before, &o.m11, "m11", 100)]),
        &alone,
        &atpkg::Anchor::of(vec![o.master_pub.clone()], 0),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect("it is a genuine release of the older generation");
    assert_eq!(solo.index_build, 100);
    assert_eq!(solo.attribution().machine_id, "m11");
}

/// The client verifies and admits the revoking generation, then installs nothing (no
/// manifests, as with a held pin, a failed download or a no-op plan). The ratchet turns on
/// observation, not on a completed install: otherwise the still-genuine pre-revocation
/// roster would re-authorize the revoked machine on the next pass.
#[test]
fn observing_a_revocation_is_durable_even_when_nothing_installs() {
    let tmp = scratch("observe");
    let dir = tmp.path();
    let o = two_machines();
    let before = both_rostered(&o, &[]);
    let after = both_rostered(&o, &["m11"]);
    let layout = store_at(dir, "prefix");
    std::fs::create_dir_all(&layout.prefix).unwrap();

    // Pass 1: the revoking generation is admitted; the install fails downstream for want of
    // a published manifest.
    let err = atpkg::install(
        &Releases(vec![release("owner-revoking", &after, &o.m3, "m3", 50)]),
        &layout,
        &atpkg::Anchor::of(vec![o.master_pub.clone()], 0),
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("nothing can install: the release publishes no pkg manifest");
    assert!(
        !matches!(err, atpkg::FlowError::NoIndex),
        "precondition: the failure is DOWNSTREAM of selection — the index was admitted, \
         verified and parsed. Got {err:?}"
    );
    assert!(atpkg::which(&layout, "ay").is_none(), "and nothing landed");

    // The observation is on disk.
    let recorded = atpkg::Floor::new(layout.roster_floor()).current();
    assert_eq!(
        recorded, after.0.roster_seq,
        "merely SEEING generation {} must ratchet the durable floor to it",
        after.0.roster_seq
    );

    // Pass 2: the attacker serves only the pre-revocation generation (its master signature
    // still valid) with a high-build index from revoked m11. That generation is dead to this
    // client, so nothing is selected and the store keeps the last index it trusted: the
    // owner's, from pass 1.
    let thief = Releases(vec![release("thief-m11", &before, &o.m11, "m11", 999)]);
    let anchor = atpkg::Anchor::of(vec![o.master_pub.clone()], recorded);
    let served =
        atpkg::resolve_verified_index(&thief, &layout, &anchor, atpkg::BuildFloor::none(), NOW)
            .expect("the cached owner's index still serves");
    assert_eq!(
        (
            served.attribution().machine_id.as_str(),
            served.index_build,
            served.roster_seq()
        ),
        ("m3", 50, after.0.roster_seq),
        "the revoked machine's index is never selected; the owner's cached one is"
    );
    let replayed = atpkg::install(
        &thief,
        &layout,
        &anchor,
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("nothing installs: the cached owner's release publishes no manifest");
    assert!(
        !matches!(replayed, atpkg::FlowError::NoIndex),
        "the cached owner's index is selected, so the failure is downstream of it: \
         {replayed:?}"
    );

    // Negative control: on a client that never saw the revocation, the same bytes get past
    // selection.
    let naive = store_at(dir, "naive");
    let naive_err = atpkg::install(
        &Releases(vec![release("thief-m11", &before, &o.m11, "m11", 999)]),
        &naive,
        &atpkg::Anchor::of(vec![o.master_pub.clone()], 0),
        &request(),
        atpkg::BuildFloor::none(),
        NOW,
    )
    .expect_err("still no manifest to install");
    assert!(
        !matches!(naive_err, atpkg::FlowError::NoIndex),
        "a client that never saw the revocation DOES accept the thief's index — which is \
         exactly why the observation has to be durable. Got {naive_err:?}"
    );
}
