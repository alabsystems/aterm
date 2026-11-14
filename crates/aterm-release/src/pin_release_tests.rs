// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::github::Fixture;
use crate::ledger::RunOut;
use agh_fake::{AssetSpec, FakeGithub, ReleaseSpec, RepoSpec};
use aterm_update_core::github_rest::Origins;
use aterm_update_core::linux::LinuxTarget;
use ring::signature::{Ed25519KeyPair, KeyPair as _};

const SLUG: &str = "fixture-pin/aterm";
const TAG: &str = "v0.100.0";
const NOW: i64 = 1_785_801_600;
const MASTER: [u8; 32] = [0x41; 32];
const MACHINE: [u8; 32] = [0x42; 32];
const OTHER: [u8; 32] = [0x43; 32];
const LEDGER: &str = "# ledger\n99 0.99.0\n100 0.100.0\n";

fn kp(seed: &[u8; 32]) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(seed).unwrap()
}

fn pk(seed: &[u8; 32]) -> String {
    aterm_codec::base64::encode(kp(seed).public_key().as_ref()).unwrap()
}

fn sign(seed: &[u8; 32], bytes: &[u8]) -> Vec<u8> {
    kp(seed).sign(bytes).as_ref().to_vec()
}

fn commit() -> String {
    "a".repeat(40)
}

/// A synthetic static verifier for `target` (`interpreter` makes it dynamic), as the
/// handoff tests build one.
fn verifier(target: LinuxTarget, interpreter: bool) -> Vec<u8> {
    let mut bytes = vec![0u8; 64 + 56];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    let machine: u16 = if target == LinuxTarget::Aarch64 {
        183
    } else {
        62
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    let kind: u32 = if interpreter { 3 } else { 1 };
    bytes[64..68].copy_from_slice(&kind.to_le_bytes());
    bytes
}

fn digest(bytes: &[u8]) -> String {
    crate::publish::sha256_bytes(bytes)
}

fn roster(valid_until: &str) -> Vec<u8> {
    format!(
        "schema = 1\nroster_seq = 5\nvalid_until = \"{valid_until}\"\nrevoked = []\n\n\
         [[machine]]\nid = \"m3\"\npubkey = \"{}\"\nadded_at = \"2026-08-01T00:00:00Z\"\n",
        pk(&MACHINE)
    )
    .into_bytes()
}

/// An appcast's head for `version` (build `build`, commit `commit`, floor `min_build`),
/// attributed to the roster's `m3` and naming that release of the channel as its origin.
fn cast(version: &str, build: u64, commit: &str, min_build: u64) -> String {
    format!(
        "schema = 1\nversion = \"{version}\"\nbuild_number = {build}\ncommit = \"{commit}\"\n\
         dmg = \"aterm-{version}.dmg\"\nsha256 = \"{}\"\n\
         url = \"https://github.com/{SLUG}/releases/download/v{version}/aterm-{version}.dmg\"\n\
         min_build = {min_build}\nmachine_id = \"m3\"\nroster_seq = 5\n",
        "b".repeat(64)
    )
}

/// A channel head apart from the pinned release: its version and build, and the apply
/// floor its signed appcast sets.
#[derive(Clone)]
struct Head {
    version: &'static str,
    build: u64,
    min_build: u64,
}

/// The release the fake serves, and how each case bends it.
#[derive(Clone)]
struct Shape {
    /// The verifier bytes the appcast signs, per architecture (`None`: not signed).
    signed: [Option<Vec<u8>>; 2],
    /// The verifier bytes the release serves, per architecture (`None`: not uploaded).
    served: [Option<Vec<u8>>; 2],
    /// The Windows verifier bytes the appcast signs (`None`: not signed).
    windows_signed: Option<Vec<u8>>,
    /// The Windows verifier bytes the release serves (`None`: not uploaded).
    windows_served: Option<Vec<u8>>,
    /// The machine whose key signs the appcast.
    signer: [u8; 32],
    /// The appcast's commit and build.
    commit: String,
    build: u64,
    /// Whether the release is a prerelease (a demoted build).
    prerelease: bool,
    /// Whether the release carries its appcast only as the archived pair.
    archived: bool,
    /// One text replacement in the signed appcast (before it is signed).
    edit: Option<(String, String)>,
    /// The roster's `valid_until`.
    valid_until: &'static str,
    /// The channel head, when it is another release (`None`: the pinned one is the head).
    head: Option<Head>,
}

impl Shape {
    fn good() -> Self {
        let x86 = verifier(LinuxTarget::X86_64, false);
        let arm = verifier(LinuxTarget::Aarch64, false);
        Self {
            signed: [Some(x86.clone()), Some(arm.clone())],
            served: [Some(x86), Some(arm)],
            windows_signed: None,
            windows_served: None,
            signer: MACHINE,
            commit: commit(),
            build: 100,
            prerelease: false,
            archived: false,
            edit: None,
            valid_until: "9999-12-31T23:59:59Z",
            head: None,
        }
    }

    /// `good`, behind a head `v0.101.0` whose appcast sets the floor `min_build`, and
    /// carrying its own appcast as the archived pair, as a release behind the head does.
    fn behind(min_build: u64) -> Self {
        Self {
            archived: true,
            head: Some(Head {
                version: "0.101.0",
                build: 101,
                min_build,
            }),
            ..Self::good()
        }
    }

    fn appcast(&self) -> Vec<u8> {
        let mut text = cast("0.100.0", self.build, &self.commit, 0);
        let mut any = false;
        for (arch, bytes) in pin::ARCHES.iter().zip(&self.signed) {
            if let Some(bytes) = bytes {
                any = true;
                text.push_str(&format!(
                    "linux_verifier_{arch} = \"{}\"\nlinux_verifier_{arch}_sha256 = \
                     \"{}\"\nlinux_verifier_{arch}_size = {}\n",
                    pin::asset_name("0.100.0", arch),
                    digest(bytes),
                    bytes.len()
                ));
            }
        }
        if any {
            text.push_str(&format!("linux_verifier_interface = {}\n", pin::INTERFACE));
        }
        if let Some(bytes) = &self.windows_signed {
            text.push_str(&format!(
                "windows_verifier_x86_64 = \"{}\"\nwindows_verifier_x86_64_sha256 = \"{}\"\n\
                 windows_verifier_x86_64_size = {}\nwindows_verifier_interface = {}\n",
                pin::windows_asset_name("0.100.0", "x86_64"),
                digest(bytes),
                bytes.len(),
                pin::WINDOWS_INTERFACE
            ));
        }
        if let Some((from, to)) = &self.edit {
            assert!(text.contains(from), "the edit {from:?} applies to:\n{text}");
            text = text.replace(from, to);
        }
        text.into_bytes()
    }
}

/// A loopback GitHub serving `shape` as the channel's release (and head, unless the
/// shape names another), routed for this thread while the world lives.
struct World {
    _fake: FakeGithub,
    _routed: crate::github::FixtureGuard,
    root: aterm_tempfile::TempDir,
    scratch: aterm_tempfile::TempDir,
    script: String,
    /// The test's paper master public key.
    master: String,
}

fn world(shape: &Shape) -> World {
    let fake = FakeGithub::start();
    fake.add_repo(SLUG, RepoSpec::public());
    let id = fake.add_release(
        SLUG,
        ReleaseSpec::published(TAG, "2026-10-26T00:00:00Z").prerelease(shape.prerelease),
    );
    let appcast = shape.appcast();
    let name = if shape.archived {
        format!("aterm-appcast-{TAG}.toml")
    } else {
        APPCAST.to_string()
    };
    fake.add_asset(id, AssetSpec::uploaded(&name, &appcast));
    fake.add_asset(
        id,
        AssetSpec::uploaded(&format!("{name}.sig"), &sign(&shape.signer, &appcast)),
    );
    for (arch, bytes) in pin::ARCHES.iter().zip(&shape.served) {
        if let Some(bytes) = bytes {
            fake.add_asset(
                id,
                AssetSpec::uploaded(&pin::asset_name("0.100.0", arch), bytes),
            );
        }
    }
    if let Some(bytes) = &shape.windows_served {
        fake.add_asset(
            id,
            AssetSpec::uploaded(&pin::windows_asset_name("0.100.0", "x86_64"), bytes),
        );
    }
    let head = match &shape.head {
        None => id,
        Some(head) => {
            let tag = format!("v{}", head.version);
            let at = fake.add_release(SLUG, ReleaseSpec::published(&tag, "2026-10-27T00:00:00Z"));
            let policy = cast(head.version, head.build, &"d".repeat(40), head.min_build);
            fake.add_asset(at, AssetSpec::uploaded(APPCAST, policy.as_bytes()));
            fake.add_asset(
                at,
                AssetSpec::uploaded(
                    &format!("{APPCAST}.sig"),
                    &sign(&MACHINE, policy.as_bytes()),
                ),
            );
            at
        }
    };
    let roster = roster(shape.valid_until);
    fake.add_asset(head, AssetSpec::uploaded(ROSTER_ASSET, &roster));
    fake.add_asset(
        head,
        AssetSpec::uploaded(ROSTER_SIG_ASSET, &sign(&MASTER, &roster)),
    );
    fake.set_latest(SLUG, Some(head));
    let p = fake.ports();
    let routed = Fixture {
        origins: Origins::loopback(p.api, p.uploads, p.web, p.cdn),
        token: None,
        spawn: None,
    }
    .enter();
    let root = aterm_tempfile::Builder::new()
        .prefix("aterm-pin-root-")
        .tempdir()
        .unwrap();
    let script = format!(
        "#!/bin/bash\n# the installer\n{}rest\n",
        Pin::default().lines()
    );
    std::fs::create_dir_all(root.path().join("tools")).unwrap();
    std::fs::create_dir_all(root.path().join("crates/aterm-bootstrap-verify")).unwrap();
    std::fs::write(root.path().join("tools/install.sh"), &script).unwrap();
    std::fs::create_dir_all(root.path().join("apps/aterm-win")).unwrap();
    std::fs::write(root.path().join(pin::WINDOWS_SCRIPT), windows_script()).unwrap();
    std::fs::write(root.path().join("RELEASES.ledger"), LEDGER).unwrap();
    let scratch = aterm_tempfile::Builder::new()
        .prefix("aterm-pin-scratch-")
        .tempdir()
        .unwrap();
    World {
        _fake: fake,
        _routed: routed,
        root,
        scratch,
        script,
        master: pk(&MASTER),
    }
}

/// A checkout whose tag `v0.100.0` names `commit` (and has no other tag).
struct Tags(String);

impl GitRunner for Tags {
    fn git(&self, args: &[&str]) -> crate::ledger::Result<RunOut> {
        let wanted = format!("refs/tags/{TAG}^{{commit}}");
        Ok(
            if args == ["rev-parse", "--verify", "--quiet", wanted.as_str()] {
                RunOut {
                    status: 0,
                    stdout: format!("{}\n", self.0).into_bytes(),
                    stderr: Vec::new(),
                }
            } else {
                RunOut {
                    status: 1,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }
            },
        )
    }
}

impl World {
    /// The ask for `tag` under `masters`, downloading into this world's scratch.
    fn ask<'a>(&'a self, tag: &'a str, masters: &'a [&'a str]) -> Ask<'a> {
        Ask {
            slug: SLUG,
            tag,
            masters,
            now_unix: NOW,
            scratch: self.scratch.path(),
        }
    }

    /// `read_record`, then `write_pin` with what it read, as `pin_from_release` runs them.
    fn pin(&self, git: &dyn GitRunner, ask: &Ask<'_>) -> Result<Pin, String> {
        let record = read_record(self.root.path(), git, ask)?;
        super::super::write_pin(self.root.path(), &record)
    }

    /// [`Self::refused`] for `v0.100.0` under the test's paper master.
    fn refuses(&self, git: &dyn GitRunner, why: &str) {
        let masters = [self.master.as_str()];
        self.refused(git, &self.ask(TAG, &masters), why);
    }

    /// The refusal `why` (a fragment of its words), with nothing written into the checkout
    /// and no download left behind.
    fn refused(&self, git: &dyn GitRunner, ask: &Ask<'_>, why: &str) {
        let said = self.pin(git, ask).expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
        assert_eq!(
            std::fs::read_to_string(self.root.path().join("tools/install.sh")).unwrap(),
            self.script,
            "{why}: install.sh untouched"
        );
        assert!(
            !self.root.path().join(pin::RECORD).exists(),
            "{why}: no record"
        );
        assert_eq!(
            std::fs::read_dir(self.scratch.path()).unwrap().count(),
            0,
            "{why}: the download directory is gone"
        );
    }
}

/// `shape`'s release pins the architectures its appcast signs: install.sh's four lines
/// and the committed record, the record exactly the one the cut's sign step would have
/// written from the same receipts, and the pair passes `pin::check`.
fn pins_as_the_sign_step_would(shape: &Shape) {
    let w = world(shape);
    let masters = [w.master.as_str()];
    let ask = w.ask(TAG, &masters);
    let record = read_record(w.root.path(), &Tags(commit()), &ask).unwrap();
    let mut handoff = super::super::Handoff {
        directory: std::path::PathBuf::from("/"),
        targets: super::super::TARGETS
            .iter()
            .map(|t| t.triple().to_string())
            .collect(),
        artifacts: Vec::new(),
        verifiers: Vec::new(),
        workers: Vec::new(),
    };
    for (target, bytes) in super::super::TARGETS.iter().zip(&shape.signed) {
        let Some(bytes) = bytes else { continue };
        handoff.verifiers.push(super::super::VerifierArtifact {
            schema: 1,
            version: "0.100.0".into(),
            build_number: 100,
            commit: commit(),
            target: target.triple().into(),
            asset: pin::asset_name("0.100.0", target.arch()),
            sha256: digest(bytes),
            size: bytes.len() as u64,
            source_fingerprint: "b".repeat(64),
            compiler_version: format!("release: 1.99.1\nhost: {}\n", target.triple()),
        });
    }
    assert_eq!(
        Some(record.clone()),
        handoff.record(SLUG, "0.100.0", 100, &commit())
    );
    assert_eq!(
        std::fs::read_dir(w.scratch.path()).unwrap().count(),
        0,
        "the downloads are removed"
    );
    let pin = w.pin(&Tags(commit()), &ask).unwrap();
    assert_eq!(pin, record.pin());
    let signed = |i: usize| shape.signed[i].as_deref().map(digest).unwrap_or_default();
    assert_eq!(
        (pin.x86_64.clone(), pin.aarch64.clone()),
        (signed(0), signed(1))
    );
    let written = std::fs::read_to_string(w.root.path().join("tools/install.sh")).unwrap();
    let committed = std::fs::read_to_string(w.root.path().join(pin::RECORD)).unwrap();
    assert_eq!(pin::check(&written, Some(&committed), LEDGER), Ok(pin));
}

/// The head's signed verifiers pin both architectures as the sign step would.
#[test]
fn a_signed_release_pins_both_architectures_as_the_sign_step_would() {
    pins_as_the_sign_step_would(&Shape::good());
}

/// A release behind the head pins from its archived appcast, under the head's roster and
/// a floor at its build; one whose appcast signs a single architecture pins that one alone
/// (the other stays on install.sh's embedded verifier), as its record would.
#[test]
fn a_release_behind_the_head_or_with_one_architecture_pins_what_it_signs() {
    pins_as_the_sign_step_would(&Shape::behind(100));
    let mut shape = Shape::good();
    shape.signed[1] = None;
    shape.served[1] = None;
    pins_as_the_sign_step_would(&shape);
}

/// Negative controls: a release that lacks a signed architecture's verifier, or lists it
/// at another size than the appcast signs, writes nothing.
#[test]
fn a_release_missing_a_signed_verifier_is_refused() {
    let mut shape = Shape::good();
    shape.served[0] = None;
    world(&shape).refuses(&Tags(commit()), "lacks its x86_64 bootstrap verifier");
    let mut shape = Shape::good();
    let mut longer = verifier(LinuxTarget::Aarch64, false);
    longer.push(0);
    shape.served[1] = Some(longer);
    world(&shape).refuses(
        &Tags(commit()),
        "at 121 bytes, and its signed appcast says 120",
    );
}

/// Negative controls: served bytes that are not the signed digest, or a signed verifier
/// that is not a static executable, write nothing.
#[test]
fn a_served_verifier_that_is_not_the_signed_one_is_refused() {
    let mut shape = Shape::good();
    let mut tampered = verifier(LinuxTarget::Aarch64, false);
    tampered[100] ^= 1;
    shape.served[1] = Some(tampered);
    world(&shape).refuses(&Tags(commit()), "does not match");
    let mut shape = Shape::good();
    let dynamic = verifier(LinuxTarget::Aarch64, true);
    shape.signed[1] = Some(dynamic.clone());
    shape.served[1] = Some(dynamic);
    world(&shape).refuses(&Tags(commit()), "not a static executable");
}

/// Negative controls: a tag that is not a release tag, one RELEASES.ledger does not claim,
/// a signed build or commit that is not the claim's, and a tag this checkout lacks, write
/// nothing.
#[test]
fn a_release_whose_claim_this_checkout_does_not_hold_is_refused() {
    let w = world(&Shape::good());
    let masters = [w.master.as_str()];
    for bare in ["0.100.0", "v0.100", "v0.100.0-rc1"] {
        w.refused(
            &Tags(commit()),
            &w.ask(bare, &masters),
            "is not a release tag",
        );
    }
    std::fs::write(w.root.path().join("RELEASES.ledger"), "99 0.99.0\n").unwrap();
    w.refuses(&Tags(commit()), "records no claim for 0.100.0");
    let mut shape = Shape::good();
    shape.build = 101;
    world(&shape).refuses(&Tags(commit()), "not the claim this checkout holds");
    let w = world(&Shape::good());
    w.refuses(&Tags("c".repeat(40)), "not the claim this checkout holds");
    w.refuses(&Tags(String::new()), "not a full commit id");
    let masters = [w.master.as_str()];
    let older = w.ask("v0.99.0", &masters);
    w.refused(&Tags(commit()), &older, "has no tag v0.99.0");
}

/// Negative controls: an appcast that signs no verifier digest (every release before
/// this change), one signed by a machine off the roster, a roster this checkout's master
/// did not sign or whose window has lapsed, and a demoted (prerelease) release, write
/// nothing.
#[test]
fn a_release_whose_verifier_digests_are_not_signed_is_refused() {
    let mut shape = Shape::good();
    shape.signed = [None, None];
    world(&shape).refuses(&Tags(commit()), "names no bootstrap verifier");
    let mut shape = Shape::good();
    shape.signer = OTHER;
    world(&shape).refuses(&Tags(commit()), "no authorized live machine signature");
    let w = world(&Shape::good());
    let other = pk(&OTHER);
    let masters = [other.as_str()];
    let foreign = w.ask(TAG, &masters);
    w.refused(&Tags(commit()), &foreign, "does not verify under");
    let mut shape = Shape::good();
    shape.valid_until = "2026-01-01T00:00:00Z";
    world(&shape).refuses(&Tags(commit()), "roster is refused (Stale)");
    let mut shape = Shape::good();
    shape.prerelease = true;
    world(&shape).refuses(&Tags(commit()), "a draft or a prerelease");
}

/// Negative controls: a signed appcast that names another machine than its signer,
/// another channel's release as its origin, or an interface this tree's install.sh does
/// not call, writes nothing.
#[test]
fn a_signed_appcast_of_another_signer_channel_or_interface_is_refused() {
    for (edit, why) in [
        (
            ("machine_id = \"m3\"", "machine_id = \"m4\""),
            "attribution mismatch",
        ),
        (("roster_seq = 5", "roster_seq = 6"), "attribution mismatch"),
        (
            (
                "https://github.com/fixture-pin/aterm/",
                "https://github.com/fixture-pin/fork/",
            ),
            "signed release origin mismatch",
        ),
        (
            (
                "linux_verifier_interface = 1",
                "linux_verifier_interface = 2",
            ),
            "speaks interface 2",
        ),
    ] {
        let mut shape = Shape::good();
        shape.edit = Some((edit.0.to_string(), edit.1.to_string()));
        world(&shape).refuses(&Tags(commit()), why);
    }
}

/// Negative controls: a release newer than the head, one below the head's apply floor (a
/// yank raised it past the build), and a head whose appcast is only an archived pair,
/// write nothing.
#[test]
fn a_release_the_channel_head_does_not_serve_is_refused() {
    world(&Shape::behind(101)).refuses(&Tags(commit()), "below the apply floor 101");
    let mut shape = Shape::behind(0);
    shape.head = Some(Head {
        version: "0.99.0",
        build: 99,
        min_build: 0,
    });
    world(&shape).refuses(&Tags(commit()), "newer than the channel head v0.99.0");
    let mut shape = Shape::good();
    shape.archived = true;
    world(&shape).refuses(&Tags(commit()), "cannot supply the channel's policy");
}

/// get-aterm.ps1 as a checkout carries it before a pin: its three lines empty.
fn windows_script() -> String {
    format!(
        "# get-aterm.ps1\n{}rest\n",
        pin::WindowsPin::default().lines()
    )
}

/// A synthetic x86-64 console executable's header, standing in for the Windows verifier.
fn windows_verifier() -> Vec<u8> {
    let mut bytes = vec![0u8; 1024];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[0x94..0x96].copy_from_slice(&240u16.to_le_bytes());
    bytes[0x96..0x98].copy_from_slice(&0x0022u16.to_le_bytes());
    bytes[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
    bytes[0xdc..0xde].copy_from_slice(&3u16.to_le_bytes());
    bytes
}

/// `good`, whose appcast also signs, and whose release also serves, the Windows verifier.
fn windows_shape() -> Shape {
    let bytes = windows_verifier();
    Shape {
        windows_signed: Some(bytes.clone()),
        windows_served: Some(bytes),
        ..Shape::good()
    }
}

impl World {
    /// `read_windows_record`, then `write_windows_pin` with what it read, as
    /// `windows_pin_from_release` runs them.
    fn pin_windows(&self, git: &dyn GitRunner, ask: &Ask<'_>) -> Result<pin::WindowsPin, String> {
        let record = read_windows_record(self.root.path(), git, ask)?;
        crate::windows_pin::write_windows_pin(self.root.path(), &record)
    }

    /// The Windows refusal `why` for `v0.100.0`, with get-aterm.ps1 and both records untouched
    /// and no download left behind.
    fn refuses_windows(&self, git: &dyn GitRunner, why: &str) {
        let masters = [self.master.as_str()];
        let said = self
            .pin_windows(git, &self.ask(TAG, &masters))
            .expect_err(why);
        assert!(said.contains(why), "{why}: {said}");
        assert_eq!(
            std::fs::read_to_string(self.root.path().join(pin::WINDOWS_SCRIPT)).unwrap(),
            windows_script(),
            "{why}: get-aterm.ps1 untouched"
        );
        assert!(
            !self.root.path().join(pin::WINDOWS_RECORD).exists(),
            "{why}: no record"
        );
        assert_eq!(
            std::fs::read_dir(self.scratch.path()).unwrap().count(),
            0,
            "{why}: the download directory is gone"
        );
    }
}

/// A release whose appcast signs its Windows verifier pins get-aterm.ps1 to it, by the chain
/// install.sh's pin is read under, and touches nothing of install.sh's.
#[test]
fn a_signed_release_pins_its_windows_verifier() {
    let w = world(&windows_shape());
    let masters = [w.master.as_str()];
    let ask = w.ask(TAG, &masters);
    let bytes = windows_verifier();
    let record = read_windows_record(w.root.path(), &Tags(commit()), &ask).unwrap();
    assert_eq!(
        record,
        pin::WindowsRecord(pin::Record {
            repo: SLUG.into(),
            tag: TAG.into(),
            build_number: 100,
            commit: commit(),
            interface: pin::WINDOWS_INTERFACE,
            verifiers: vec![pin::Receipt {
                arch: "x86_64".into(),
                asset: pin::windows_asset_name("0.100.0", "x86_64"),
                sha256: digest(&bytes),
                size: bytes.len() as u64,
            }],
        })
    );
    assert_eq!(
        std::fs::read_dir(w.scratch.path()).unwrap().count(),
        0,
        "the downloads are removed"
    );
    let pinned = w.pin_windows(&Tags(commit()), &ask).unwrap();
    assert_eq!(pinned, record.pin());
    let written = std::fs::read_to_string(w.root.path().join(pin::WINDOWS_SCRIPT)).unwrap();
    let committed = std::fs::read_to_string(w.root.path().join(pin::WINDOWS_RECORD)).unwrap();
    assert_eq!(
        pin::check_windows(&written, Some(&committed), LEDGER),
        Ok(pinned)
    );
    assert_eq!(
        std::fs::read_to_string(w.root.path().join("tools/install.sh")).unwrap(),
        w.script,
        "install.sh's pin is untouched"
    );
}

/// NEGATIVE CONTROLS: a release whose appcast signs no Windows verifier (every release before
/// the fields), one that does not serve it, serves other bytes, signs a file that is no
/// Windows executable, or names an interface this tree's get-aterm.ps1 does not call, writes
/// nothing.
#[test]
fn a_windows_verifier_the_release_does_not_sign_and_serve_is_refused() {
    world(&Shape::good()).refuses_windows(&Tags(commit()), "names no Windows bootstrap verifier");
    let mut shape = windows_shape();
    shape.windows_served = None;
    world(&shape).refuses_windows(&Tags(commit()), "lacks its x86_64 bootstrap verifier asset");
    let mut shape = windows_shape();
    let mut tampered = windows_verifier();
    tampered[600] ^= 1;
    shape.windows_served = Some(tampered);
    world(&shape).refuses_windows(&Tags(commit()), "does not match");
    let elf = verifier(LinuxTarget::X86_64, false);
    let shape = Shape {
        windows_signed: Some(elf.clone()),
        windows_served: Some(elf),
        ..Shape::good()
    };
    world(&shape).refuses_windows(&Tags(commit()), "no MZ header");
    let mut shape = windows_shape();
    let (now, older) = (pin::WINDOWS_INTERFACE, pin::WINDOWS_INTERFACE - 1);
    shape.edit = Some((
        format!("windows_verifier_interface = {now}"),
        format!("windows_verifier_interface = {older}"),
    ));
    world(&shape).refuses_windows(&Tags(commit()), &format!("speaks interface {older}"));
}

/// `ship pin-verifier` takes a release tag after `--from-release`, or one record path,
/// and nothing else.
#[test]
fn pin_verifier_takes_a_release_or_a_record_and_nothing_else() {
    use crate::cli::{Cmd, PinSource};
    let parse = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        crate::cli::parse(&args)
    };
    assert_eq!(
        parse(&["pin-verifier", "--from-release", "v0.145.0"]),
        Ok(Cmd::PinVerifier(PinSource::Release("v0.145.0".into())))
    );
    assert_eq!(
        parse(&["pin-verifier", "--windows", "--from-release", "v0.145.0"]),
        Ok(Cmd::PinWindowsVerifier("v0.145.0".into()))
    );
    assert_eq!(
        parse(&["pin-verifier", "dist/aterm-bootstrap-verify-pin.toml"]),
        Ok(Cmd::PinVerifier(PinSource::Record(
            "dist/aterm-bootstrap-verify-pin.toml".into()
        )))
    );
    for bad in [
        &["pin-verifier"][..],
        &["pin-verifier", "--from-release"],
        &["pin-verifier", "--from-release", "--dry-run"],
        &["pin-verifier", "--from-release", "v0.145.0", "extra"],
        &["pin-verifier", "dist/a.toml", "dist/b.toml"],
        &["pin-verifier", "--record", "dist/a.toml"],
        &["pin-verifier", "--windows"],
        &["pin-verifier", "--windows", "dist/a.toml"],
        &["pin-verifier", "--windows", "--from-release"],
        &["pin-verifier", "--windows", "--from-release", "--dry-run"],
        &[
            "pin-verifier",
            "--windows",
            "--from-release",
            "v0.145.0",
            "extra",
        ],
        &["pin-verifier", "--from-release", "v0.145.0", "--windows"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}
