// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

use ring::signature::{Ed25519KeyPair, KeyPair as _};

const TAG: &str = "v0.100.0";
const SLUG: &str = "example/aterm";
/// A newer release than [`TAG`], which the walk-back tests make the latest.
const LATEST: &str = "v0.101.0";
/// 2027-01-15, before the roster's deadline.
const NOW: i64 = 1_800_000_000;
/// The build [`TAG`]'s appcast signs.
const BUILD: u64 = 1_800_000_000;

/// The synthetic seeds of crates/aterm-update/tests/install_sh/chain.rs: NOT production keys.
fn keypair(seed: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("a synthetic seed")
}

fn b64(bytes: &[u8]) -> String {
    aterm_codec::base64::encode(bytes).expect("base64")
}

fn sha256(bytes: &[u8]) -> String {
    aterm_codec::hex::encode_lower(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

const MASTER: u8 = 0x17;
const MACHINE: u8 = 0x29;
const OTHER: u8 = 0x3b;
const THIEF: u8 = 0x4d;

/// The zip a release publishes: its bytes are only ever hashed here.
fn zip_bytes() -> Vec<u8> {
    (0..4096u32).map(|n| (n % 251) as u8).collect()
}

/// A roster of sequence `seq` naming `machines` (each an id and its key's seed) and revoking
/// `revoked`.
fn roster(seq: u64, machines: &[(&str, u8)], revoked: &[&str]) -> String {
    let mut text =
        format!("schema = 1\nroster_seq = {seq}\nvalid_until = \"2099-01-01T00:00:00Z\"\n");
    if !revoked.is_empty() {
        let quoted: Vec<String> = revoked.iter().map(|id| format!("{id:?}")).collect();
        text.push_str(&format!("revoked = [{}]\n", quoted.join(", ")));
    }
    for (id, seed) in machines {
        text.push_str(&format!(
            "\n[[machine]]\nid = \"{id}\"\npubkey = \"{}\"\n",
            b64(keypair(*seed).public_key().as_ref())
        ));
    }
    text
}

/// An appcast's keys in order: release `version` of [`SLUG`], build `build`, signed by
/// `machine` under roster `roster_seq`, with the Windows triple of [`zip_bytes`] when `windows`.
fn cast(
    version: &str,
    build: u64,
    machine: &str,
    roster_seq: u64,
    windows: bool,
) -> Vec<(String, String)> {
    let zip = zip_bytes();
    let row = |k: &str, v: String| (k.to_string(), v);
    let mut rows = vec![
        row("schema", "1".into()),
        row("version", format!("{version:?}")),
        row("build_number", build.to_string()),
        row("commit", format!("{:?}", "a".repeat(40))),
        row("sha256", format!("{:?}", "d".repeat(64))),
        row("dmg", format!("\"aterm-{version}.dmg\"")),
        row(
            "url",
            format!(
                "\"https://github.com/{SLUG}/releases/download/v{version}/aterm-{version}.dmg\""
            ),
        ),
        row("machine_id", format!("{machine:?}")),
        row("roster_seq", roster_seq.to_string()),
    ];
    if windows {
        rows.extend([
            row(
                "windows_x86_64",
                format!("\"aterm-{version}-windows-x86_64.zip\""),
            ),
            row("windows_x86_64_sha256", format!("{:?}", sha256(&zip))),
            row("windows_x86_64_size", zip.len().to_string()),
        ]);
    }
    rows
}

/// `rows` with `key` set to `value` (a TOML literal), or removed when `None`.
fn with(mut rows: Vec<(String, String)>, key: &str, value: Option<&str>) -> Vec<(String, String)> {
    let at = rows.iter().position(|(k, _)| k == key);
    match (at, value) {
        (Some(n), Some(v)) => rows[n].1 = v.to_string(),
        (Some(n), None) => {
            rows.remove(n);
        }
        (None, Some(v)) => rows.push((key.to_string(), v.to_string())),
        (None, None) => {}
    }
    rows
}

/// `text` written to `dir` as `name`, and `by`'s signature of it as `name.sig`.
fn sign_into(dir: &std::path::Path, name: &str, text: &str, by: u8) {
    std::fs::write(dir.join(name), text).expect("a proof");
    let signature = keypair(by).sign(text.as_bytes());
    std::fs::write(dir.join(format!("{name}.sig")), signature.as_ref()).expect("a signature");
}

fn toml(rows: &[(String, String)]) -> String {
    rows.iter().map(|(k, v)| format!("{k} = {v}\n")).collect()
}

/// One release as published: who signs each proof, and the appcast's keys in order.
struct Release {
    roster_by: u8,
    appcast_by: u8,
    appcast: Vec<(String, String)>,
}

impl Release {
    fn good() -> Self {
        Self {
            roster_by: MASTER,
            appcast_by: MACHINE,
            appcast: cast("0.100.0", BUILD, "synthetic", 3, true),
        }
    }

    /// The appcast with `key` set to `value` (a TOML literal), or removed when `None`.
    fn with(mut self, key: &str, value: Option<&str>) -> Self {
        self.appcast = with(self.appcast, key, value);
        self
    }

    /// The four proofs in a new folder.
    fn lay(&self) -> aterm_tempfile::TempDir {
        let dir = aterm_tempfile::tempdir().expect("a scratch folder");
        sign_into(
            dir.path(),
            "aterm-machines.toml",
            &roster(3, &[("synthetic", MACHINE)], &[]),
            self.roster_by,
        );
        sign_into(
            dir.path(),
            "aterm-appcast.toml",
            &toml(&self.appcast),
            self.appcast_by,
        );
        dir
    }
}

fn pins() -> Vec<String> {
    vec![b64(keypair(MASTER).public_key().as_ref())]
}

/// The command line's answer over `dir` for release `tag` under latest release `latest`, asked
/// for `arch`, with `zip` (its bytes) laid beside the proofs as the download when given.
fn ask_in(
    dir: &std::path::Path,
    latest: &str,
    tag: &str,
    arch: &str,
    zip: Option<&[u8]>,
) -> Verdict<Option<String>> {
    let mut args: Vec<OsString> = vec![
        dir.as_os_str().to_os_string(),
        latest.into(),
        tag.into(),
        SLUG.into(),
        arch.into(),
    ];
    if let Some(bytes) = zip {
        let at = dir.join("candidate.zip");
        std::fs::write(&at, bytes).expect("the candidate");
        args.push(at.into_os_string());
    }
    main(&args, NOW, &pins())
}

/// The command line's answer for `release`, the latest, asked for `arch`.
fn answer(release: &Release, arch: &str, zip: Option<&[u8]>) -> Verdict<Option<String>> {
    let dir = release.lay();
    ask_in(dir.path(), TAG, TAG, arch, zip)
}

fn refusal(said: Verdict<Option<String>>) -> String {
    said.expect_err("a refusal").0
}

/// The line the verifier prints for [`TAG`]'s zip under a roster of sequence `roster_seq`.
fn tag_line(roster_seq: u64) -> String {
    let zip = zip_bytes();
    format!(
        "aterm-0.100.0-windows-x86_64.zip\t{}\t{}\t{BUILD}\t{roster_seq}",
        sha256(&zip),
        zip.len()
    )
}

#[test]
fn a_signed_release_names_its_zip_and_holds_the_download_to_it() {
    let zip = zip_bytes();
    assert_eq!(
        answer(&Release::good(), "x86_64", None),
        Ok(Some(tag_line(3)))
    );
    assert_eq!(
        answer(&Release::good(), "x86_64", Some(&zip)),
        Ok(Some(tag_line(3)))
    );
    assert_eq!(
        asset_name("0.100.0", "x86_64"),
        "aterm-0.100.0-windows-x86_64.zip"
    );
}

/// The Windows ask is the chain's: a proof no pinned or rostered key signed is refused before
/// any Windows field is read.
#[test]
fn the_windows_ask_holds_every_rule_of_the_chain() {
    let thief = Release {
        roster_by: THIEF,
        ..Release::good()
    };
    assert_eq!(
        refusal(answer(&thief, "x86_64", None)),
        "roster is not signed by the pinned paper master"
    );
    let unrostered = Release {
        appcast_by: OTHER,
        ..Release::good()
    };
    assert_eq!(
        refusal(answer(&unrostered, "x86_64", None)),
        "appcast has no authorized live machine signature"
    );
    for (key, value, why) in [
        (
            "version",
            Some("\"0.101.0\""),
            "release tag/appcast version mismatch",
        ),
        (
            "url",
            Some("\"https://github.com/other/aterm/releases/download/v0.100.0/aterm-0.100.0.dmg\""),
            "signed release origin mismatch",
        ),
        ("commit", None, "invalid source commit"),
        ("schema", Some("2"), "newer manifest schema"),
        (
            "machine_id",
            Some("\"other\""),
            "appcast signer attribution mismatch",
        ),
        (
            "linux_x86_64",
            Some("\"aterm-0.100.0-linux-x86_64\""),
            "incomplete Linux artifact triple",
        ),
    ] {
        let release = Release::good().with(key, value);
        assert_eq!(refusal(answer(&release, "x86_64", None)), why, "{key}");
    }
}

#[test]
fn the_windows_triple_is_whole_and_canonical() {
    let zip = zip_bytes();
    for (key, value, why) in [
        (
            "windows_x86_64_size",
            None,
            "incomplete Windows artifact triple",
        ),
        ("windows_x86_64", None, "incomplete Windows artifact triple"),
        (
            "windows_x86_64",
            Some("\"aterm-0.99.0-windows-x86_64.zip\""),
            "noncanonical Windows asset",
        ),
        (
            "windows_x86_64",
            Some("\"aterm-0.100.0-windows-x86_64-portable.zip\""),
            "noncanonical Windows asset",
        ),
        (
            "windows_x86_64_sha256",
            Some(&format!("{:?}", sha256(&zip).to_uppercase())),
            "invalid Windows digest",
        ),
        ("windows_x86_64_sha256", Some("7"), "invalid Windows digest"),
        (
            "windows_x86_64_size",
            Some("63"),
            "Windows artifact too small",
        ),
        (
            "windows_x86_64_size",
            Some(&(MAX_ZIP + 1).to_string()),
            "invalid Windows size",
        ),
        (
            "windows_x86_64_size",
            Some("\"4096\""),
            "invalid Windows size",
        ),
    ] {
        let release = Release::good().with(key, value);
        assert_eq!(
            refusal(answer(&release, "x86_64", None)),
            why,
            "{key} = {value:?}"
        );
    }
    assert_eq!(
        refusal(answer(&Release::good(), "aarch64", None)),
        "unsupported Windows architecture"
    );
}

/// A latest release whose chain holds and whose appcast signs no zip is answered [`NO_ZIP`],
/// which get-aterm.ps1 walks back on; with a download to hold, it is refused, and a latest
/// release the chain refuses is refused whatever it carries.
#[test]
fn a_latest_release_with_no_zip_is_answered_none() {
    let mac_only = Release::good()
        .with("windows_x86_64", None)
        .with("windows_x86_64_sha256", None)
        .with("windows_x86_64_size", None);
    assert_eq!(
        answer(&mac_only, "x86_64", None),
        Ok(Some(NO_ZIP.to_string()))
    );
    assert_eq!(
        refusal(answer(&mac_only, "x86_64", Some(&zip_bytes()))),
        "signed release has no Windows artifact for x86_64"
    );
    let stolen = Release {
        appcast_by: OTHER,
        ..mac_only
    };
    assert_eq!(
        refusal(answer(&stolen, "x86_64", None)),
        "appcast has no authorized live machine signature"
    );
}

/// NEGATIVE CONTROL for the candidate: one byte changed, one byte more, and a folder in the
/// file's place are each refused; the release's own bytes pass (above).
#[test]
fn a_download_that_is_not_the_signed_zip_is_refused() {
    let zip = zip_bytes();
    let mut flipped = zip.clone();
    flipped[100] ^= 1;
    assert_eq!(
        refusal(answer(&Release::good(), "x86_64", Some(&flipped))),
        "Windows artifact SHA-256 mismatch"
    );
    let mut longer = zip.clone();
    longer.push(0);
    assert_eq!(
        refusal(answer(&Release::good(), "x86_64", Some(&longer))),
        "Windows artifact size mismatch"
    );
    let folder = aterm_tempfile::tempdir().expect("a folder");
    let digest = sha256(&zip);
    assert!(verify_zip(folder.path().as_os_str(), &digest, 4096).is_err());
    assert!(verify_zip(folder.path().join("absent.zip").as_os_str(), &digest, 4096).is_err());
}

#[test]
fn the_command_line_takes_five_or_six_words() {
    for count in [0, 4, 7] {
        let args: Vec<OsString> = (0..count).map(|n| OsString::from(n.to_string())).collect();
        assert_eq!(
            refusal(main(&args, NOW, &pins())),
            "invalid Windows verifier invocation"
        );
    }
}

/// A walk back as get-aterm.ps1 lays it: the latest release's roster and appcast, and an older
/// release's appcast.
struct Walk {
    /// The latest release's roster.
    roster: String,
    /// The latest release's appcast and who signs it.
    latest: Vec<(String, String)>,
    latest_by: u8,
    /// The proposed release's appcast and who signs it.
    older: Vec<(String, String)>,
    older_by: u8,
    /// Its tag.
    tag: &'static str,
}

impl Walk {
    /// A Mac-only latest [`LATEST`] under roster 5, which names the machine that signed
    /// [`TAG`] under its own roster 3, and [`TAG`] carrying the zip.
    fn good() -> Self {
        Self {
            roster: roster(5, &[("synthetic", MACHINE), ("older", OTHER)], &[]),
            latest: cast("0.101.0", BUILD + 10, "synthetic", 5, false),
            latest_by: MACHINE,
            older: cast("0.100.0", BUILD, "older", 3, true),
            older_by: OTHER,
            tag: TAG,
        }
    }

    fn lay(&self) -> aterm_tempfile::TempDir {
        let dir = aterm_tempfile::tempdir().expect("a scratch folder");
        sign_into(dir.path(), "aterm-machines.toml", &self.roster, MASTER);
        sign_into(
            dir.path(),
            LATEST_APPCAST,
            &toml(&self.latest),
            self.latest_by,
        );
        sign_into(
            dir.path(),
            "aterm-appcast.toml",
            &toml(&self.older),
            self.older_by,
        );
        dir
    }

    fn answer(&self, zip: Option<&[u8]>) -> Verdict<Option<String>> {
        let dir = self.lay();
        ask_in(dir.path(), LATEST, self.tag, "x86_64", zip)
    }
}

/// What a refusal of the proposed release says.
fn of_tag(why: &str) -> String {
    format!("release {TAG}, proposed in place of the latest release {LATEST}, is refused: {why}")
}

/// An older release with the zip, proposed under a Mac-only latest, is held to the latest
/// release's roster (its sequence, 5, is the line's) and the download to its signed zip. Its own
/// roster is never asked for: the folder holds only the latest release's.
#[test]
fn an_older_release_is_admitted_under_the_latest_release_chain() {
    let zip = zip_bytes();
    let walk = Walk::good();
    assert_eq!(walk.answer(None), Ok(Some(tag_line(5))));
    assert_eq!(walk.answer(Some(&zip)), Ok(Some(tag_line(5))));
    let mut flipped = zip;
    flipped[7] ^= 1;
    assert_eq!(
        refusal(walk.answer(Some(&flipped))),
        "Windows artifact SHA-256 mismatch"
    );
    // At the latest release's minimum build exactly, and with a minimum build of its own that
    // it meets, it is still admitted.
    let at_floor = Walk {
        latest: with(Walk::good().latest, "min_build", Some(&BUILD.to_string())),
        older: with(Walk::good().older, "min_build", Some(&BUILD.to_string())),
        ..Walk::good()
    };
    assert_eq!(at_floor.answer(None), Ok(Some(tag_line(5))));
}

/// NEGATIVE CONTROL for the build floor: the latest release's signed minimum build one above the
/// older release's build refuses it, and so does its own minimum build above its build.
#[test]
fn an_older_release_below_the_latest_minimum_build_is_refused() {
    let walk = Walk {
        latest: with(
            Walk::good().latest,
            "min_build",
            Some(&(BUILD + 1).to_string()),
        ),
        ..Walk::good()
    };
    assert_eq!(
        refusal(walk.answer(None)),
        of_tag("release build rollback or below apply floor")
    );
    let walk = Walk {
        older: with(
            Walk::good().older,
            "min_build",
            Some(&(BUILD + 1).to_string()),
        ),
        ..Walk::good()
    };
    assert_eq!(
        refusal(walk.answer(None)),
        of_tag("release build rollback or below apply floor")
    );
}

/// NEGATIVE CONTROL for revocation: the machine that signed the older release, revoked by the
/// latest release's roster, signs nothing it admits; one the roster does not name is no signer
/// either, though the older release's own roster may have named it.
#[test]
fn an_older_release_signed_by_a_revoked_machine_is_refused() {
    let revoked = Walk {
        roster: roster(5, &[("synthetic", MACHINE), ("older", OTHER)], &["older"]),
        ..Walk::good()
    };
    assert_eq!(
        refusal(revoked.answer(None)),
        of_tag("appcast has no authorized live machine signature")
    );
    let dropped = Walk {
        roster: roster(5, &[("synthetic", MACHINE)], &[]),
        ..Walk::good()
    };
    assert_eq!(
        refusal(dropped.answer(None)),
        of_tag("appcast has no authorized live machine signature")
    );
}

/// The older release must sign the zip, be the release of its tag and be older than the latest,
/// and its appcast may name no roster newer than the latest release's.
#[test]
fn an_older_release_must_be_an_older_windows_release_of_its_tag() {
    for (older, tag, why) in [
        (
            cast("0.100.0", BUILD, "older", 3, false),
            TAG,
            "signed release has no Windows artifact for x86_64",
        ),
        (
            cast("0.99.0", BUILD, "older", 3, true),
            TAG,
            "release tag/appcast version mismatch",
        ),
        (
            cast("0.100.0", BUILD, "older", 6, true),
            TAG,
            "appcast requires newer roster",
        ),
    ] {
        let walk = Walk {
            older,
            tag,
            ..Walk::good()
        };
        assert_eq!(refusal(walk.answer(None)), of_tag(why));
    }
    for tag in ["v0.102.0", "v0.101.1", "v0.100", "v0.100.00"] {
        let walk = Walk {
            tag,
            ..Walk::good()
        };
        assert_eq!(
            refusal(walk.answer(None)),
            format!(
                "release {tag}, proposed in place of the latest release {LATEST}, is refused: \
                 it is not a release older than the latest"
            )
        );
    }
}

/// The latest release is verified first and binds: refused, or carrying the zip itself, nothing
/// older is admitted in its place; and its appcast must be beside the older one's, and only then.
#[test]
fn the_latest_release_is_verified_first_and_must_carry_no_zip() {
    let carries = Walk {
        latest: cast("0.101.0", BUILD + 10, "synthetic", 5, true),
        ..Walk::good()
    };
    assert_eq!(
        refusal(carries.answer(None)),
        format!(
            "the latest release {LATEST} signs a Windows zip for x86_64: it is the release to \
             install, not {TAG}"
        )
    );
    let stolen = Walk {
        latest_by: THIEF,
        ..Walk::good()
    };
    assert_eq!(
        refusal(stolen.answer(None)),
        format!(
            "the latest release {LATEST} is refused: appcast has no authorized live machine \
             signature"
        )
    );
    let mislabelled = Walk {
        latest: cast("0.102.0", BUILD + 10, "synthetic", 5, false),
        ..Walk::good()
    };
    assert_eq!(
        refusal(mislabelled.answer(None)),
        format!("the latest release {LATEST} is refused: release tag/appcast version mismatch")
    );
    // The latest appcast missing: nothing to hold the older release to.
    let dir = Walk::good().lay();
    std::fs::remove_file(dir.path().join(LATEST_APPCAST)).expect("removed");
    assert!(
        refusal(ask_in(dir.path(), LATEST, TAG, "x86_64", None))
            .starts_with(&format!("the latest release {LATEST} is refused: ")),
    );
    // Asked about the latest alone, a latest-release appcast beside it is refused.
    let dir = Release::good().lay();
    sign_into(
        dir.path(),
        LATEST_APPCAST,
        &toml(&Release::good().appcast),
        MACHINE,
    );
    assert_eq!(
        refusal(ask_in(dir.path(), TAG, TAG, "x86_64", None)),
        "a latest-release appcast lies beside the release's own, which is the latest"
    );
}

/// install.sh's Linux ask reads no Windows field, as its embedded Python verifier reads none:
/// a release whose Windows triple a Windows ask refuses still verifies for Linux.
#[test]
fn a_linux_ask_reads_no_windows_field() {
    let release = Release::good().with("windows_x86_64_size", None);
    let dir = release.lay();
    let ask = |arch| Ask {
        directory: dir.path().as_os_str(),
        tag: TAG,
        slug: SLUG,
        arch,
        manifest_name: "aterm-appcast.toml",
        now: NOW,
        pins: &[],
    };
    let pins = pins();
    let linux = Ask {
        pins: &pins,
        ..ask(None)
    };
    assert!(verify_manifest(&linux, &Policy::default()).is_ok());
    let windows = Ask {
        pins: &pins,
        ..ask(Some("x86_64"))
    };
    assert_eq!(
        verify_windows_manifest(&windows, &Policy::default())
            .expect_err("refused")
            .0,
        "incomplete Windows artifact triple"
    );
}
